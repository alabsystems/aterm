// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Mac on the home ALab LAN gets into the vault with nobody, and so does a Mac that is ALab's
//! already (owner, 2026-11-01: "No more asking to join"; 2026-11-02: "we assume that physical
//! access to the LAN is sufficient proof"; `docs/DESIGN-automatic-lan-admission-2026-11-02.md`
//! "Re-admission by proof").
//!
//! The Mac shows one of three things to any member, on a fresh challenge, and that member lets it
//! in by itself, on that one connection, with the log bundle the Mac writes at once
//! ([`admit_by_proof`]):
//!
//! - **Its place on the home ALab LAN.** The connection came from another host on the network
//!   this member reads as its home ALab LAN (`members::desk::Local::automatically_admits`: the
//!   default route's Ethernet or Wi-Fi subnet, never a hotspot, a VPN or a bridge, with fresh
//!   member evidence there, `crate::home_lan`). Being there is the admission: a Mac no seat binds
//!   takes a new seat under the next free label, with no hourly or daily share, and a seat whose
//!   node key it proves moves to its new key as below, whatever binds that key.
//! - **Its seat's node key.** The log binds its fleet node key to a seat by authority: the genesis
//!   lists that key for the founder, or the seat's own `Node` record carries the key's signature
//!   for it. The Mac's vault key changed (a reinstall, a reset vault folder), so the member signs
//!   a `Remove` of the old seat and, after it, an `Admit` of the new key under the old seat's
//!   label with the same node key. The evaluator reads that `Admit` as the same Mac back (the
//!   removed seat is superseded by the node key, `eval.rs`), so the label, the node key and every
//!   grant tied to them carry over. A seat that only its own `Admit` binds to the key (no proof by
//!   the key in the log) moves only where the Mac also proved the fleet's current key below.
//! - **The fleet's current key.** The fleet handshake (`atf/2`) proved the fleet secret the vault
//!   holds now and the node key, bound to that connection's TLS exporter, and this member's own
//!   drive admission takes the node: the fleet already lets that Mac drive this one and read its
//!   files, so the vault is never easier to enter than the fleet. A node key no seat binds gets a
//!   new seat under the next free label.
//!
//! Only record kinds and evidences a 0.147 Mac takes are signed (`Remove`, `Admit` of
//! [`Evidence::Manual`], `Wrap`): a record of a kind it does not know would stop it taking any
//! record of the stream. A 0.147 evaluator does not supersede a seat by its node key: it would
//! read the moved seat's node key and the new recipient as removed, and its epoch tending would
//! leave the returned Mac out of every newer key for good. So a seat moves only once every member
//! the deciding member's fleet book heard lately runs 0.148 or later ([`Book::moved_seats_read`]).
//!
//! What keeps a proof from doing harm ([`decide`]):
//!
//! - A Mac the log removed stays out: its vault key, its recipient, its writer key, its node key,
//!   and a node id the fleet book bound to a key the log holds out. A seat moved off a key never
//!   moves back to it, since that key's seat is then a removed one.
//! - Two Macs holding one node key (Migration Assistant, a restore, a cloned disk) never trade a
//!   seat back and forth: a seat moves only once it signed nothing for [`DARK_FOR_MS`] and the
//!   fleet book does not hear its node key from another address, at most once in
//!   [`READMIT_EVERY_MS`] and at most [`MAX_READMISSIONS`] times in all, and a key another `Admit`
//!   already names, a key two seats hold, and a key a member replaced go to a person. The Mac a
//!   move left out is offered no Undo of it.
//! - Off the home ALab LAN, a member signs at most [`AUTOMATIC_PER_HOUR`] such admissions an
//!   hour, and no member signs one once the log holds [`PROVEN_PER_DAY`] `Admit`s of
//!   [`Evidence::Manual`] from the last day; a Mac on the LAN takes neither share, as the LAN is
//!   the admission. No member signs one once either of the log's record budgets is
//!   [`PROVEN_FILL`] full, nor one whose whole batch (its `Admit`, a moved seat's `Remove`, and
//!   the `Wrap`s) would pass any bound of the log, and a node id the fleet book binds to another
//!   seat's key takes no new seat.
//! - Nothing is kept unless the evaluator, run over the signed records, seats the new key under
//!   the label decided, with the node key it proved.

use super::admit::{
    LogAdmission, held_audiences, held_log, labels_taken, log_bundle, node_removal, removal_naming,
    roster_keeps_out, wraps_lacking,
};
use super::*;
use crate::commons::binding::Request;
use crate::members::Allowed;
use crate::membership_log::{
    Body, Budget, Entrant, EpochAudience, Evidence, Kind, Log, MemberId, NodeStanding, Record,
    RecordCounts, RecordId, Signer, encode_records,
};

