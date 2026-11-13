// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A member lets its own Commons writer successor into the vault with nobody
//! (`docs/DESIGN-shared-files-2026-11-11.md` section 13, decision 1; ATERM-KI-1109-1).
//!
//! A Mac whose Commons writer lost a table's directory binding fences that writer and mints a
//! successor device in a private home of its own (`aterm_db::generation`). The successor is a
//! new seat: its own age identity, its own writer key, no fleet node key. Since no plain ask is
//! admitted without a person (`c46efe588c3`), the fenced original, which is still a current
//! member, signs for it itself: an [`Allowed`] naming the successor's binding (the statement),
//! then, here, the `Admit` of [`Evidence::Manual`] and the `Wrap`s a member's desk signs for any
//! Mac it lets in ([`super::readmit`]). Every member's evaluator seats the successor on that
//! `Admit` as it seats any member's (`docs/DESIGN-fluid-swarm-2026-10-28.md` F15, "any member
//! admits"), so only the Mac that recovers needs this build, and no record kind is new.
//!
//! What keeps it from doing harm ([`judge`]):
//!
//! - The statement must carry this member's own device signature and name exactly the key that
//!   asks; a statement another key signed, or one for another key, signs nothing.
//! - A member the log removed, left out of `[everyone]` or seats nowhere signs nothing
//!   (`log_entries::open_local_member_signer_held`), and every other member's evaluator voids a
//!   removed seat's `Admit` past its cutoff.
//! - A successor the log removed, left out on purpose, or whose key another seat binds stays out
//!   ([`kept_out`], [`roster_keeps_out`]), and a lab identity never joins the live ALab (A12).
//! - A new seat is signed only once this member read the vault's newest records just now, so one
//!   that missed a removal of that key restores nothing ([`super::readmit::FRESH_WITHIN`]).
//! - The record budgets hold as for a proof: nothing is signed once either is
//!   [`super::readmit::PROVEN_FILL`] full, nor a batch that would pass a bound of the log. The
//!   hourly and daily shares of proofs are not taken: they bound what one holder of the fleet's
//!   key can ask of many members, and a member has one successor at a time.
//! - Nothing is kept unless the evaluator, over what was signed, seats the successor and still
//!   seats this member: the original stays a member, as every read through its successor needs.
//! - A member names one successor key, ever: once the log holds its admission of one, in or
//!   out, a statement for another key signs nothing, whatever this Mac still holds of its fence
//!   (a home restored from a backup holds none of it), and a person decides.
//!
//! The successor's seat is the same Mac's, on the same disk, under an age identity of its own.
//! The log ties the two ([`Lineages`]): the `Admit` carries the name the log gave its signer's
//! own seat when it came in, no fleet node key and [`Evidence::Manual`]. A person's removal of
//! the Mac takes every such seat out with it, down the lineage (`super::remove`), so no epoch
//! minted after a removal is wrapped to a key on the removed Mac's disk.

use super::admit::{
    HandedVault, LogAdmission, held_audiences, held_log, kept_out, labels_taken, log_bundle,
    roster_keeps_out, wraps_lacking,
};
use super::readmit::{FRESH_WITHIN, NOT_FRESH, ProofAdmissionCapacity, post_aside, read_newest_by};
use super::*;
use crate::commons::binding::Request;
use crate::members::Allowed;
use crate::members::bundle::Reader;
use crate::membership_log::{
    Body, Budget, Entrant, EpochAudience, Evidence, Kind, Log, MemberId, Record, RecordCounts,
    Signer, encode_records,
};
use std::collections::{BTreeMap, BTreeSet};

/// The successor a member admits ([`admit_successor`]).
#[derive(Clone, Copy, Debug)]
pub struct Succeeding<'a> {
    /// The successor's self-signed device request.
    pub request: &'a Request,
    /// The member's statement: its [`Allowed`] of that request's binding, signed with the
    /// member's own device key.
    pub statement: &'a Allowed,
    /// What the successor's home takes of a membership log.
    pub reader: Reader,
    /// The successor's vault identity is a lab identity (A12).
    pub lab: bool,
}

/// What [`admit_successor`] came to.
#[derive(Debug)]
pub enum Succession {
    /// The log seats the successor.
    Let {
        /// The log bundle the successor's home writes its vault from
        /// ([`super::readmit_from_log_bundle`]).
        bundle: SecretBytes,
        /// The label the log gives the successor's seat.
        label: String,
        /// How many records this member signed: none where the log seated it already and it
        /// lacked no `Wrap`.
        signed: usize,
    },
    /// The statement admits nothing here: why.
    Refused(String),
    /// A vault pass holds this Mac's state; the next recovery pass tries again.
    Busy,
    /// This member admits nobody through the log: no trusted log with epochs, no seat for its
    /// device key in it, or an `[everyone]` epoch it cannot hand on.
    NotHere,
}

