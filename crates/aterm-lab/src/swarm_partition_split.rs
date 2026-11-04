// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `swarm partition`'s split ledger: whether a split the lane holds kept every vault record and
//! vault head on the island that made it, so that what the lane says of the product is never a
//! leak of the lab's own split.
//!
//! A split is the lane's `ATERM_DEBUG_PARTITION` cut ([`Cut`]): every process of each island
//! (its instances, its vault server, its Macs' subscribers and approvers, every CLI the lab runs
//! in its worlds) reads its island's file on every connect, and the lab makes each act on a
//! Mac's behalf in a process of that Mac's world ([`Cut::act`]), which the cut covers whole. An
//! in-process cut (`aterm_uds::partition::cut_here`) holds its own thread alone, and a writer's
//! records post goes to every server at once, on threads of its own. Every plane
//! that carries vault records dials through that one funnel (`aterm_uds::connect_tcp`): a
//! subscriber's records sync (its pull from its own server and from the other servers the
//! vault's `aterm-swarm-policy` lists, and its post of its held records to any of them that
//! lacks its frontier), a writer's post of its records to every server it knows, the envelope's
//! fetch and publish, and a server's pull from the peers it browses. Commons carries records
//! only between two Macs the lab pairs, and during a split it pairs Macs of one island. The
//! fleet carries none: its dials register, read a book and push survey facts.
//!
//! The ledger reads every holder three times, as the lab reads them (never cut): the moment the
//! cut is up, again [`LAND`] later (its start read, before the split's first act), and just
//! before the heal; each vault Mac's own records and head, and each island's server's. The cut
//! refuses a new dial alone, never a connection already made, so an exchange that was crossing
//! when it fell may still land: the start read waits that out. A record or head crossed where
//! both islands hold it at the end and either one island did not hold it at the start read (it
//! reached that island during the split, whoever made it and whenever) or no holder held it when
//! the cut fell (it was made during the split on one island, since a record is signed by one
//! Mac, which is on one island, and reached the other before the start read). So a record one
//! island held alone going in is judged as closely as one made during the split. [`crossings`]
//! names each, where it was found and the plane that carried it, and [`judge`] fails a split
//! that let one cross or that it could not read whole. A split the lab means to hold a records
//! sync back runs one in a Mac's world under it ([`synced`]), whose own island's server must
//! then hold what the Mac made, so the split's row has seen that pass held, not merely idle.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use aterm_secrets::client::{self, Paths};
use aterm_secrets::membership_log::{self, MemberId, RecordId};

use super::{Check, Island};

/// How long a read of a server may take: the hang detector, as the server must answer.
const READ: Duration = super::HANG;

/// How long after the cut falls the ledger takes its start read, before the split's first act:
/// an exchange that was crossing when the cut fell (the cut refuses a new dial alone) has landed
/// by then. A no-wait records pass's pull from the other island's server ends within
/// [`client::PROBE_DEADLINE`] whole, and the Mac keeps what it brought within as long again; a
/// records post or a publish to a loopback server answers in milliseconds. One that lands later
/// still is a crossing of a record its island held when the cut fell ([`Crossing::held_at_cut`]):
/// a lab failure, which the crossing's words name as possibly that, never a verdict on the
/// product.
pub const LAND: Duration = client::PROBE_DEADLINE.saturating_mul(2);

/// Wait out [`LAND`] from `cut`, the moment the cut fell.
///
/// # Errors
/// The run was interrupted.
pub fn land(cut: Instant) -> Result<(), crate::stop::Stopped> {
    crate::stop::sleep(LAND.saturating_sub(cut.elapsed()))
}

/// The lane's cut, as a plane that holds a split of its own drives it.
pub trait Cut {
    /// Cut the islands apart (`true`) or heal them (`false`), rewriting both islands'
    /// `ATERM_DEBUG_PARTITION` files.
    ///
    /// # Errors
    /// A cut file could not be written.
    fn set(&self, cut: bool) -> Result<(), String>;
    /// Make the act `words` on `mac`'s behalf in a process of `mac`'s world, cut as every
    /// process of its island is (the lab's `partition-act` verb, [`super::removals::act`]): what
    /// it printed.
    ///
    /// # Errors
    /// The act could not run, or it failed: what it said.
    fn act(&self, mac: &str, words: &[&str]) -> Result<String, String>;
    /// Each island's vault server's URL.
    fn servers(&self) -> Vec<(Island, String)>;
}