/// How long after a seat took a new key it may take another by proof: two Macs that hold one
/// node key then wait for a person instead of trading the seat back and forth.
pub const READMIT_EVERY_MS: u64 = 24 * 60 * 60 * 1000;
/// How many times one node key's seat moves by proof in all; past it a person decides.
pub const MAX_READMISSIONS: usize = 4;
/// How many Macs one member lets in by proof an hour.
pub const AUTOMATIC_PER_HOUR: usize = 4;
/// How many `Admit`s of [`Evidence::Manual`] (what a proof signs, and a person's Allow too) the
/// whole log may hold from the last day before no member lets another Mac in by proof: a limit
/// every member reads alike from the log, so ~86 members' hourly shares cannot add up to
/// hundreds of seats a day for one holder of the fleet's key.
pub const PROVEN_PER_DAY: usize = 8;
/// The share of each of the log's record budgets, the weighed one and the keyed one
/// ([`crate::membership_log::RecordBudgets`]), past which no member lets a Mac in by proof: the
/// log grows and is never pruned, and a full budget takes no record of its kinds again (a full
/// weighed budget no `Admit` or `Remove`, a full keyed one no `Wrap` or `Put`), so the last
/// quarter of each is kept for what people do. The held total keeps no share back: an
/// admission's whole batch need only fit under it ([`decide`]). So the quarters are kept whole
/// only where the held total is the two budgets together: while
/// [`crate::membership_log::MAX_HELD_RECORDS`] is less than their two shares together, proof
/// admissions may fill the log to it, and the log then takes no `Remove`, `Put` or rotation.
pub const PROVEN_FILL: (usize, usize) = (3, 4);

/// The record-budget condition for a new admission by proof. This is only one
/// prerequisite: it grants no membership and does not judge a pending request,
/// an already admitted member, or a separate Commons enrollment exchange.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProofAdmissionCapacity {
    /// Causally complete weighed records (`Genesis`, `Admit`, `Remove`) in the observed log.
    pub weighed_records: usize,
    /// At this many weighed records and above, new proof admissions leave the rest of the
    /// weighed budget to other operations: [`PROVEN_FILL`] of it.
    pub weighed_limit: usize,
    /// Causally complete keyed records (every other kind) in the observed log.
    pub keyed_records: usize,
    /// At this many keyed records and above, new proof admissions leave the rest of the keyed
    /// budget to other operations: [`PROVEN_FILL`] of it.
    pub keyed_limit: usize,
    /// Causally complete records in the observed log, excluding waiting records.
    pub held_records: usize,
    /// The most records the log holds in all: a hard bound an admission's whole batch must fit
    /// under, with no share kept back.
    pub held_cap: usize,
    /// Whether neither budget is [`PROVEN_FILL`] full, so a new proof may be considered. All
    /// other admission checks still apply, the fit of the admission's whole batch among them.
    pub within_limit: bool,
}

impl ProofAdmissionCapacity {
    /// Inspect the same complete record counts and predicate as [`decide`].
    #[must_use]
    pub fn observe(log: &Log) -> Self {
        let counts = log.counts();
        let budgets = log.budgets();
        let reserve = |budget: usize| (budget * PROVEN_FILL.0).div_ceil(PROVEN_FILL.1);
        let weighed_limit = reserve(budgets.weighed);
        let keyed_limit = reserve(budgets.keyed);
        Self {
            weighed_records: counts.weighed,
            weighed_limit,
            keyed_records: counts.keyed,
            keyed_limit,
            held_records: counts.held(),
            held_cap: budgets.held,
            within_limit: counts.weighed < weighed_limit && counts.keyed < keyed_limit,
        }
    }

    /// The budget that is [`PROVEN_FILL`] full, the weighed one first; `None` where neither is.
    #[must_use]
    pub const fn reserved(&self) -> Option<Budget> {
        if self.weighed_records >= self.weighed_limit {
            Some(Budget::Weighed)
        } else if self.keyed_records >= self.keyed_limit {
            Some(Budget::Keyed)
        } else {
            None
        }
    }
}
/// How long a seat must have signed nothing before a proof of its node key moves it: a Mac still
/// running under the seat (its fleet key copied by Migration Assistant, a restore or a cloned
/// disk onto a Mac that made a new vault key) keeps it, and a person decides.
pub const DARK_FOR_MS: u64 = 24 * 60 * 60 * 1000;
/// How long a member reads the vault's newest records before it decides a proof: one that reads
/// none from any server or peer in that time may not know of a removal, and lets no new key in.
/// The whole `prove` exchange runs inside the fleet listener's handshake watchdog
/// (`aterm_net::fleet::HANDSHAKE_WITHIN`, 3 s), so the read takes at most [`super::PROBE_DEADLINE`]
/// in all, once an exchange ([`Proven::fresh`]): the configured server and the LAN peers are read at
/// once, discovery and pulls bounded by it, so one server that does not answer leaves the peers
/// their time (`super::sync_records_until`).
pub const FRESH_WITHIN: Duration = super::PROBE_DEADLINE;
const HOUR_MS: u64 = 60 * 60 * 1000;

/// What a Mac proved to this member on one connection.
#[derive(Clone, Copy, Debug)]
pub struct Proven<'a> {
    /// Its self-signed device request; the possession challenge proved it holds that request's
    /// age identity and writer key.
    pub request: &'a Request,
    /// Its fleet node key, proven on this connection: by the fleet handshake's node proof, bound
    /// to the connection's TLS exporter, or by its signature over this member's fresh greeting
    /// challenge.
    pub node: [u8; 32],
    /// The machine name it asks under, for display.
    pub name: &'a str,
    /// The fleet handshake also proved the fleet's current key, and this member's own drive
    /// admission takes the node.
    pub fleet: bool,
    /// The connection came from another host on this member's home ALab LAN: the Mac is let in
    /// for being there ([`crate::members::desk::Proving::lan`]).
    pub lan: bool,
    /// The check before possession on this exchange read the vault's newest records already
    /// ([`crate::members::desk::Proving::fresh`]): [`admit_by_proof`] reads them no second time.
    pub fresh: bool,
    /// What this member's fleet book says of the Mac and of the fleet.
    pub book: Book,
}