/// The writer successions a membership log holds: which seats are a member's own successors.
///
/// A seat is a member's successor when an `Admit` of it that the member signed carries
/// [`Evidence::Manual`], no fleet node key, and a name the log gave the member's own seat when
/// it came in (its founder entry's, or that of an `Admit` of it): what [`admit_successor`]
/// signs. Every other admission differs in one of the three: a Mac a person allowed or that
/// proved itself comes in under its own name, and a seat with a fleet node key is a Mac of the
/// fleet. A seat's `Name` records are not read: its holder signs those, and the tie reads only
/// what was signed about each seat as it came in, which no later record changes.
pub(super) struct Lineages {
    /// Each successor, under the member that admitted it.
    links: Vec<(MemberId, MemberId)>,
}

impl Lineages {
    /// The successions `log` holds, whether or not their records still count: a removed
    /// member's admission of its successor names that successor as much as a current one's.
    pub(super) fn of(log: &Log) -> Self {
        let mut names: BTreeMap<MemberId, BTreeSet<String>> = BTreeMap::new();
        if let Body::Genesis { founders, .. } = log.genesis().body() {
            for founder in founders {
                if let Some(member) = founder.member() {
                    names
                        .entry(member)
                        .or_default()
                        .insert(founder.name.to_ascii_lowercase());
                }
            }
        }
        let mut admits = Vec::new();
        for record in log.records() {
            let Body::Admit { entrant, evidence } = record.body() else {
                continue;
            };
            let name = entrant.name.to_ascii_lowercase();
            if *evidence == Evidence::Manual && entrant.node_key == [0; 32] {
                admits.push((*record.actor(), entrant.member(), name.clone()));
            }
            names.entry(entrant.member()).or_default().insert(name);
        }
        let links = admits
            .into_iter()
            .filter(|(by, successor, name)| {
                by != successor && names.get(by).is_some_and(|own| own.contains(name))
            })
            .map(|(by, successor, _)| (by, successor))
            .collect();
        Self { links }
    }

    /// The successors `member` itself admitted.
    pub(super) fn of_member<'a>(
        &'a self,
        member: &'a MemberId,
    ) -> impl Iterator<Item = &'a MemberId> {
        self.links
            .iter()
            .filter(move |(by, _)| by == member)
            .map(|(_, successor)| successor)
    }

    /// Every seat below one of `seats` in a succession: each one's successors, theirs, and so
    /// on. A seat of `seats` is among them only where another of `seats` leads to it.
    pub(super) fn under(&self, seats: &BTreeSet<MemberId>) -> BTreeSet<MemberId> {
        let mut below = BTreeSet::new();
        let mut frontier: Vec<MemberId> = seats.iter().copied().collect();
        while let Some(seat) = frontier.pop() {
            for successor in self.of_member(&seat) {
                if below.insert(*successor) {
                    frontier.push(*successor);
                }
            }
        }
        below
    }
}

/// The name the record that let the member `me` in gave its seat: the name its successor is
/// admitted under ([`Lineages`]).
fn entry_name(log: &Log, me: &MemberId) -> Option<String> {
    let entry = log.membership_ref().get(me)?.entry;
    if entry == *log.genesis().id() {
        let Body::Genesis { founders, .. } = log.genesis().body() else {
            return None;
        };
        return founders
            .iter()
            .find(|founder| founder.member() == Some(*me))
            .map(|founder| founder.name.clone());
    }
    match log.get(&entry)?.body() {
        Body::Admit { entrant, .. } => Some(entrant.name.clone()),
        _ => None,
    }
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static SECOND_SUCCESSOR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// MUTANT of [`admit_successor`]'s rule, on for this thread while held: a member that the log
/// shows admitted one successor key signs for another, as before the log was asked. Public
/// under `debug_assertions` for the conformance test of `aterm_db::generation`, which builds
/// this crate without `cfg(test)`.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub struct SecondSuccessor(bool);

#[cfg(any(test, debug_assertions))]
impl SecondSuccessor {
    /// Turn the mutant on until the guard drops.
    #[must_use = "the rule is put back when the guard drops"]
    pub fn allowed() -> Self {
        Self(SECOND_SUCCESSOR.replace(true))
    }
}

#[cfg(any(test, debug_assertions))]
impl Drop for SecondSuccessor {
    fn drop(&mut self) {
        SECOND_SUCCESSOR.set(self.0);
    }
}

/// Whether a member names one successor key, ever: always in a release build, which has no
/// switch, and elsewhere unless `SecondSuccessor` is held on this thread.
fn one_successor() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        !SECOND_SUCCESSOR.get()
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        true
    }
}