/// Each member id the ledger has seen in a Mac's own log, with that Mac's name and island.
pub type Signers = BTreeMap<MemberId, (String, Island)>;

/// One vault Mac the ledger reads.
#[derive(Clone, Debug)]
pub struct Watched {
    /// Its name.
    pub name: String,
    /// Its island.
    pub island: Island,
    /// Its vault paths.
    pub paths: Paths,
}

/// A vault head, by its envelope's digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadMark {
    /// Its sequence number.
    pub seq: u64,
    /// Its digest, in hex.
    pub digest: String,
    /// The digests, in hex, of the heads it descends from, where the ledger read its envelope (a
    /// server's head; a Mac's `state.toml` names none): a head that crossed and was then replaced
    /// is seen in its successor's parents.
    pub parents: Vec<String>,
}

impl HeadMark {
    /// `seq <n> <first 8 hex digits>`.
    #[must_use]
    pub fn word(&self) -> String {
        format!(
            "seq {} {}",
            self.seq,
            self.digest.get(..8).unwrap_or(&self.digest)
        )
    }
}

/// One record, as the ledger names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Made {
    /// What it is: `a1's Remove 1a2b3c4d`.
    pub what: String,
    /// The island of the Mac that signed it, where the ledger knows that Mac.
    pub from: Option<Island>,
}

/// One holder of vault records at one moment: a vault Mac, or an island's server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    /// A Mac's name (`a1`), or `island A's server`.
    pub name: String,
    /// Its island.
    pub island: Island,
    /// It is a server.
    pub server: bool,
    /// Each record it holds.
    pub records: BTreeMap<RecordId, Made>,
    /// The vault head it holds.
    pub head: Option<HeadMark>,
}

/// Every holder at one moment, and each one that could not be read, with why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// The holders read.
    pub holders: Vec<Holder>,
    /// `<holder>: <why>` for each one that could not be read.
    pub unread: Vec<String>,
}

/// A records sync the lab ran in a Mac's world under the split (`aterm secrets sync`, cut as its
/// island is): the pass that would carry a record across if the cut let it, since it posts the
/// Mac's held records to every server that lacks its frontier, its own island's and the other's.
/// Its own island's server must take what the Mac made during the split, or the split is not
/// shown to have held that pass back ([`Shortfall::Unexercised`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Synced {
    /// The Mac whose sync it was.
    pub mac: String,
    /// Its island.
    pub island: Island,
    /// How many records the Mac held once its sync was done that no holder held when the cut
    /// fell: what the sync had to post.
    pub made: usize,
    /// Each of those its island's server lacked once the sync was done (`a1's Remove 1a2b3c4d`).
    pub lacking: Vec<String>,
    /// Why the Mac or its island's server could not be read, where one could not.
    pub unread: Option<String>,
    /// What the sync said, where it failed.
    pub said: Option<String>,
}

/// One split's ledger.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ledger {
    /// Every holder the moment the cut was up.
    pub cut: Snapshot,
    /// Every holder [`LAND`] after the cut, before the split's first act: its start read.
    pub start: Snapshot,
    /// Every holder just before the heal.
    pub end: Snapshot,
    /// Milliseconds from the cut to the start read.
    pub landed_ms: u64,
    /// Milliseconds from the cut to the end read.
    pub ms: u64,
    /// Why the lab could not hold or end the split, where it could not.
    pub failed: Option<String>,
    /// Each records sync the lab ran under the split ([`Synced`]), in order.
    pub syncs: Vec<Synced>,
}

/// The plane that carried what crossed a split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plane {
    /// The records sync: the other island's server holds it, posted to it by a Mac (a writer's
    /// post, or a subscriber's post of its held records to a server that lacks its frontier)
    /// or pulled by it from a peer.
    RecordsSync,
    /// Straight to a Mac: no server of the island it reached holds it, so a Mac took it itself
    /// (its subscriber's pull from the other island's server, a Commons exchange, or a carry).
    Direct,
    /// The envelope: a vault head made on one island and held on the other (its fetch or its
    /// publish).
    Envelope,
}