/// What this member's fleet book says of a Mac proving itself and of the fleet, as the caller
/// that holds the book read it (`aterm_agent::vault_join::book_word`): never the Mac's own word.
/// [`Book::default`] is the book that lets nothing move.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Book {
    /// The book binds the node id the Mac proved to another key the log holds out: a removed
    /// Mac that minted a fresh node key under its old node id.
    pub node_id_held_out: bool,
    /// The book binds the node id the Mac proved to another key the log seats: one Mac (a VM that
    /// re-mints its node key at each boot, say) takes one seat by the fleet's key, not one a boot.
    pub node_id_seated: bool,
    /// The book heard the node key the Mac proved, within its live window, from another address
    /// than the one the proof came from: two Macs may hold that key, so its seat stays.
    pub node_elsewhere: bool,
    /// Every member the book heard in the last 14 days runs aterm 0.148.0 or later, whose
    /// evaluator reads a seat moved by its node key (a `Remove` of the old seat, then an `Admit`
    /// of the same node key) as the same Mac back. A 0.146 or 0.147 Mac reads the node key as
    /// removed and so the new seat's key as a removed Mac's: its vault pass mints new
    /// `[everyone]` and Commons keys that leave the returned Mac out, and every build reads that
    /// as on purpose, so the Mac would lose every newer secret for good. Until then a seat moves
    /// by no proof.
    pub moved_seats_read: bool,
}

/// How a proof let a Mac in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum How {
    /// The log let it in already, by this member or another: only the `Wrap`s it lacked.
    Already,
    /// Its node key is the seat `label`'s: the old seat went and the new key took its label.
    Seat {
        /// The seat's label.
        label: String,
    },
    /// Its node key is the one the genesis lists for the founder `label`, which no Mac proved.
    Founder {
        /// The founder's label.
        label: String,
    },
    /// No seat binds its node key, and it proved the fleet's current key: a new seat.
    Fleet {
        /// The label it took.
        label: String,
    },
    /// No seat binds its node key, and it asked from the home ALab LAN: a new seat.
    Lan {
        /// The label it took.
        label: String,
    },
}

/// What [`admit_by_proof`] came to.
#[derive(Debug)]
pub enum Readmit {
    /// Let in.
    Let {
        /// The log bundle the Mac writes its vault from ([`write_from_log_bundle`]).
        bundle: SecretBytes,
        /// How.
        how: How,
        /// How many records this member signed.
        signed: usize,
    },
    /// No proof lets it in here: why, for the Mac's log. It asks a person instead.
    Refused(String),
    /// A vault pass holds this Mac's state; the Mac tries another member.
    Busy,
    /// This member admits nobody through the log: no trusted log with epochs, no seat for its
    /// device key in it, or an `[everyone]` epoch it cannot hand on.
    NotHere,
}

/// What the log says of a proof, before anything is signed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Decision {
    /// The log seats the Mac already.
    Already,
    /// Remove `seat` at `cutoff`, then admit the new key under `label`.
    Replace {
        /// The seat whose node key the Mac proved.
        seat: MemberId,
        /// That seat's last record the log holds, else the record that let it in.
        cutoff: RecordId,
        /// The label the seat claimed.
        label: String,
    },
    /// Admit the new key under the founder's `label`.
    Founder {
        /// The label the genesis lists.
        label: String,
    },
    /// Admit the new key under the next free label: it proved the fleet's current key.
    Fleet,
    /// Admit the new key under the next free label: it asked from the home ALab LAN.
    Lan,
    /// Let nobody in: why.
    Refuse(String),
}

/// Whether the log binds `node` to `seat` by authority: the genesis lists it for that founder,
/// the seat's own `Node` record carries it, whose proof by the key [`Record::decode`] verified,
/// or the seat took it by proof from a seat bound so ([`moved_by_proof`]). Any other `Admit`'s
/// entrant key is the admitting member's word alone.
pub(super) fn bound_by_authority(log: &Log, seat: &MemberId, node: &[u8; 32]) -> bool {
    authority_within(log, seat, node, MAX_READMISSIONS + 1)
}

fn authority_within(log: &Log, seat: &MemberId, node: &[u8; 32], depth: usize) -> bool {
    let listed = matches!(
        log.genesis().body(),
        Body::Genesis { founders, .. }
            if founders.iter().any(|f| f.member() == Some(*seat) && f.node_key == Some(*node))
    );
    listed
        || log.records().any(|r| {
            r.actor() == seat && matches!(r.body(), Body::Node { node_key, .. } if node_key == node)
        })
        || (depth > 0 && moved_by_proof(log, seat, node, depth - 1))
}