/// What the log says of a member's statement for its successor, before anything is signed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Standing {
    /// The log seats the successor already: only the `Wrap`s it lacks.
    Already,
    /// A new seat.
    New,
}

/// What the log says the member `me` may do with `succeeding` at `now_ms` (its clock), where its
/// admission signs `wraps` `Wrap`s beside the `Admit`: the rule in the module's head.
fn judge(
    log: &Log,
    me: &MemberId,
    succeeding: &Succeeding<'_>,
    wraps: usize,
    now_ms: u64,
) -> Result<Standing, String> {
    if now_ms == 0 {
        return Err("this member's clock is not set".to_owned());
    }
    let binding = succeeding.request.binding();
    if succeeding.statement.signer()[..] != me[..32] {
        return Err(
            "the statement is not signed with this member's device key, and a member admits only \
             its own successor"
                .to_owned(),
        );
    }
    if succeeding.statement.binding() != binding {
        return Err("the statement names another key than the one that asks".to_owned());
    }
    if binding.actor() == *me || binding.writer()[..] == me[..32] {
        return Err("a writer's successor is a new actor with a device key of its own".to_owned());
    }
    if let Some(why) = kept_out(log, binding, false) {
        return Err(why);
    }
    if log.membership().contains(&binding.actor()) {
        return Ok(Standing::Already);
    }
    if one_successor()
        && Lineages::of(log)
            .of_member(me)
            .any(|successor| *successor != binding.actor())
    {
        return Err(
            "the membership log holds this member's admission of another successor key, and a              member names one, once: a person decides"
                .to_owned(),
        );
    }
    if let Some(budget) = ProofAdmissionCapacity::observe(log).reserved() {
        return Err(format!(
            "the membership log's {} fill three quarters of their budget, and the rest is kept \
             for what people do: a person decides",
            budget.counts()
        ));
    }
    let batch = RecordCounts {
        weighed: 1,
        keyed: wraps,
    };
    // Its bytes are not known before it is signed: each record at its kind's longest.
    let bytes = Kind::Admit
        .most_bytes()
        .saturating_add(wraps.saturating_mul(Kind::Wrap.most_bytes()));
    match log.passed_by(batch, bytes) {
        None => Ok(Standing::New),
        Some(Budget::Bytes) => Err(format!(
            "the membership log nears its limit of record bytes: this admission's {} records, at \
             their longest, would pass it, so none of them is signed",
            batch.held()
        )),
        Some(budget) => Err(format!(
            "the membership log is full: this admission's {} records would pass its limit of {}, \
             so none of them is signed",
            batch.held(),
            budget.counts()
        )),
    }
}