impl Plane {
    /// Its words in a check.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Plane::RecordsSync => "the records sync (a post to, or a pull by, a server)",
            Plane::Direct => {
                "a Mac's own pull (from the other island's server), a Commons exchange or a carry"
            }
            Plane::Envelope => "the envelope (its fetch or its publish)",
        }
    }
}

/// One record or head that crossed a split.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crossing {
    /// What crossed: `a1's Remove 1a2b3c4d`, `head seq 12 ab12cd34`.
    pub what: String,
    /// The plane that carried it.
    pub plane: Plane,
    /// Every holder of it at the split's end.
    pub on: Vec<String>,
    /// A holder held it when the cut fell: it crossed after the start read, or an exchange that
    /// was crossing when the cut fell landed past [`LAND`] (the lab's failure either way).
    pub held_at_cut: bool,
}

impl Crossing {
    /// Its words in a check or a report: `<what> crossed <split> by <plane>: on <holders>`.
    #[must_use]
    pub fn line(&self, split: &str) -> String {
        format!(
            "{} crossed {split} by {}{}: on {}",
            self.what,
            self.plane.word(),
            if self.held_at_cut {
                " (held on one island when the cut fell: it crossed after the start read, or an \
                 exchange in flight then landed late)"
            } else {
                ""
            },
            self.on.join(", ")
        )
    }
}

/// Every record any holder of `snapshot` holds.
fn records_anywhere(snapshot: &Snapshot) -> BTreeSet<&RecordId> {
    snapshot
        .holders
        .iter()
        .flat_map(|h| h.records.keys())
        .collect()
}

/// Every record a holder of `snapshot` on `island` holds.
fn records_on(snapshot: &Snapshot, island: Island) -> BTreeSet<&RecordId> {
    snapshot
        .holders
        .iter()
        .filter(|h| h.island == island)
        .flat_map(|h| h.records.keys())
        .collect()
}

/// Every head digest `holder` shows: its head's, and those its head descends from.
fn heads_of(holder: &Holder) -> impl Iterator<Item = &str> {
    holder.head.iter().flat_map(|m| {
        std::iter::once(m.digest.as_str()).chain(m.parents.iter().map(String::as_str))
    })
}

/// Every head digest the holders of `snapshot` on `island` (every island, with `None`) show.
fn heads_on(snapshot: &Snapshot, island: Option<Island>) -> BTreeSet<&str> {
    snapshot
        .holders
        .iter()
        .filter(|h| island.is_none_or(|island| h.island == island))
        .flat_map(heads_of)
        .collect()
}

/// Whether an id both islands hold at a split's end crossed it: some island did not hold it at
/// the start read, or no holder held it when the cut fell (`at_cut`), so it was made during the
/// split and reached the other island before the start read.
const fn is_crossing(at_cut: bool, start_a: bool, start_b: bool) -> bool {
    !(at_cut && start_a && start_b)
}

/// The islands an id that crossed reached: each that did not hold it at the start read (or, one
/// made after the cut that both held by then, each), but the island of its maker (`from`).
fn reached(from: Option<Island>, start_a: bool, start_b: bool) -> Vec<Island> {
    let both = start_a && start_b;
    let lacked = |island: Island| match island {
        Island::A => !start_a,
        Island::B => !start_b,
    };
    let out: Vec<Island> = [Island::A, Island::B]
        .into_iter()
        .filter(|i| (both || lacked(*i)) && from != Some(*i))
        .collect();
    if out.is_empty() {
        [Island::A, Island::B]
            .into_iter()
            .filter(|i| from != Some(*i))
            .collect()
    } else {
        out
    }
}