/// Whether an `Admit` of [`Evidence::Manual`] let `seat` in with `node` right after its signer
/// removed a seat the log binds `node` to by authority: what [`admit_by_proof`] signs once the Mac
/// proved `node` to that member. The Mac that took a seat so never signs a `Node` record for a
/// key the log binds to it already (`publish_local_node_key`), so this is how its next move is
/// proven too.
fn moved_by_proof(log: &Log, seat: &MemberId, node: &[u8; 32], depth: usize) -> bool {
    log.records().any(|r| {
        matches!(r.body(), Body::Admit { entrant, evidence: Evidence::Manual }
            if entrant.member() == *seat && entrant.node_key == *node)
            && r.previous().and_then(|p| log.get(p)).is_some_and(|before| {
                before.actor() == r.actor()
                    && matches!(before.body(), Body::Remove { member, .. }
                        if authority_within(log, member, node, depth))
            })
    })
}

/// Whether `seat` signed any record at or after `since_ms` (its signer's clock): its Mac may
/// still run under it.
fn signed_since(log: &Log, seat: &MemberId, since_ms: u64) -> bool {
    log.records()
        .any(|r| r.kind() != Kind::Genesis && r.actor() == seat && r.authored_ms() >= since_ms)
}

/// Whether `remover` removed `seat` only to seat a new vault key under it by the seat's node key
/// (what [`admit_by_proof`] signs: the `Remove`, then at once an `Admit` of [`Evidence::Manual`]
/// of a node key bound to `seat`). The Mac left out by such a move is offered no Undo of it.
pub(super) fn seat_moved_by(log: &Log, remover: &MemberId, seat: &MemberId) -> bool {
    log.records().any(|r| {
        r.actor() == remover
            && matches!(r.body(), Body::Admit { entrant, evidence: Evidence::Manual }
                if binds(log, seat, &entrant.node_key))
            && r.previous().and_then(|p| log.get(p)).is_some_and(|before| {
                before.actor() == remover
                    && matches!(before.body(), Body::Remove { member, .. } if member == seat)
            })
    })
}

/// Whether any record binds `node` to `member`: the genesis, an `Admit` of it, or its `Node`.
fn binds(log: &Log, member: &MemberId, node: &[u8; 32]) -> bool {
    bound_by_authority(log, member, node)
        || log.records().any(|r| {
            matches!(r.body(), Body::Admit { entrant, .. }
                if entrant.member() == *member && entrant.node_key == *node)
        })
}

/// When each move of `node`'s seat by proof was signed (the signer's clock, in UTC
/// milliseconds): an `Admit` of [`Evidence::Manual`] naming `node` whose signer's previous record
/// is the `Remove` of a seat bound to `node`.
pub(super) fn readmissions(log: &Log, node: &[u8; 32]) -> Vec<u64> {
    log.records()
        .filter(|r| {
            matches!(r.body(), Body::Admit { entrant, evidence: Evidence::Manual }
                if entrant.node_key == *node)
                && r.previous().and_then(|p| log.get(p)).is_some_and(|before| {
                    before.actor() == r.actor()
                        && matches!(before.body(), Body::Remove { member, .. }
                            if binds(log, member, node))
                })
        })
        .map(Record::authored_ms)
        .collect()
}

/// How many `Admit`s of [`Evidence::Manual`] `me` signed in the hour before `now_ms`.
fn automatic_this_hour(log: &Log, me: &MemberId, now_ms: u64) -> usize {
    log.records()
        .filter(|r| {
            r.actor() == me
                && matches!(
                    r.body(),
                    Body::Admit {
                        evidence: Evidence::Manual,
                        ..
                    }
                )
                && now_ms.saturating_sub(r.authored_ms()) < HOUR_MS
        })
        .count()
}

/// How many `Admit`s of [`Evidence::Manual`] any member signed in the day before `now_ms`.
fn manual_this_day(log: &Log, now_ms: u64) -> usize {
    log.records()
        .filter(|r| {
            matches!(
                r.body(),
                Body::Admit {
                    evidence: Evidence::Manual,
                    ..
                }
            ) && now_ms.saturating_sub(r.authored_ms()) < READMIT_EVERY_MS
        })
        .count()
}

/// The label `seat` claimed: its genesis entry's, else its `Admit`'s.
fn claimed_label(log: &Log, seat: &MemberId) -> Option<String> {
    if let Body::Genesis { founders, .. } = log.genesis().body()
        && let Some(f) = founders.iter().find(|f| f.member() == Some(*seat))
    {
        return Some(f.label.clone());
    }
    let entry = log.membership().get(seat)?.entry;
    match log.get(&entry)?.body() {
        Body::Admit { entrant, .. } => Some(entrant.label.clone()),
        _ => None,
    }
}