/// Let this member's own Commons writer successor into the vault, as the member at `paths`, at
/// `now_ms` (this Mac's clock), with no person and by `deadline` at most: read the vault's
/// newest records, [`judge`] the statement, then under this Mac's state lock sign the `Admit`
/// of [`Evidence::Manual`] under the next free label and the name the log gave this member's
/// own seat ([`Lineages`]), with no fleet node key (the Mac keeps its seat and its node key),
/// and the `Wrap`s of every epoch a new Mac needs, keep them only where the evaluator seats the
/// successor beside this member, and hand the log bundle. Its records go to every vault server
/// off the caller's thread. A vault no new Mac can reach (one read from a path, or none pinned)
/// is refused with nothing signed. No wait here outlasts `deadline`: the read of the newest
/// records, a vault pass that holds this Mac's state ([`Succession::Busy`]) and another
/// process that holds its device are each waited for until then at most.
///
/// # Errors
/// Local state cannot be read or written, signing fails, or this Mac's device custody is still
/// held at `deadline`.
pub fn admit_successor(
    paths: &Paths,
    succeeding: &Succeeding<'_>,
    now_ms: u64,
    deadline: Instant,
) -> crate::Result<Succession> {
    // The vault's newest records first, as a proof reads them: a member that missed a removal
    // of the successor's key would restore it.
    // [`FRESH_WITHIN`] is how long a member waits for servers that may not be there; a test
    // that started its own on loopback, which answer, waits its hang detector for them
    // ([`super::servers_within`]).
    let fresh = read_newest_by(
        paths,
        deadline.min(Instant::now() + super::servers_within(FRESH_WITHIN)),
    );
    // Read before the state is taken: an entry read under this process's lock finds no policy.
    let policy = super::handover::policy_servers(paths);
    let Some(lock) =
        StateLock::try_take_until(paths, deadline.min(Instant::now() + HANDOVER_LOCK_WAIT))?
    else {
        return Ok(Succession::Busy);
    };
    let Some((mut store, mut log, _, opened)) = held_log(paths)? else {
        return Ok(Succession::NotHere);
    };
    if succeeding.lab && super::live_genesis_store(&store) {
        return Ok(Succession::Refused(
            crate::members::desk::LAB_REFUSED.to_owned(),
        ));
    }
    let vault = match HandedVault::of(super::config(paths)?) {
        Ok(vault) => vault,
        Err(why) => return Ok(Succession::Refused(why.to_owned())),
    };
    let source = records_bound(paths, &store)?;
    if !succeeding.reader.can_take(source) {
        return Ok(Succession::Refused(succeeding.reader.refusal(source)));
    }
    let binding = succeeding.request.binding();
    if let Some(why) = roster_keeps_out(paths, Some(&opened), binding, [0; 32], Some(&lock))? {
        return Ok(Succession::Refused(why));
    }
    let Some((me, device, incarnation)) =
        log_entries::open_local_member_signer_held_until(paths, &log, &lock, Some(deadline))?
    else {
        return Ok(Succession::NotHere);
    };
    if log.audience_left_out(&EpochAudience::Everyone, &me) {
        return Ok(Succession::NotHere);
    }
    let Some(audiences) = held_audiences(&log, &opened, &me) else {
        return Ok(Succession::NotHere);
    };
    let wraps = Log::admission_wraps(&opened, &audiences).len();
    let standing = match judge(&log, &me, succeeding, wraps, now_ms) {
        Ok(standing) => standing,
        Err(why) => return Ok(Succession::Refused(why)),
    };
    if !fresh && standing == Standing::New {
        return Ok(Succession::Refused(NOT_FRESH.to_owned()));
    }
    let actor = binding.actor();
    // The name the log gave this member's own seat, never one read from this Mac's files: it
    // is what ties the successor's seat to this one for a removal ([`Lineages`]).
    let Some(name) = entry_name(&log, &me) else {
        return Ok(Succession::NotHere);
    };
    let (taken, counted) = labels_taken(&opened.by_audience);
    let signed = device.with_seed(|seed| -> crate::Result<Vec<Record>> {
        let signer = Signer {
            actor: &me,
            seed,
            incarnation,
        };
        match standing {
            Standing::Already => {
                wraps_lacking(&mut log, &opened, &audiences, &signer, &actor, now_ms)
            }
            Standing::New => {
                let label = log
                    .next_label(&name, &taken, &counted)
                    .ok_or_else(|| Error::new("no m<N> label up to m99999 is free"))?;
                log.admit_with(
                    &signer,
                    Entrant {
                        request: succeeding.request.clone(),
                        node_key: [0; 32],
                        name: name.clone(),
                        label,
                    },
                    Evidence::Manual,
                    &opened,
                    &audiences,
                    now_ms,
                )
            }
        }
    })?;
    // The evaluator, over what was signed, must seat the successor's own binding beside this
    // member: else nothing is kept.
    let membership = log.membership();
    let Some(seated) = membership
        .get(&actor)
        .filter(|seated| seated.binding == *binding && membership.contains(&me))
    else {
        return Ok(Succession::Refused(
            "the membership log does not seat this successor beside its member after the \
             statement"
                .to_owned(),
        ));
    };
    if !signed.is_empty() {
        store.ingest(&encode_records(&signed))?;
        if signed
            .iter()
            .any(|record| store.get_bytes(record.id()).is_none())
        {
            return Err(Error::new(
                "the successor's admission cannot be retained in full",
            ));
        }
    }
    let source = records_bound(paths, &store)?;
    if !succeeding.reader.can_take(source) {
        return Ok(Succession::Refused(succeeding.reader.refusal(source)));
    }
    if !signed.is_empty() {
        super::write_records(paths, &store)?;
    }
    let label = seated.label.clone();
    let bundle = log_bundle(
        vault,
        succeeding.statement,
        LogAdmission {
            source,
            admit: seated.entry,
            label: label.clone(),
            signed: signed.clone(),
            records: store.encode_held(),
        },
        policy,
        succeeding.reader,
    )?;
    drop(lock);
    if !signed.is_empty() {
        post_aside(paths);
    }
    Ok(Succession::Let {
        bundle: bundle.encode()?,
        label,
        signed: signed.len(),
    })
}