/// Every record and head both islands hold at `ledger`'s end that crossed its split
/// ([`is_crossing`]: one island did not hold it at the start read, or no holder held it when the cut
/// fell).
///
/// A record's plane is [`Plane::RecordsSync`] where a server of an island it reached
/// ([`reached`]: the island of its signer is the island it came from) holds it, else
/// [`Plane::Direct`]; a head's is [`Plane::Envelope`], a head being each holder's and each one
/// a server's head descends from.
#[must_use]
pub fn crossings(ledger: &Ledger) -> Vec<Crossing> {
    let at_cut = records_anywhere(&ledger.cut);
    let (start_a, start_b) = (
        records_on(&ledger.start, Island::A),
        records_on(&ledger.start, Island::B),
    );
    let (end_a, end_b) = (
        records_on(&ledger.end, Island::A),
        records_on(&ledger.end, Island::B),
    );
    let mut out = Vec::new();
    for id in end_a.intersection(&end_b) {
        let (cut, a, b) = (
            at_cut.contains(id),
            start_a.contains(id),
            start_b.contains(id),
        );
        if !is_crossing(cut, a, b) {
            continue;
        }
        let holders: Vec<&Holder> = ledger
            .end
            .holders
            .iter()
            .filter(|h| h.records.contains_key(*id))
            .collect();
        let made = holders.iter().find_map(|h| h.records.get(*id));
        let to = reached(made.and_then(|m| m.from), a, b);
        let through_server = holders.iter().any(|h| h.server && to.contains(&h.island));
        out.push(Crossing {
            what: made.map(|m| m.what.clone()).unwrap_or_default(),
            plane: if through_server {
                Plane::RecordsSync
            } else {
                Plane::Direct
            },
            on: holders.iter().map(|h| h.name.clone()).collect(),
            held_at_cut: cut,
        });
    }
    let heads_at_cut = heads_on(&ledger.cut, None);
    let (heads_start_a, heads_start_b) = (
        heads_on(&ledger.start, Some(Island::A)),
        heads_on(&ledger.start, Some(Island::B)),
    );
    let heads_end_b = heads_on(&ledger.end, Some(Island::B));
    for digest in heads_on(&ledger.end, Some(Island::A)).intersection(&heads_end_b) {
        let cut = heads_at_cut.contains(digest);
        if !is_crossing(
            cut,
            heads_start_a.contains(digest),
            heads_start_b.contains(digest),
        ) {
            continue;
        }
        let showing: Vec<&Holder> = ledger
            .end
            .holders
            .iter()
            .filter(|h| heads_of(h).any(|d| d == *digest))
            .collect();
        let what = showing
            .iter()
            .filter_map(|h| h.head.as_ref())
            .find(|m| m.digest == *digest)
            .map_or_else(
                || {
                    format!(
                        "head {} (one a later head descends from)",
                        digest.get(..8).unwrap_or(digest)
                    )
                },
                |mark| format!("head {}", mark.word()),
            );
        out.push(Crossing {
            what,
            plane: Plane::Envelope,
            on: showing.iter().map(|h| h.name.clone()).collect(),
            held_at_cut: cut,
        });
    }
    out
}

/// How many records each island made during `ledger`'s split: held at its end on that island
/// and by no holder when the cut fell.
fn made_in(ledger: &Ledger, island: Island) -> usize {
    let at_cut = records_anywhere(&ledger.cut);
    records_on(&ledger.end, island)
        .into_iter()
        .filter(|id| !at_cut.contains(id))
        .count()
}

/// Why a split is not shown to have held: what its row, F21's unjudged row and the
/// not-recorded line say, so a split that could not be read whole is never told as one that
/// leaked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Shortfall {
    /// A record or head crossed it ([`crossings`]).
    Crossed,
    /// A holder could not be read when the cut fell, at its start read or at its end, or the
    /// split was never read: nothing is seen to cross it, but that is not shown.
    NotReadWhole,
    /// The lab could not hold it or heal it ([`Ledger::failed`]).
    NotHeld,
    /// A records sync the lab ran under it did not put what its Mac made on its own island's
    /// server ([`Synced`]), so the split is not shown to have held that pass back. Nothing is
    /// seen to cross, so what the run says of the product stands ([`Shortfall::voids`]).
    Unexercised,
}

impl Shortfall {
    /// What fell short of `split` (`the cut`, `F21's split`).
    #[must_use]
    pub fn words(self, split: &str) -> String {
        match self {
            Shortfall::Crossed => format!("a record or head crossed {split}"),
            Shortfall::NotReadWhole => {
                format!("{split} could not be read whole, so it is not shown that nothing crossed")
            }
            Shortfall::NotHeld => format!("the lab could not hold or heal {split}"),
            Shortfall::Unexercised => format!(
                "a records sync under {split} did not reach its own island's server, so {split} \
                 is not shown to hold that pass back"
            ),
        }
    }