/// What the log says the member `me` may do with `proven` at `now_ms` (its clock): the rule in
/// the module's head, read from `log` alone, so every member decides one proof alike. `wraps` is
/// how many `Wrap`s an admission by `me` signs beside its `Admit` ([`Log::admission_wraps`]): an
/// admission whose whole batch would pass a bound of the log, its bytes counted at each record
/// kind's longest ([`Kind::most_bytes`]), is refused here, before any of it is signed.
pub(super) fn decide(
    log: &Log,
    me: &MemberId,
    proven: &Proven<'_>,
    wraps: usize,
    now_ms: u64,
) -> Decision {
    let refuse = |why: &str| Decision::Refuse(why.to_owned());
    if now_ms == 0 {
        return refuse("this member's clock is not set");
    }
    let binding = proven.request.binding();
    let actor = binding.actor();
    let recipient = binding.recipient();
    let node = proven.node;
    let membership = log.membership();
    if node == [0; 32] {
        return refuse("it proved no node key");
    }
    if membership.get(me).and_then(|m| m.node_key) == Some(node) {
        return refuse("it proved this member's own node key");
    }
    if removal_naming(log, binding).is_some()
        || log.recipient_is_removed(&recipient)
        || node_removal(log, *binding.writer()).is_some()
    {
        return refuse("the vault removed this Mac's key: a person's Allow again decides");
    }
    if proven.book.node_id_held_out {
        return refuse("its node id held a key the vault removed: a person decides");
    }
    if log.audience_left_out_recipient(&EpochAudience::Everyone, &recipient)
        || (membership.contains(&actor) && log.audience_left_out(&EpochAudience::Everyone, &actor))
    {
        return refuse("the vault's newest key leaves this Mac out on purpose");
    }
    if membership.contains(&actor) {
        return Decision::Already;
    }
    if !proven.lan && automatic_this_hour(log, me, now_ms) >= AUTOMATIC_PER_HOUR {
        return refuse("this member let its share of Macs in this hour; another member may");
    }
    if !proven.lan && manual_this_day(log, now_ms) >= PROVEN_PER_DAY {
        return refuse(
            "the vault let as many Macs in as it lets in by proof a day: a person decides, or the \
             Mac comes in by itself tomorrow",
        );
    }
    if let Some(budget) = ProofAdmissionCapacity::observe(log).reserved() {
        return Decision::Refuse(format!(
            "the membership log's {} fill three quarters of their budget, and the rest is kept \
             for what people do: a person decides",
            budget.counts()
        ));
    }
    let decision = match membership.node(&node) {
        NodeStanding::Member(seat) => {
            let Some(held) = membership.get(&seat) else {
                return refuse("the seat this node key names is gone");
            };
            if log.recipient_is_removed(&held.binding.recipient())
                || membership.member_node_removed(held)
                || log.audience_left_out(&EpochAudience::Everyone, &seat)
            {
                return refuse("the seat this node key names is held out");
            }
            if !bound_by_authority(log, &seat, &node) && !proven.fleet && !proven.lan {
                return refuse(
                    "only that seat's Admit binds this node key, which no proof by the key backs: \
                     proving the fleet's key too, or asking from the home ALab LAN, lets it in",
                );
            }
            let past = readmissions(log, &node);
            if past.len() >= MAX_READMISSIONS {
                return refuse("this node key's seat took a new key too often: a person decides");
            }
            if past
                .iter()
                .any(|&at| now_ms.saturating_sub(at) < READMIT_EVERY_MS)
            {
                return refuse(
                    "this node key's seat took a new key less than a day ago, so two Macs may \
                     hold it (a clone or a restore): a person decides",
                );
            }
            if signed_since(log, &seat, now_ms.saturating_sub(DARK_FOR_MS)) {
                return refuse(
                    "the seat this node key names signed a record in the last day, so its Mac may \
                     still run (a copy of its fleet key on a Mac with a new vault key?): a person \
                     decides",
                );
            }
            if proven.book.node_elsewhere {
                return refuse(
                    "this member's fleet book heard that node key from another address just now, \
                     so two Macs may hold it: a person decides",
                );
            }
            if !proven.book.moved_seats_read {
                return refuse(
                    "a member heard in the last 14 days runs an aterm older than 0.148, which \
                     would read the moved seat as removed and leave this Mac out of the vault's \
                     newer keys for good: the seat moves by itself once every member updated",
                );
            }
            let Some(label) = claimed_label(log, &seat) else {
                return refuse("the seat this node key names claims no label");
            };
            let cutoff = log
                .records()
                .filter(|r| r.kind() != Kind::Genesis && *r.actor() == seat)
                .max_by(|a, b| a.sequence().cmp(&b.sequence()).then(b.id().cmp(a.id())))
                .map_or(held.entry, |r| *r.id());
            Decision::Replace {
                seat,
                cutoff,
                label,
            }
        }
        NodeStanding::Unknown => {
            if log.records().any(
                |r| matches!(r.body(), Body::Admit { entrant, .. } if entrant.node_key == node),
            ) {
                return refuse(
                    "another Admit names this node key already, so two Macs may hold it: a person \
                     decides",
                );
            }
            if proven.book.node_id_seated {
                return refuse(
                    "its node id holds another seat's node key already, and one Mac takes one \
                     seat: a person decides",
                );
            }
            if let Some(founder) = membership
                .unproved()
                .iter()
                .find(|f| f.node_key == Some(node))
            {
                Decision::Founder {
                    label: founder.label.clone(),
                }
            } else if proven.fleet {
                Decision::Fleet
            } else if proven.lan {
                Decision::Lan
            } else {
                refuse(NO_SEAT)
            }
        }
        NodeStanding::Retired(_) => refuse("a member replaced this node key with another"),
        NodeStanding::Contested => refuse("two seats hold this node key: a person decides"),
        NodeStanding::Removed(_) => refuse("the vault removed this node key: a person decides"),
    };
    // The weighed records it signs: a moved seat's `Remove`, then the `Admit`.
    let weighed: &[Kind] = match &decision {
        Decision::Replace { .. } => &[Kind::Remove, Kind::Admit],
        Decision::Founder { .. } | Decision::Fleet | Decision::Lan => &[Kind::Admit],
        Decision::Already | Decision::Refuse(_) => return decision,
    };
    let batch = RecordCounts {
        weighed: weighed.len(),
        keyed: wraps,
    };
    // Its bytes are not known before it is signed: each record at its kind's longest.
    let bytes = weighed
        .iter()
        .map(|kind| kind.most_bytes())
        .sum::<usize>()
        .saturating_add(wraps.saturating_mul(Kind::Wrap.most_bytes()));
    match log.passed_by(batch, bytes) {
        None => decision,
        Some(Budget::Bytes) => Decision::Refuse(format!(
            "the membership log nears its limit of record bytes: this admission's {} records, \
             at their longest, would pass it, so none of them is signed",
            batch.held()
        )),
        Some(budget) => Decision::Refuse(format!(
            "the membership log is full: this admission's {} records would pass its limit of {}, \
             so none of them is signed",
            batch.held(),
            budget.counts()
        )),
    }
}

/// Why a Mac with no seat, no fleet key and no place on the home ALab LAN gets none by its proof.
const NO_SEAT: &str = "no seat binds this node key, the Mac did not prove the fleet's key, and it \
                       asked from off this member's home ALab LAN";

/// Why a member that cannot sign lets nobody in.
const NOT_HERE: &str = "this member admits nobody through the membership log";

/// Why a member that may not know of a removal lets no new key in.
const NOT_FRESH: &str = "this member did not read all of the vault's newest records just now (no \
                         server or peer answered, or one holds records this build cannot take), \
                         so it may not know of a removal: another member may let the Mac in";

/// Whether this member read all of the vault's newest records from its servers and peers just now
/// (within [`FRESH_WITHIN`]).
fn read_newest(paths: &Paths) -> bool {
    super::sync_records_until(
        paths,
        None,
        &[],
        None,
        Some(Instant::now() + FRESH_WITHIN),
        None,
    )
    .is_ok_and(|pass| !pass.untaken)
}

/// Whether a Mac under `node` has nothing in `log` that gives it a claim: no seat, no `Admit` and no
/// unproved founder names the node key.
fn unclaimed(log: &Log, node: &[u8; 32]) -> bool {
    let membership = log.membership();
    membership.node(node) == NodeStanding::Unknown
        && !membership
            .unproved()
            .iter()
            .any(|founder| founder.node_key == Some(*node))
        && !log
            .records()
            .any(|r| matches!(r.body(), Body::Admit { entrant, .. } if entrant.node_key == *node))
}

/// What [`refuses_before_possession_at_ms`] reads from `paths`'s held log, roster and signer on one
/// pass: `Err(Some(why))` refuses before possession, `Err(None)` leaves an unreadable log or signer
/// undecided here, and `Ok(decision)` is [`decide`]'s verdict when this member can sign.
///
/// On the unsynced pass (`!synced`), a Mac that proved neither the fleet's key nor a claim in the
/// local log, and asked from off the home ALab LAN, is told [`NO_SEAT`] before touching the roster
/// or local signer, so an unauthenticated caller learns nothing more of the log or of a copied
/// recipient and triggers no server read.
fn pre_possession_pass(
    paths: &Paths,
    proving: &crate::members::desk::Proving<'_>,
    proven: &Proven<'_>,
    now_ms: u64,
    synced: bool,
) -> Result<Decision, Option<String>> {
    let Some((_, log, _, opened)) = held_log(paths).map_err(|_| None)? else {
        return Err(Some(NOT_HERE.to_owned()));
    };
    if !synced && !proving.fleet && !proving.lan && unclaimed(&log, &proving.node) {
        return Err(Some(NO_SEAT.to_owned()));
    }
    let binding = proving.request.binding();
    if let Some(why) =
        roster_keeps_out(paths, Some(&opened), binding, proving.node, None).map_err(|_| None)?
    {
        return Err(Some(why));
    }
    let Some((me, ..)) = log_entries::open_local_member_signer(paths, &log).map_err(|_| None)?
    else {
        return Err(Some(NOT_HERE.to_owned()));
    };
    if log.audience_left_out(&EpochAudience::Everyone, &me) {
        return Err(Some(NOT_HERE.to_owned()));
    }
    let Some(audiences) = held_audiences(&log, &opened, &me) else {
        return Err(Some(NOT_HERE.to_owned()));
    };
    let wraps = Log::admission_wraps(&opened, &audiences).len();
    match decide(&log, &me, proven, wraps, now_ms) {
        Decision::Refuse(why) => Err(Some(why)),
        decision => Ok(decision),
    }
}