    /// It leaves the split not shown to have kept what each island made from the other: a row
    /// judged on that split is not judged, and the run is not recorded. A split whose records
    /// sync went unexercised still saw nothing cross, so it does not.
    #[must_use]
    pub const fn voids(self) -> bool {
        !matches!(self, Shortfall::Unexercised)
    }
}

/// Whether `sync` put everything its Mac made on its own island's server.
fn sync_reached(sync: &Synced) -> bool {
    sync.unread.is_none() && sync.made > 0 && sync.lacking.is_empty()
}

/// `sync`'s words: what it put on its island's server, or what fell short.
fn sync_words(sync: &Synced) -> String {
    let server = format!("island {}'s server", sync.island.word());
    let said = sync
        .said
        .as_ref()
        .map_or_else(String::new, |said| format!(" (the sync said: {said})"));
    if let Some(why) = &sync.unread {
        return format!("{}'s records sync was not read: {why}{said}", sync.mac);
    }
    if sync.made == 0 {
        return format!(
            "{} made no record during the split for its records sync to post{said}",
            sync.mac
        );
    }
    if sync.lacking.is_empty() {
        return format!(
            "{}'s records sync put the {} records it made on {server}",
            sync.mac, sync.made
        );
    }
    format!(
        "{}'s records sync left {server} lacking {} of the {} records it made ({}){said}",
        sync.mac,
        sync.lacking.len(),
        sync.made,
        sync.lacking.join(", ")
    )
}

/// Every way `ledger`'s split fell short, each once, in [`Shortfall`]'s order: none where the lab
/// held it, read every holder at each of its three reads, and nothing crossed.
#[must_use]
pub fn shortfalls(ledger: Option<&Ledger>) -> Vec<Shortfall> {
    let Some(ledger) = ledger else {
        return vec![Shortfall::NotReadWhole];
    };
    let mut out = Vec::new();
    if !crossings(ledger).is_empty() {
        out.push(Shortfall::Crossed);
    }
    if [&ledger.cut, &ledger.start, &ledger.end]
        .iter()
        .any(|s| s.holders.is_empty() || !s.unread.is_empty())
    {
        out.push(Shortfall::NotReadWhole);
    }
    if ledger.failed.is_some() {
        out.push(Shortfall::NotHeld);
    }
    if !ledger.syncs.iter().all(sync_reached) {
        out.push(Shortfall::Unexercised);
    }
    out
}