/// Why the member at `paths` refuses `proving` at `now_ms` whatever the Mac proves next, or `None`
/// where it would go on, as [`admit_by_proof`] rules. A desk asks this BEFORE the possession
/// challenge, so the Mac has sent nothing a member could sign on and may try the next member and its
/// plain ask (`join::prove_first`): a `NO` after possession stays ambiguous, as nothing authenticates
/// it, and a forged one must not send the Mac to a second member. Refusing on an older log is the
/// safe side, as nothing was signed and the Mac tries again. A readable state with no usable log
/// refuses here too; a log or signer it cannot read decides nothing here.
///
/// A Mac that proved neither the fleet's key nor a claim on its node key, and asked from off the
/// home ALab LAN, has shown only keys it chose, and is told [`NO_SEAT`] and nothing more of the log
/// or of a recipient it may have copied, with no server read, so a Mac that proves itself to each
/// member in turn waits on none of them. For any other (a Mac on the LAN included) the vault's
/// newest records come only once [`decide`] would let it in, and the log,
/// roster, local signer and [`decide`] are checked again on what came back.
///
/// What stays for after possession: [`Book::moved_seats_read`]'s refusal, as the seat moves by itself
/// once every member updated and a plain ask would put a person's Allow in front of the very seat it
/// protects, and whatever changes or fails between this and the signature.
#[must_use]
pub fn refuses_before_possession_at_ms(
    paths: &Paths,
    proving: &crate::members::desk::Proving<'_>,
    book: Book,
    now_ms: u64,
) -> Option<String> {
    let proven = Proven {
        request: proving.request,
        node: proving.node,
        name: proving.name,
        fleet: proving.fleet,
        lan: proving.lan,
        fresh: false,
        book: Book {
            moved_seats_read: true,
            ..book
        },
    };
    match pre_possession_pass(paths, proving, &proven, now_ms, false) {
        Err(refusal) => return refusal,
        Ok(Decision::Already) => return None,
        Ok(_) => {}
    }
    if !read_newest(paths) {
        return Some(NOT_FRESH.to_owned());
    }
    proving.fresh.set(true);
    match pre_possession_pass(paths, proving, &proven, now_ms, true) {
        Err(refusal) => refusal,
        Ok(_) => None,
    }
}

/// [`refuses_before_possession_at_ms`] at this Mac's clock.
#[must_use]
pub fn refuses_before_possession(
    paths: &Paths,
    proving: &crate::members::desk::Proving<'_>,
    book: Book,
) -> Option<String> {
    refuses_before_possession_at_ms(paths, proving, book, aterm_time::unix_millis().unwrap_or(0))
}

/// The machine name an `Allowed` and an `Admit` carry for a Mac asking under `asked`.
fn admitted_name(asked: &str) -> String {
    let lower = asked.to_ascii_lowercase();
    if text::valid_machine_name(&lower) {
        lower
    } else {
        "machine".to_owned()
    }
}