/// The check `name` on `ledger`, the split `split` names (`the cut`, `F21's split`): it holds
/// where the lab held the split, read every holder when the cut fell, at its start read and at
/// its end, found nothing that crossed ([`crossings`]), and saw each records sync it ran under
/// the split reach its own island's server ([`Synced`]); else it says each way the split fell
/// short ([`shortfalls`]).
///
/// Its negative controls are this module's tests (`swarm_partition_split_tests.rs`).
#[must_use]
pub fn judge(name: &'static str, split: &str, ledger: Option<&Ledger>) -> Check {
    let fail = |detail: String| Check {
        name,
        pass: false,
        detail,
    };
    let Some(ledger) = ledger else {
        return fail(format!(
            "{}: it was never read",
            Shortfall::NotReadWhole.words(split)
        ));
    };
    let mut wrong = Vec::new();
    for short in shortfalls(Some(ledger)) {
        let lines: Vec<String> = match short {
            Shortfall::Crossed => crossings(ledger).iter().map(|c| c.line(split)).collect(),
            Shortfall::NotReadWhole => [
                ("cut", &ledger.cut),
                ("start", &ledger.start),
                ("end", &ledger.end),
            ]
            .into_iter()
            .flat_map(|(at, snapshot)| {
                let nothing = snapshot
                    .holders
                    .is_empty()
                    .then(|| format!("nothing was read at {split}'s {at}"));
                nothing.into_iter().chain(
                    snapshot
                        .unread
                        .iter()
                        .map(move |why| format!("{why} at {split}'s {at}")),
                )
            })
            .collect(),
            Shortfall::NotHeld => ledger.failed.iter().cloned().collect(),
            Shortfall::Unexercised => ledger
                .syncs
                .iter()
                .filter(|sync| !sync_reached(sync))
                .map(sync_words)
                .collect(),
        };
        wrong.push(format!("{} ({})", short.words(split), lines.join("; ")));
    }
    if !wrong.is_empty() {
        return fail(wrong.join("; "));
    }
    let syncs = if ledger.syncs.is_empty() {
        String::new()
    } else {
        format!(
            "; the records syncs it held back: {}",
            ledger
                .syncs
                .iter()
                .map(sync_words)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    Check {
        name,
        pass: true,
        detail: format!(
            "island A made {} records and island B {} during {split} ({:.1}s); {} Macs and \
             servers read when the cut fell, {:.1}s on and at its end: nothing made during it, \
             or held on one island alone by then, reached the other, nor any head (a holder's, \
             or one a server's head descends from){syncs}",
            made_in(ledger, Island::A),
            made_in(ledger, Island::B),
            super::secs(ledger.ms),
            ledger.end.holders.len(),
            super::secs(ledger.landed_ms)
        ),
    }
}

/// `mac`'s records sync under `ledger`'s split, just done ([`Synced`]): which of the records it
/// made during the split (held by no holder when the cut fell) its island's server (`server`,
/// read from this process, which no cut lists) now lacks. `said` is what the sync said, where
/// it failed.
#[must_use]
pub fn synced(
    ledger: &Ledger,
    mac: &Watched,
    server: &str,
    signers: &Signers,
    said: Option<String>,
) -> Synced {
    let mut out = Synced {
        mac: mac.name.clone(),
        island: mac.island,
        made: 0,
        lacking: Vec::new(),
        unread: None,
        said,
    };
    let at_cut = records_anywhere(&ledger.cut);
    let held = client::load_records(&mac.paths)
        .map_err(|e| format!("{}'s records: {e}", mac.name))
        .and_then(|store| records_of(&store.encode(), signers));
    let on_server = aterm_secrets::transport::fetch_records(server, &[], None, READ, None)
        .map_err(|e| format!("island {}'s server's records: {e}", mac.island.word()))
        .and_then(|bytes| records_of(&bytes, signers));
    match (held, on_server) {
        (Ok(held), Ok(on_server)) => {
            let made: Vec<(&RecordId, &Made)> =
                held.iter().filter(|(id, _)| !at_cut.contains(id)).collect();
            out.made = made.len();
            out.lacking = made
                .into_iter()
                .filter(|(id, _)| !on_server.contains_key(*id))
                .map(|(_, m)| m.what.clone())
                .collect();
        }
        (Err(e), _) | (_, Err(e)) => out.unread = Some(e),
    }
    out
}

/// Each Mac's member id in its own log, with the Mac's name and island: who signed what. Added
/// to `signers`, so a Mac a later log leaves out keeps what an earlier read found.
fn note_signers(macs: &[Watched], signers: &mut Signers) {
    for m in macs {
        let Some(me) = client::read_machine_identity(&m.paths)
            .ok()
            .flatten()
            .map(|identity| identity.recipient())
        else {
            continue;
        };
        let Some(log) = client::load_records(&m.paths)
            .ok()
            .and_then(|store| store.log().cloned())
        else {
            continue;
        };
        for (actor, member) in log.membership().members() {
            if member.binding.recipient() == me {
                signers.insert(*actor, (m.name.clone(), m.island));
            }
        }
    }
}

/// `bytes`' records, each with what it is.
fn records_of(bytes: &[u8], signers: &Signers) -> Result<BTreeMap<RecordId, Made>, String> {
    let records = membership_log::decode_records(bytes).map_err(|e| e.to_string())?;
    Ok(records
        .iter()
        .map(|r| {
            let id = r.id();
            let signer = signers.get(r.actor());
            let short = aterm_codec::hex::encode_lower(id.get(..4).unwrap_or(&[]));
            let what = format!(
                "{}'s {:?} {short}",
                signer.map_or("an unknown signer", |(name, _)| name.as_str()),
                r.kind()
            );
            (
                *id,
                Made {
                    what,
                    from: signer.map(|(_, island)| *island),
                },
            )
        })
        .collect())
}

/// The head a Mac's `state.toml` names, read with no lock (a torn read reads as none).
fn mac_head(paths: &Paths) -> Option<HeadMark> {
    let text = crate::util::read_regular_text(&paths.dir.join("state.toml")).ok()?;
    let table = aterm_toml::from_str::<aterm_toml::Table>(&text).ok()?;
    let seq = table
        .get("seq")
        .and_then(aterm_toml::Value::as_integer)
        .and_then(|n| u64::try_from(n).ok())
        .filter(|seq| *seq > 0)?;
    let digest = table.get("digest").and_then(aterm_toml::Value::as_str)?;
    Some(HeadMark {
        seq,
        digest: digest.to_owned(),
        parents: Vec::new(),
    })
}

/// The head a server serves, as the lab reads it (never cut).
fn server_head(url: &str) -> Result<Option<HeadMark>, String> {
    let fetched =
        aterm_secrets::transport::fetch_within(url, None, READ).map_err(|e| e.to_string())?;
    let aterm_secrets::transport::Fetched::Body(bytes) = fetched else {
        return Ok(None);
    };
    let envelope = aterm_secrets::envelope::parse(&bytes).map_err(|e| e.to_string())?;
    Ok(Some(HeadMark {
        seq: envelope.seq,
        digest: aterm_codec::hex::encode_lower(&envelope.digest()),
        parents: envelope
            .parents
            .iter()
            .map(|p| aterm_codec::hex::encode_lower(p))
            .collect(),
    }))
}

/// Read every holder now: each of `macs`' own records and head, and each server's records and
/// head over HTTP from this process, which no cut lists. `signers` names who signed what, and
/// this read adds each Mac's member id to it.
#[must_use]
pub fn read(macs: &[Watched], servers: &[(Island, String)], signers: &mut Signers) -> Snapshot {
    note_signers(macs, signers);
    let mut snapshot = Snapshot::default();
    for m in macs {
        let records = client::load_records(&m.paths)
            .map_err(|e| e.to_string())
            .and_then(|store| records_of(&store.encode(), signers));
        match records {
            Ok(records) => snapshot.holders.push(Holder {
                name: m.name.clone(),
                island: m.island,
                server: false,
                records,
                head: mac_head(&m.paths),
            }),
            Err(e) => snapshot.unread.push(format!("{}'s records: {e}", m.name)),
        }
    }
    for (island, url) in servers {
        let name = format!("island {}'s server", island.word());
        let records = aterm_secrets::transport::fetch_records(url, &[], None, READ, None)
            .map_err(|e| e.to_string())
            .and_then(|bytes| records_of(&bytes, signers));
        match (records, server_head(url)) {
            (Ok(records), Ok(head)) => snapshot.holders.push(Holder {
                name,
                island: *island,
                server: true,
                records,
                head,
            }),
            (Err(e), _) => snapshot.unread.push(format!("{name}'s records: {e}")),
            (_, Err(e)) => snapshot.unread.push(format!("{name}'s head: {e}")),
        }
    }
    snapshot
}

/// The ledger's lines in a plane's report section, each led by `lead` (`F21's split`).
#[must_use]
pub fn report(lead: &str, ledger: &Ledger) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let count = |s: &Snapshot| {
        s.holders
            .iter()
            .flat_map(|h| h.records.keys())
            .collect::<BTreeSet<_>>()
            .len()
    };
    let _ = writeln!(
        out,
        "- {lead}: the lab's cut (`ATERM_DEBUG_PARTITION`) held it for {:.1}s; when it fell {} \
         Macs and servers held {} records between them, {} at its start read {:.1}s on, and \
         during it island A made {} and island B {}",
        super::secs(ledger.ms),
        ledger.cut.holders.len(),
        count(&ledger.cut),
        count(&ledger.start),
        super::secs(ledger.landed_ms),
        made_in(ledger, Island::A),
        made_in(ledger, Island::B)
    );
    for h in &ledger.end.holders {
        let _ = writeln!(
            out,
            "- {lead}'s end, {} (island {}): {} records, head {}",
            h.name,
            h.island.word(),
            h.records.len(),
            h.head
                .as_ref()
                .map_or_else(|| "none".to_owned(), HeadMark::word)
        );
    }
    for sync in &ledger.syncs {
        let _ = writeln!(out, "- {lead}: {}", sync_words(sync));
    }
    for c in crossings(ledger) {
        let _ = writeln!(out, "- {}", c.line(lead));
    }
    out
}

#[cfg(test)]
#[path = "swarm_partition_split_tests.rs"]
pub(crate) mod tests;