/// Let the Mac `proven` names in by its proof, as the member at `paths`, at `now_ms` (this Mac's
/// clock): read the vault's newest records (within [`FRESH_WITHIN`]; a member that reads none
/// signs nothing new), [`decide`], then under this Mac's state lock sign what the decision says and keep it
/// only where the evaluator seats the new key as decided, and hand the log bundle, with an
/// `Allowed` this member signs for the approvers 0.145 Macs read. Its records go to every vault
/// server off the caller's thread. Nothing here waits on a person, and nothing runs on a window's
/// main thread: the callers are the fleet listener's connection threads.
///
/// # Errors
/// Local state cannot be read or written, or signing fails.
pub fn admit_by_proof(paths: &Paths, proven: &Proven<'_>, now_ms: u64) -> crate::Result<Readmit> {
    // The vault's newest records first, from its servers and peers: a member that has not read
    // a removal since (asleep, or away with only the removed Mac) would let that Mac back in. So
    // would one whose server holds a removal it cannot take, past a record of a kind only a newer
    // build writes: a pass that took records beside it, or named a memo's heads so the server
    // sent nothing, still did not read the vault's newest records.
    let fresh = proven.fresh || read_newest(paths);
    // Read before the state is taken: an entry read under this process's lock finds no policy.
    let policy = super::handover::policy_servers(paths);
    let Some(lock) = StateLock::try_take_until(paths, Instant::now() + HANDOVER_LOCK_WAIT)? else {
        return Ok(Readmit::Busy);
    };
    let Some((mut store, mut log, _, opened)) = held_log(paths)? else {
        return Ok(Readmit::NotHere);
    };
    let binding = proven.request.binding();
    if let Some(why) = roster_keeps_out(paths, Some(&opened), binding, proven.node, Some(&lock))? {
        return Ok(Readmit::Refused(why));
    }
    let Some((me, device, incarnation)) =
        log_entries::open_local_member_signer_held(paths, &log, &lock)?
    else {
        return Ok(Readmit::NotHere);
    };
    if log.audience_left_out(&EpochAudience::Everyone, &me) {
        return Ok(Readmit::NotHere);
    }
    let Some(audiences) = held_audiences(&log, &opened, &me) else {
        return Ok(Readmit::NotHere);
    };
    let wraps = Log::admission_wraps(&opened, &audiences).len();
    let decision = decide(&log, &me, proven, wraps, now_ms);
    if let Decision::Refuse(why) = decision {
        return Ok(Readmit::Refused(why));
    }
    if !fresh && decision != Decision::Already {
        return Ok(Readmit::Refused(NOT_FRESH.to_owned()));
    }
    let actor = binding.actor();
    let name = admitted_name(proven.name);
    let entrant = |label: String| Entrant {
        request: proven.request.clone(),
        node_key: proven.node,
        name: name.clone(),
        label,
    };
    let (taken, counted) = labels_taken(&opened.by_audience);
    let (signed, how) = device.with_seed(|seed| -> crate::Result<(Vec<Record>, How)> {
        let signer = Signer {
            actor: &me,
            seed,
            incarnation,
        };
        Ok(match decision.clone() {
            Decision::Already => (
                wraps_lacking(&mut log, &opened, &audiences, &signer, &actor, now_ms)?,
                How::Already,
            ),
            Decision::Replace {
                seat,
                cutoff,
                label,
            } => {
                let remove = log
                    .draft(
                        &me,
                        now_ms,
                        Body::Remove {
                            member: seat,
                            cutoff,
                        },
                    )
                    .sign(seed, incarnation)?;
                log.insert(remove.clone())?;
                let mut signed = vec![remove];
                signed.extend(log.admit_with(
                    &signer,
                    entrant(label.clone()),
                    Evidence::Manual,
                    &opened,
                    &audiences,
                    now_ms,
                )?);
                (signed, How::Seat { label })
            }
            Decision::Founder { label } => (
                log.admit_with(
                    &signer,
                    entrant(label.clone()),
                    Evidence::Manual,
                    &opened,
                    &audiences,
                    now_ms,
                )?,
                How::Founder { label },
            ),
            Decision::Fleet | Decision::Lan => {
                let label = log
                    .next_label(&name, &taken, &counted)
                    .ok_or_else(|| Error::new("no m<N> label up to m99999 is free"))?;
                let signed = log.admit_with(
                    &signer,
                    entrant(label.clone()),
                    Evidence::Manual,
                    &opened,
                    &audiences,
                    now_ms,
                )?;
                let how = if decision == Decision::Fleet {
                    How::Fleet { label }
                } else {
                    How::Lan { label }
                };
                (signed, how)
            }
            Decision::Refuse(why) => return Err(Error::new(why)),
        })
    })?;
    // The evaluator, over what was signed, must seat the new key as decided: else nothing is
    // kept, and the Mac asks a person.
    let membership = log.membership();
    let Some(seated) = membership.get(&actor) else {
        return Ok(Readmit::Refused(
            "the membership log does not seat this Mac after its proof".to_owned(),
        ));
    };
    let label_held = match &how {
        How::Already => true,
        How::Seat { label }
        | How::Founder { label }
        | How::Fleet { label }
        | How::Lan { label } => seated.label == *label,
    };
    let node_held = match &how {
        How::Already => true,
        How::Seat { .. } | How::Founder { .. } | How::Fleet { .. } | How::Lan { .. } => {
            membership.node(&proven.node) == NodeStanding::Member(actor)
        }
    };
    if !label_held || !node_held {
        return Ok(Readmit::Refused(format!(
            "the membership log would seat this Mac as {} after its proof, not as decided",
            seated.label
        )));
    }
    let admission = LogAdmission {
        admit: seated.entry,
        label: seated.label.clone(),
        signed: signed.clone(),
        records: Vec::new(),
    };
    if !signed.is_empty() {
        store.ingest(&encode_records(&signed))?;
        super::write_records(paths, &store)?;
    }
    let allowed = device.with_seed(|seed| Allowed::sign(binding, &name, seed))?;
    let bundle = log_bundle(
        paths,
        &allowed,
        LogAdmission {
            records: store.encode(),
            ..admission
        },
        policy,
    )?;
    drop(lock);
    if !signed.is_empty() {
        post_aside(paths);
    }
    Ok(Readmit::Let {
        bundle: bundle.encode()?,
        how,
        signed: signed.len(),
    })
}

/// A desk's [`crate::members::desk::Prove`] for the member at `paths`: [`admit_by_proof`] of the
/// Mac `proving` names at this Mac's clock, its bundle where it let the Mac in, else why not,
/// which the Mac reads (and then asks a person). `book` is the fleet book's word ([`Book`]); a
/// proof on the desk's own port names no node id.
///
/// # Errors
/// It let the Mac in by no proof here, or local state failed.
pub fn prove_at(
    paths: &Paths,
    proving: &crate::members::desk::Proving<'_>,
    book: Book,
) -> crate::Result<SecretBytes> {
    prove_at_ms(paths, proving, book, aterm_time::unix_millis().unwrap_or(0))
}

/// [`prove_at`] at `now_ms` (this Mac's clock).
///
/// # Errors
/// As [`prove_at`].
pub fn prove_at_ms(
    paths: &Paths,
    proving: &crate::members::desk::Proving<'_>,
    book: Book,
    now_ms: u64,
) -> crate::Result<SecretBytes> {
    let proven = Proven {
        request: proving.request,
        node: proving.node,
        name: proving.name,
        fleet: proving.fleet,
        lan: proving.lan,
        fresh: proving.fresh.get(),
        book,
    };
    match admit_by_proof(paths, &proven, now_ms)? {
        Readmit::Let { bundle, .. } => Ok(bundle),
        Readmit::Refused(why) => Err(Error::new(why)),
        Readmit::Busy => Err(Error::new(
            "a vault pass on this member holds its state; try another member or again soon",
        )),
        Readmit::NotHere => Err(Error::new(NOT_HERE)),
    }
}

/// Post this Mac's records to every vault server it reaches ([`post_records_to_servers`]), on a
/// thread of its own: the bundle goes back to the Mac first.
fn post_aside(paths: &Paths) {
    let paths = paths.clone();
    let _ = std::thread::Builder::new()
        .name("readmit-post".to_owned())
        .spawn(move || drop(post_records_to_servers(&paths)));
}

#[cfg(test)]
#[path = "readmit_tests.rs"]
mod tests;
