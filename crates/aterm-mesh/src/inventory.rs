// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `Inventory` exchange (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.6, "No anti-entropy
//! timer", and §11 B7b): the digest exchange Commons repair runs
//! (`aterm_db::replica::exchange_inventory`), with one neighbour, over an `Inventory` stream on its
//! `atm/1` link. Each side lists the digests it holds of the membership log and of each table
//! either holds, and the missing records move in batches of [`crate::ingest::BATCH`] into the
//! stores' own checks, the membership log first so a writer's seat lands before its operations. A
//! record that enters this way is never published again by this Mac.
//!
//! Each table moves its conflict proofs first (Commons repair's conflicts phase, with its fork
//! rule: a proof the neighbour holds the fork proof of is not sent), then its operations. The
//! tables both hold go first, then each table one side holds and the other does not: the other
//! gets every record of it where its stores take the table ([`Stores::takes`]), so a Mac that never
//! held a table gets it here, one first written and cut from every log while it was away included.
//! The read right is the seat: every current seat opens every Commons table (no descriptor names
//! readers, and a table's key derives from the Commons network key, which the fold derives from
//! the `[everyone]` epoch key), and both sides exchange only with a current seat
//! ([`Mesh::admits`]), as the relay's `/t/>` grant serves only seats. Going after the tables
//! both hold, a table only one side holds reaches a vault whose catalog it just declared: the
//! declaration moved with the catalog, which every Mac holds, and subscribes that vault at once.
//! A ring window's table (claims, recoveries, the journal), which no declaration names, the vault
//! takes up when it is offered one, as Commons repair does (`Replica::mesh_takes`).
//!
//! It runs only on an event, never on a timer ([`Reason`]):
//! - **a gap notice**: a relay read that its neighbour's cursor fell below what `tables.log` keeps
//!   (the cut passed it), so the neighbour missed records the relay can no longer send;
//! - **a lineage change**: a relay resumed onto a later generation of its neighbour's log, which
//!   may have lost the last records it held;
//! - **a new link**: a relay with no cursor (a neighbour never relayed to, rebuilt, renewed, or
//!   silent for 7 days, or a new Mac whose stores are empty) starts at the log's head, and what
//!   lies before it moves here;
//! - **the membership log**: the ingest consumer's waiting set is full, or its group lost records;
//! - **the digest trigger** (PF5b's finding H1): a neighbour's presence rows show a digest of a
//!   store an exchange moves other than this Mac's own latest row's, two rows in a row, while both
//!   relays are caught up (`relay_digest::digest_asks`, [`Inventory::digest_row`]): the membership
//!   log's, or the lab tables', which names their operations and their conflict records. A record
//!   that entered this Mac by an exchange is in no log, so no relay carries it on; this is what
//!   does, hop by hop, whichever unit brought it.
//!
//! A relay start that asked a lineage or new link's exchange while this Mac's ingest had not yet
//! passed the last table record its log held asks it once more when the ingest does
//! ([`Inventory::ask_once_ingested`], `relay_live::asks_again`): the first exchange can run before
//! this Mac's stores hold what its log held at the start, and move nothing, with nothing else
//! asking again (PF5b's finding H2).
//!
//! After an exchange that moved nothing, the pair does not exchange again until this Mac's digest
//! changes, and the digest trigger asks once for each pair of digests it reads, again only after
//! an exchange with that neighbour failed. An exchange whose received records wait for a
//! membership record runs again once one lands; one whose records found a store busy runs again
//! after a doubling back-off.
//!
//! **`NO_INVENTORY_ON_GAP`** (`no-inventory-on-gap`) is the gap rule's negative control: a gap
//! notice starts no exchange, so a record cut below a sleeping Mac's cursor reaches it only by
//! the digest trigger, where presence runs (in a lab without presence, never).
//! **`NO_DIGEST_TRIGGER`** (`no-digest-trigger`) is the digest trigger's: a neighbour's rows ask
//! nothing, so a record that reached a Mac by `Inventory` stays one hop from where it was cut.
//! **`NO_SECOND_ASK`** (`no-second-ask`) is the second ask's (the island's `noSecondAsk`): a
//! start's exchange is never asked again, so a record its stores had not yet ingested never
//! reaches a neighbour whose relay started at the head.
//! **`BOTH_HOLD_ONLY`** (`both-hold-only`) is the one-sided tables' (the rule before §11 B8): only
//! the tables both hold move, so a table a Mac never held never reaches it here.
//!
//! **The wire.** Both sides first send `atm-inv 2` and their summary digest (over every digest
//! they would list); equal summaries end the exchange. Then each table list, then per unit (the
//! membership log, then each table both listed, then each table one listed, in order, a table's
//! proofs before its operations) the key lists, then the initiator's batches (a `u32` count, that
//! many length-prefixed records, a zero count to end), then the responder's. The unit list is a
//! function of the two table lists alone, so both sides walk it in step; for a table it does not
//! take, a side sends no key list (`exchange_inventory`'s unavailable), and is sent nothing.

use crate::egress::Egress;
use crate::logs::Mesh;
use crate::presence::Held;
use crate::relay::{self, Event};
use crate::stores::{Digest, Kind, Stores, Verdict};
use aterm_mesh_core::relay_digest::Trigger;
use aterm_mesh_core::relay_live;
use aterm_net::fleet::FleetOp;
use aterm_net::fleet::link::{Bell, BusLog, Incoming, Link, LinkStream};
use aterm_types::MutexExt;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

/// The greeting's head. `atm-inv 1` walked only the tables both sides hold, a prefix of this
/// unit list: a pair of the two would move those and then break off where the old side ended
/// early, so each refuses the other's greeting instead.
const HEAD: &[u8] = b"atm-inv 2";

/// The most tables a list holds (`aterm_db::replica::MAX_TABLES`).
const TABLES_MAX: usize = 64;

/// The most digests one table's operation list holds.
const DIGESTS_MAX: usize = aterm_db::replica::MAX_RECORDS;

/// The most keys one table's proof list holds.
const PROOFS_MAX: usize = aterm_db::replica::MAX_PROOFS;

/// The most ids the membership log's list holds: every record it may hold.
const MEMBERS_MAX: usize = aterm_secrets::membership_log::MAX_HELD_RECORDS;

/// The first wait before an exchange whose records found a store busy runs again.
const BUSY_FIRST: Duration = Duration::from_millis(100);

/// The longest such wait.
const BUSY_MAX: Duration = Duration::from_secs(10);

/// The most bytes one record may take on the wire.
const RECORD_MAX: usize = 4 * 1024 * 1024;

/// What started an exchange.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Reason {
    /// A relay's neighbour fell below what `tables.log` keeps.
    Gap,
    /// A relay resumed onto a later generation of its neighbour's log.
    Lineage,
    /// A relay with no cursor: a new neighbour, or one rebuilt, renewed or silent for 7 days.
    NewLink,
    /// The ingest consumer needs the membership log: its waiting set is full.
    Members,
    /// The ingest consumer's group lost records.
    Lost,
    /// A neighbour's presence rows showed a membership or tables digest other than this Mac's,
    /// two rows in a row, while both relays were caught up (the digest trigger).
    Digest,
}

/// What a presence row says its Mac holds of each store an exchange moves: the digest of its
/// membership log and of its lab tables (their operations and conflict records), `None` where
/// the row does not know it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Holds {
    pub(crate) membership: Option<Held>,
    pub(crate) tables: Option<Held>,
}

/// The digest trigger's memory of one neighbour's presence rows: a fold of each store's digests
/// (the island's `Mem` for each), and a row asks where either asks. So a lasting difference in one
/// store is asked whether the other's digest is unknown, moving or in step.
#[derive(Default)]
struct Triggers {
    membership: Trigger<Held>,
    tables: Trigger<Held>,
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The negative control of the gap rule (`no-inventory-on-gap`): while set on the thread that
    /// opens a mesh, its gap notices start no `Inventory`. A lab instance turns it on for its
    /// whole process with `no-inventory-on-gap` in `ATERM_TEST_NEGATIVE_CONTROL`.
    pub static NO_INVENTORY_ON_GAP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether [`NO_INVENTORY_ON_GAP`] is on for a mesh opened on this thread.
#[cfg(any(test, debug_assertions))]
fn no_inventory_on_gap() -> bool {
    NO_INVENTORY_ON_GAP.with(std::cell::Cell::get)
        || aterm_types::negative_control::on("no-inventory-on-gap")
}

#[cfg(not(any(test, debug_assertions)))]
const fn no_inventory_on_gap() -> bool {
    false
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The negative control of the digest trigger (`no-digest-trigger`): while set on the thread
    /// that opens a mesh, a neighbour's presence rows ask no `Inventory`, so a record that reached
    /// this Mac by one goes no further (PF5b's `h1_hole`). A lab instance turns it on for its whole
    /// process with `no-digest-trigger` in `ATERM_TEST_NEGATIVE_CONTROL`.
    pub static NO_DIGEST_TRIGGER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether [`NO_DIGEST_TRIGGER`] is on for a mesh opened on this thread.
#[cfg(any(test, debug_assertions))]
fn no_digest_trigger() -> bool {
    NO_DIGEST_TRIGGER.with(std::cell::Cell::get)
        || aterm_types::negative_control::on("no-digest-trigger")
}

#[cfg(not(any(test, debug_assertions)))]
const fn no_digest_trigger() -> bool {
    false
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The negative control of the second ask (`no-second-ask`): while set on the thread that opens
    /// a mesh, a relay start's exchange is never asked again once the ingest passes the last table
    /// record its log held. A lab instance turns it on for its whole process with
    /// `no-second-ask` in `ATERM_TEST_NEGATIVE_CONTROL`.
    pub static NO_SECOND_ASK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether [`NO_SECOND_ASK`] is on for a mesh opened on this thread.
#[cfg(any(test, debug_assertions))]
fn no_second_ask() -> bool {
    NO_SECOND_ASK.with(std::cell::Cell::get) || aterm_types::negative_control::on("no-second-ask")
}

#[cfg(not(any(test, debug_assertions)))]
const fn no_second_ask() -> bool {
    false
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The negative control of the one-sided tables (`both-hold-only`): while set on the thread
    /// that opens a mesh, its exchanges walk only the tables both sides hold, so a table a Mac
    /// never held reaches it by no `Inventory`. Both sides of a lab's exchange read one thread's
    /// flag, so they walk one unit list. A lab instance turns it on for its whole process with
    /// `both-hold-only` in `ATERM_TEST_NEGATIVE_CONTROL`.
    pub static BOTH_HOLD_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether [`BOTH_HOLD_ONLY`] is on for a mesh opened on this thread.
#[cfg(any(test, debug_assertions))]
fn both_hold_only() -> bool {
    BOTH_HOLD_ONLY.with(std::cell::Cell::get) || aterm_types::negative_control::on("both-hold-only")
}

#[cfg(not(any(test, debug_assertions)))]
const fn both_hold_only() -> bool {
    false
}

/// What one exchange moved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Moved {
    /// Records this Mac sent.
    pub sent: usize,
    /// Records this Mac received and its stores applied.
    pub received: usize,
    /// Records received that wait for a membership record.
    pub waiting: usize,
    /// Records received that found their store busy.
    pub busy: usize,
}

impl Moved {
    fn nothing(self) -> bool {
        self.sent == 0 && self.received == 0 && self.waiting == 0 && self.busy == 0
    }

    fn add(&mut self, other: Self) {
        self.sent += other.sent;
        self.received += other.received;
        self.waiting += other.waiting;
        self.busy += other.busy;
    }
}

/// The `Inventory` exchanges of one mesh: what is asked, the links to ask over, and the pairs that
/// last moved nothing.
pub struct Inventory {
    stores: Arc<dyn Stores>,
    egress: Arc<Egress>,
    queue: Mutex<VecDeque<(String, Weak<Link>, Reason)>>,
    bell: Arc<Bell>,
    /// Each neighbour an exchange with moved nothing, and this Mac's summary then.
    quiet: Mutex<BTreeMap<String, Digest>>,
    /// The live link to each neighbour a relay follows.
    links: Mutex<BTreeMap<String, Weak<Link>>>,
    /// The neighbours whose last exchange left records waiting for a membership record: asked
    /// again once one lands ([`Self::members_landed`]).
    waiting: Mutex<BTreeSet<String>>,
    /// The neighbours whose last exchange left records a busy store did not take: asked again
    /// after [`Self::busy_wait`].
    busy: Mutex<BTreeSet<String>>,
    /// Each neighbour whose ask has no link to run over (the link it was asked over closed before
    /// it ran, or its exchange broke off on the link it ran over, and no other live link is
    /// registered): asked again over the next link a relay follows ([`Self::linked`]). A duplicate
    /// that replaces a new link before its start's ask ran leaves the relay resuming on the
    /// replacement with its cursor, which asks nothing itself (`relay_live::start_repair`).
    parked: Mutex<BTreeMap<String, Reason>>,
    /// The digest trigger's memory of each neighbour's presence rows ([`Self::digest_row`]).
    digests: Mutex<BTreeMap<String, Triggers>>,
    /// The neighbours a relay start asks again once this Mac's ingest floor passes the last table
    /// record its log held then, and why ([`Self::ask_once_ingested`]).
    after_ingest: Mutex<BTreeMap<String, (u64, Reason)>>,
    no_gap: bool,
    no_digest: bool,
    no_second_ask: bool,
    /// Whether exchanges walk only the tables both sides hold ([`BOTH_HOLD_ONLY`]).
    both_hold_only: bool,
    /// Whether the worker runs an exchange now, and its change's notice.
    inflight: (Mutex<bool>, std::sync::Condvar),
    stop: AtomicBool,
    /// The negative control of "not repeated": an exchange that moved nothing is forgotten
    /// ([`Self::quiet_off_for_test`]).
    #[cfg(any(test, debug_assertions))]
    quiet_off: AtomicBool,
}

impl std::fmt::Debug for Inventory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inventory").finish_non_exhaustive()
    }
}

impl Inventory {
    pub(crate) fn new(stores: Arc<dyn Stores>, egress: Arc<Egress>) -> Arc<Self> {
        Arc::new(Self {
            stores,
            egress,
            queue: Mutex::new(VecDeque::new()),
            bell: Arc::new(Bell::default()),
            quiet: Mutex::new(BTreeMap::new()),
            links: Mutex::new(BTreeMap::new()),
            waiting: Mutex::new(BTreeSet::new()),
            busy: Mutex::new(BTreeSet::new()),
            parked: Mutex::new(BTreeMap::new()),
            digests: Mutex::new(BTreeMap::new()),
            after_ingest: Mutex::new(BTreeMap::new()),
            no_gap: no_inventory_on_gap(),
            no_digest: no_digest_trigger(),
            no_second_ask: no_second_ask(),
            both_hold_only: both_hold_only(),
            inflight: (Mutex::new(false), std::sync::Condvar::new()),
            stop: AtomicBool::new(false),
            #[cfg(any(test, debug_assertions))]
            quiet_off: AtomicBool::new(false),
        })
    }

    /// A relay to `peer` follows its log over `link`, which runs the ask parked for `peer`. A link
    /// already closed registers nothing: a relay on a link a duplicate replaced can get here after
    /// the replacement's relay did, and would hide the live link from every later ask.
    pub(crate) fn linked(&self, peer: &str, link: &Arc<Link>) {
        let parked = {
            let mut links = self.links.lock_or_recover();
            if link.closed().is_some() {
                return;
            }
            let weak = Arc::downgrade(link);
            if let Some(slot) = links.get_mut(peer) {
                *slot = weak;
            } else {
                links.insert(peer.to_owned(), weak);
            }
            self.parked.lock_or_recover().remove(peer)
        };
        if let Some(reason) = parked {
            self.request(peer, link, reason);
        }
    }

    /// The live link registered for `peer` in `links`, unless it is `ended`.
    fn live(
        links: &BTreeMap<String, Weak<Link>>,
        peer: &str,
        ended: Option<&Link>,
    ) -> Option<Arc<Link>> {
        links.get(peer).and_then(Weak::upgrade).filter(|link| {
            link.closed().is_none() && ended.is_none_or(|ended| !std::ptr::eq(link.as_ref(), ended))
        })
    }

    /// The live link registered for `peer`, parking nothing where there is none: the digest
    /// trigger's lookup, whose row asks only while a relay follows ([`Self::digest_row`]).
    fn link_to(&self, peer: &str) -> Option<Arc<Link>> {
        Self::live(&self.links.lock_or_recover(), peer, None)
    }

    /// The live link registered for `peer`, unless it is `ended` (the link an ask for `reason`
    /// broke off on). With none, the ask is parked for `peer`'s next link: the check and the park
    /// hold the links, so a link [`Self::linked`] registers after the check runs it.
    fn relink(&self, peer: &str, ended: Option<&Link>, reason: Reason) -> Option<Arc<Link>> {
        let links = self.links.lock_or_recover();
        let newer = Self::live(&links, peer, ended);
        if newer.is_none() {
            self.parked
                .lock_or_recover()
                .entry(peer.to_owned())
                .or_insert(reason);
        }
        newer
    }

    /// Ask for an exchange with `peer` over `link`, for `reason`. A gap notice under
    /// [`NO_INVENTORY_ON_GAP`] asks nothing (`relay_live::requests`).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "MeshRelay",
            action = "GapSkip",
            project = "aterm_mesh::tests::tables::relay_conformance::mesh_relay_transition"
        )
    )]
    pub fn request(&self, peer: &str, link: &Arc<Link>, reason: Reason) {
        if !relay_live::requests(reason == Reason::Gap, self.no_gap) {
            return;
        }
        let mut queue = self.queue.lock_or_recover();
        if let Some(asked) = queue.iter_mut().find(|(p, _, _)| p == peer) {
            // One exchange covers both; it runs over the newest link.
            asked.1 = Arc::downgrade(link);
            return;
        }
        queue.push_back((peer.to_owned(), Arc::downgrade(link), reason));
        drop(queue);
        self.bell.ring();
    }

    /// `peer`'s presence row advanced here: what it holds `theirs`, against `mine`, this Mac's own
    /// latest row's, while this Mac's tables relay to it `follows` or not. Ask an exchange with it
    /// where the digest trigger asks for either store (`relay_digest::digest_asks`, over the link
    /// that relay follows on): the store's two digests differ, both known; they are the ones its
    /// last row was compared under (two rows in a row, neither Mac's store moving in between, so
    /// neither relay carried anything that changed it); the relay follows; and the trigger has
    /// not asked for this pair since an exchange with `peer` failed. Both stores' records travel
    /// on `tables.log`, so one relay's following stands for both.
    pub(crate) fn digest_row(&self, peer: &str, theirs: Holds, mine: Holds, follows: bool) {
        let link = follows.then(|| self.link_to(peer)).flatten();
        let asks = {
            let mut digests = self.digests.lock_or_recover();
            let triggers = match digests.get_mut(peer) {
                Some(triggers) => triggers,
                None => digests.entry(peer.to_owned()).or_default(),
            };
            let (follows, off) = (link.is_some(), self.no_digest);
            // Both folds read every row, whatever the other answers.
            let membership =
                triggers
                    .membership
                    .row(theirs.membership, mine.membership, follows, off);
            let tables = triggers
                .tables
                .row(theirs.tables, mine.tables, follows, off);
            membership || tables
        };
        if let Some(link) = link.filter(|_| asks) {
            self.request(peer, &link, Reason::Digest);
        }
    }

    /// An exchange with `peer` did not run to its end: the digest trigger may ask the pairs it
    /// last asked for again.
    fn not_run(&self, peer: &str) {
        if let Some(triggers) = self.digests.lock_or_recover().get_mut(peer) {
            triggers.membership.failed();
            triggers.tables.failed();
        }
    }

    /// A relay start asked an exchange with `peer` for `reason` while this Mac's ingest had not
    /// passed `last`, the last table record its log held then (`relay_live::asks_again`): ask it
    /// again once the ingest floor passes `last`. A later start's replaces it. The caller reads the
    /// floor after this and hands it to [`Self::ingested`], so a commit between the two is not
    /// missed. Under [`NO_SECOND_ASK`] nothing is asked again.
    pub(crate) fn ask_once_ingested(&self, peer: &str, reason: Reason, last: u64) {
        if self.no_second_ask {
            return;
        }
        let mut after_ingest = self.after_ingest.lock_or_recover();
        if let Some(slot) = after_ingest.get_mut(peer) {
            *slot = (last, reason);
        } else {
            after_ingest.insert(peer.to_owned(), (last, reason));
        }
    }

    /// Whether a relay start's second ask waits for the ingest ([`Self::ask_once_ingested`]).
    pub(crate) fn awaits_ingest(&self) -> bool {
        !self.after_ingest.lock_or_recover().is_empty()
    }

    /// This Mac's ingest floor is `floor`: its stores hold every record of `tables.log` below it.
    /// Ask each exchange a relay start left for it ([`Self::ask_once_ingested`]). One whose
    /// neighbour has no live link now stays, due, for the next start over a new one to ask.
    pub(crate) fn ingested(&self, floor: u64) {
        let due: BTreeMap<String, (u64, Reason)> = {
            let mut after = self.after_ingest.lock_or_recover();
            if !after.values().any(|(last, _)| *last < floor) {
                return;
            }
            let (due, wait) = std::mem::take(&mut *after)
                .into_iter()
                .partition(|(_, (last, _))| *last < floor);
            *after = wait;
            due
        };
        for (peer, held) in due {
            match self.link_to(&peer) {
                Some(link) => self.request(&peer, &link, held.1),
                None => {
                    self.after_ingest
                        .lock_or_recover()
                        .entry(peer)
                        .or_insert(held);
                }
            }
        }
    }

    /// The ingest consumer's waiting set is full: exchange with one neighbour a relay follows.
    pub(crate) fn members_due(&self) {
        let any = self
            .links
            .lock_or_recover()
            .iter()
            .find_map(|(peer, link)| link.upgrade().map(|link| (peer.clone(), link)));
        if let Some((peer, link)) = any {
            self.quiet.lock_or_recover().remove(&peer);
            self.request(&peer, &link, Reason::Members);
        }
    }

    /// A membership record landed: exchange again with each neighbour whose records waited.
    pub(crate) fn members_landed(&self) {
        let peers = std::mem::take(&mut *self.waiting.lock_or_recover());
        for peer in peers {
            if let Some(link) = self.relink(&peer, None, Reason::Members) {
                self.request(&peer, &link, Reason::Members);
            }
        }
    }

    /// The ingest consumer's group lost records: exchange with every neighbour a relay follows.
    pub(crate) fn lost(&self) {
        let links: Vec<(String, Arc<Link>)> = self
            .links
            .lock_or_recover()
            .iter()
            .filter_map(|(peer, link)| link.upgrade().map(|link| (peer.clone(), link)))
            .collect();
        for (peer, link) in links {
            self.quiet.lock_or_recover().remove(&peer);
            self.request(&peer, &link, Reason::Lost);
        }
    }

    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.bell.ring();
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// The worker's body: run each exchange asked, one at a time, until stopped.
    pub(crate) fn run(self: &Arc<Self>, mesh: &Weak<Mesh>) {
        let mut busy_wait = BUSY_FIRST;
        while !self.stopped() {
            self.settle_flight();
            let next = {
                let mut inflight = self.inflight.0.lock_or_recover();
                let next = self.queue.lock_or_recover().pop_front();
                *inflight = next.is_some();
                next
            };
            let Some((peer, link, reason)) = next else {
                if self.busy.lock_or_recover().is_empty() {
                    busy_wait = BUSY_FIRST;
                    self.bell.wait(None);
                } else {
                    // The back-off runs out unless a request or a stop comes first.
                    let until = Instant::now() + busy_wait;
                    loop {
                        let left = until.saturating_duration_since(Instant::now());
                        if left.is_zero()
                            || self.stopped()
                            || !self.queue.lock_or_recover().is_empty()
                        {
                            break;
                        }
                        self.bell.wait(Some(left));
                    }
                    if self.stopped() || !self.queue.lock_or_recover().is_empty() {
                        continue;
                    }
                    busy_wait = busy_wait.saturating_mul(2).min(BUSY_MAX);
                    for peer in std::mem::take(&mut *self.busy.lock_or_recover()) {
                        if let Some(link) = self.relink(&peer, None, Reason::Members) {
                            self.request(&peer, &link, Reason::Members);
                        }
                    }
                }
                continue;
            };
            let Some(mesh) = mesh.upgrade() else { break };
            if mesh.is_stopped() {
                break;
            }
            let Some(link) = link
                .upgrade()
                .filter(|link| link.closed().is_none())
                .or_else(|| self.relink(&peer, None, reason))
            else {
                self.not_run(&peer);
                continue;
            };
            self.initiate(&mesh, &peer, &link, reason);
        }
        self.settle_flight();
    }

    /// The worker holds no exchange now.
    fn settle_flight(&self) {
        *self.inflight.0.lock_or_recover() = false;
        self.inflight.1.notify_all();
    }

    /// Wait until no exchange is queued or running, or `deadline`: whether none is. An ask parked
    /// for a link waits for that link, not for the worker, so it leaves the worker idle.
    #[cfg(test)]
    pub(crate) fn wait_idle_for_test(&self, deadline: std::time::Instant) -> bool {
        let mut inflight = self.inflight.0.lock_or_recover();
        loop {
            if !*inflight && self.queue.lock_or_recover().is_empty() {
                return true;
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            inflight = self
                .inflight
                .1
                .wait_timeout(inflight, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    /// Exchange with `peer` over `link` as the initiator, unless the last exchange with it moved
    /// nothing and this Mac's summary has not changed since (`relay_live::runs`, whose island
    /// proves the skip safe: the neighbour holds whatever this Mac holds then, of every table its
    /// stores take), or `peer` is not a current seat here: an ask queued or parked before its
    /// removal sends it nothing, as [`Mesh::serve_inventory`] answers no such key.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "MeshRelay",
            action = "InventoryAB",
            project = "aterm_mesh::tests::tables::relay_conformance::mesh_relay_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "MeshRelay",
            action = "InventoryCB",
            project = "aterm_mesh::tests::tables::relay_conformance::mesh_relay_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "MeshRelay",
            action = "InventoryAD",
            project = "aterm_mesh::tests::tables::relay_conformance::mesh_relay_transition"
        )
    )]
    fn initiate(&self, mesh: &Mesh, peer: &str, link: &Link, reason: Reason) {
        if mesh.admits(link.peer().key.bytes()) != Some(true) {
            aterm_log::info!("mesh: no Inventory with {peer}, which is not a current seat here");
            return;
        }
        let summary = match summary(self.stores.as_ref()) {
            Ok(summary) => summary,
            Err(e) => {
                aterm_log::warn!("mesh: an Inventory with {peer} could not read the stores: {e}");
                self.not_run(peer);
                return;
            }
        };
        let quiet_same = self.quiet.lock_or_recover().get(peer) == Some(&summary);
        if !relay_live::runs(self.remembers(), quiet_same) {
            mesh.report(&Event::InventorySkipped {
                peer: peer.to_owned(),
                reason,
            });
            return;
        }
        let ran = link
            .open(FleetOp::Inventory, Some(BusLog::Tables), relay::IO_WITHIN)
            .and_then(|stream| {
                stream.set_read_timeout(Some(relay::IO_WITHIN));
                stream.set_write_timeout(Some(relay::IO_WITHIN));
                exchange(
                    self.stores.as_ref(),
                    &self.egress,
                    &stream,
                    summary,
                    true,
                    self.both_hold_only,
                )
            });
        match ran {
            Ok(moved) => {
                self.settled(peer, summary, moved);
                mesh.report(&Event::Inventory {
                    peer: peer.to_owned(),
                    reason: Some(reason),
                    sent: moved.sent,
                    received: moved.received,
                });
            }
            Err(e) => {
                aterm_log::info!("mesh: an Inventory with {peer} ended: {e}");
                self.not_run(peer);
                // The exchange did not finish: ask it again over a newer link, or the next one.
                if let Some(next) = self.relink(peer, Some(link), reason) {
                    self.request(peer, &next, reason);
                }
            }
        }
    }

    /// Forget every exchange that moved nothing: the negative control of "not repeated".
    #[cfg(any(test, debug_assertions))]
    pub fn quiet_off_for_test(&self, on: bool) {
        self.quiet_off.store(on, Ordering::Release);
    }

    #[cfg(any(test, debug_assertions))]
    fn remembers(&self) -> bool {
        !self.quiet_off.load(Ordering::Acquire)
    }

    #[cfg(not(any(test, debug_assertions)))]
    const fn remembers(&self) -> bool {
        true
    }

    fn settled(&self, peer: &str, summary: Digest, moved: Moved) {
        {
            let mut quiet = self.quiet.lock_or_recover();
            if moved.nothing() && self.remembers() {
                if let Some(slot) = quiet.get_mut(peer) {
                    *slot = summary;
                } else {
                    quiet.insert(peer.to_owned(), summary);
                }
            } else {
                quiet.remove(peer);
            }
        }
        if moved.waiting > 0 {
            let mut waiting = self.waiting.lock_or_recover();
            if !waiting.contains(peer) {
                waiting.insert(peer.to_owned());
            }
        }
        if moved.busy > 0 {
            let mut busy = self.busy.lock_or_recover();
            if !busy.contains(peer) {
                busy.insert(peer.to_owned());
            }
            drop(busy);
            self.bell.ring();
        }
    }

    /// Answer an `Inventory` stream `peer` opened.
    pub(crate) fn answer(&self, mesh: &Mesh, peer: &str, stream: &LinkStream) {
        let ran = summary(self.stores.as_ref()).and_then(|summary| {
            stream.set_read_timeout(Some(relay::IO_WITHIN));
            stream.set_write_timeout(Some(relay::IO_WITHIN));
            exchange(
                self.stores.as_ref(),
                &self.egress,
                stream,
                summary,
                false,
                self.both_hold_only,
            )
            .map(|moved| (summary, moved))
        });
        match ran {
            Ok((summary, moved)) => {
                self.settled(peer, summary, moved);
                mesh.report(&Event::Inventory {
                    peer: peer.to_owned(),
                    reason: None,
                    sent: moved.sent,
                    received: moved.received,
                });
            }
            Err(e) => aterm_log::info!("mesh: an Inventory {peer} opened ended: {e}"),
        }
    }
}

impl Mesh {
    /// Serve an `Inventory` stream a neighbour opened on its link: refused `unserved` where this
    /// mesh moves no tables, `unread` before the standings were read, `out` for a key that is not
    /// a current seat; otherwise answered.
    pub fn serve_inventory(&self, incoming: Incoming) {
        let Some(inventory) = self.inventory() else {
            return incoming.refuse("unserved");
        };
        if incoming.log() != Some(BusLog::Tables) || self.is_stopped() {
            return incoming.refuse("unserved");
        }
        let key = *incoming.peer().key.bytes();
        match self.admits(&key) {
            None => return incoming.refuse("unread"),
            Some(false) => return incoming.refuse("out"),
            Some(true) => {}
        }
        let peer_buf = crate::logs::principal_buf(&key);
        let peer = crate::logs::principal_str(&peer_buf);
        let stream = incoming.accept();
        inventory.answer(self, peer, &stream);
    }
}

/// One unit of the exchange: the membership log, one table's conflict proofs, or one table's
/// operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Unit {
    Members,
    Proofs(Digest),
    Table(Digest),
}

/// This Mac's summary: the SHA-256 over the membership log's complete records' ids, then each
/// table's digest, its operations' ids and its proofs' keys, all sorted.
fn summary(stores: &dyn Stores) -> io::Result<Digest> {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm/mesh/inventory/1\0");
    let members = stores.members_inventory()?;
    hash.update((members.len() as u64).to_le_bytes());
    for id in &members {
        hash.update(id);
    }
    for (table, ids, proofs) in stores.tables_summary()? {
        hash.update(table);
        hash.update((ids.len() as u64).to_le_bytes());
        for id in &ids {
            hash.update(id);
        }
        hash.update((proofs.len() as u64).to_le_bytes());
        for key in &proofs {
            hash.update(key);
        }
    }
    Ok(hash.finalize())
}

fn io_of(error: &aterm_db::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.0.clone())
}

fn write_frame(mut stream: &LinkStream, bytes: &[u8]) -> io::Result<()> {
    let len = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a frame past 4 GiB"))?;
    if let Some(total) = bytes.len().checked_add(4).filter(|&n| n <= 1024) {
        let mut buf = [0u8; 1024];
        buf[..4].copy_from_slice(&len.to_le_bytes());
        buf[4..total].copy_from_slice(bytes);
        return stream.write_all(&buf[..total]);
    }
    stream.write_all(&len.to_le_bytes())?;
    stream.write_all(bytes)
}

fn read_u32(mut stream: &LinkStream) -> io::Result<u32> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len)?;
    Ok(u32::from_le_bytes(len))
}

fn read_greeting(mut stream: &LinkStream) -> io::Result<Digest> {
    let max = HEAD.len() + 32;
    let len = read_u32(stream)? as usize;
    if len > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("an Inventory frame of {len} bytes, past {max}"),
        ));
    }
    if len != max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the neighbour speaks another Inventory",
        ));
    }
    let mut greeting = [0u8; HEAD.len() + 32];
    stream.read_exact(&mut greeting)?;
    if !greeting.starts_with(HEAD) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the neighbour speaks another Inventory",
        ));
    }
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&greeting[HEAD.len()..]);
    Ok(digest)
}

fn read_frame(stream: &LinkStream, max: usize) -> io::Result<Vec<u8>> {
    let len = read_u32(stream)? as usize;
    if len > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("an Inventory frame of {len} bytes, past {max}"),
        ));
    }
    let mut bytes = vec![0u8; len];
    let mut reader = stream;
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// The key list exchange of one unit (or of the table lists), in Commons repair's encoding: `None`
/// where this side sends no list (a table its stores do not take), and the neighbour's list, or
/// `None` where it sent none.
fn exchange_ids<const N: usize>(
    stream: &LinkStream,
    initiator: bool,
    local: Option<&BTreeSet<[u8; N]>>,
    maximum: usize,
) -> io::Result<Option<BTreeSet<[u8; N]>>> {
    let side = if initiator {
        aterm_db::replica::Side::Initiator
    } else {
        aterm_db::replica::Side::Responder
    };
    let mut io = stream;
    aterm_db::replica::exchange_inventory::<N>(&mut io, side, local, maximum, || Ok(()))
        .map_err(|e| io_of(&e))
}

/// Whether this side sends a key list for `table`: it holds the table (it listed it), or its
/// stores take it now, as a vault does a table whose declaration this exchange's catalog unit
/// just brought.
fn opens(stores: &dyn Stores, mine: &BTreeSet<Digest>, table: Digest) -> io::Result<bool> {
    Ok(mine.contains(&table) || stores.takes(table)?)
}

/// What a side gives of a unit: the keys it holds that the neighbour's list lacks, by `lacks`;
/// nothing where either side sent no list.
fn lacking<const N: usize>(
    local: Option<&BTreeSet<[u8; N]>>,
    remote: Option<&BTreeSet<[u8; N]>>,
    lacks: impl Fn(&BTreeSet<[u8; N]>, &[u8; N]) -> bool,
) -> Vec<[u8; N]> {
    match (local, remote) {
        (Some(local), Some(remote)) => local
            .iter()
            .filter(|key| lacks(remote, key))
            .copied()
            .collect(),
        _ => Vec::new(),
    }
}

/// Run one exchange over `stream` with this Mac's `summary`, as the initiator or the responder;
/// with `both_hold_only` ([`BOTH_HOLD_ONLY`]) over the tables both sides hold alone.
fn exchange(
    stores: &dyn Stores,
    egress: &Egress,
    stream: &LinkStream,
    summary: Digest,
    initiator: bool,
    both_hold_only: bool,
) -> io::Result<Moved> {
    let mut greeting = [0u8; HEAD.len() + 32];
    greeting[..HEAD.len()].copy_from_slice(HEAD);
    greeting[HEAD.len()..].copy_from_slice(&summary);
    let theirs = if initiator {
        write_frame(stream, &greeting)?;
        read_greeting(stream)?
    } else {
        let theirs = read_greeting(stream)?;
        write_frame(stream, &greeting)?;
        theirs
    };
    let mut moved = Moved::default();
    if theirs == summary {
        return Ok(moved);
    }
    let mine = stores.tables()?;
    let their_tables =
        exchange_ids(stream, initiator, Some(&mine), TABLES_MAX)?.unwrap_or_default();
    // One unit list, a function of the two lists alone, so both sides walk it in step: the
    // membership log, each table both listed, then each table one listed. The catalog, which
    // every vault holds, goes before the tables it declares to a vault that lacked them.
    let mut tables: Vec<Digest> = mine.intersection(&their_tables).copied().collect();
    if !both_hold_only {
        tables.extend(mine.symmetric_difference(&their_tables).copied());
    }
    let mut units = Vec::with_capacity(1 + 2 * tables.len());
    units.push(Unit::Members);
    for table in tables {
        units.extend([Unit::Proofs(table), Unit::Table(table)]);
    }
    for unit in units {
        let wires = match unit {
            Unit::Members => {
                let local = stores.members_inventory()?;
                let remote = exchange_ids(stream, initiator, Some(&local), MEMBERS_MAX)?;
                let give = lacking(Some(&local), remote.as_ref(), |remote, id| {
                    !remote.contains(id)
                });
                if give.is_empty() {
                    Vec::new()
                } else {
                    stores.members_wires(&give)?
                }
            }
            Unit::Proofs(table) => {
                let local = if opens(stores, &mine, table)? {
                    Some(stores.table_proofs(table)?.unwrap_or_default())
                } else {
                    None
                };
                let remote = exchange_ids(stream, initiator, local.as_ref(), PROOFS_MAX)?;
                let give = lacking(local.as_ref(), remote.as_ref(), |remote, key| {
                    let mut fork = [0u8; 81];
                    fork[..48].copy_from_slice(&key[..48]);
                    fork[48] = 1;
                    !remote.contains(key) && !remote.contains(&fork)
                });
                if give.is_empty() {
                    Vec::new()
                } else {
                    stores.table_proof_wires(table, &give)?
                }
            }
            Unit::Table(table) => {
                let local = if opens(stores, &mine, table)? {
                    Some(stores.table_inventory(table)?.unwrap_or_default())
                } else {
                    None
                };
                let remote = exchange_ids(stream, initiator, local.as_ref(), DIGESTS_MAX)?;
                let give = lacking(local.as_ref(), remote.as_ref(), |remote, id| {
                    !remote.contains(id)
                });
                if give.is_empty() {
                    Vec::new()
                } else {
                    stores.table_wires(table, &give)?
                }
            }
        };
        if initiator {
            moved.sent += send(stream, &wires)?;
            moved.add(receive(stores, egress, stream, unit)?);
        } else {
            moved.add(receive(stores, egress, stream, unit)?);
            moved.sent += send(stream, &wires)?;
        }
    }
    Ok(moved)
}

/// Send `wires` in batches, then an empty batch: how many went.
fn send(stream: &LinkStream, wires: &[Vec<u8>]) -> io::Result<usize> {
    let mut writer = stream;
    if wires.is_empty() {
        writer.write_all(&0u32.to_le_bytes())?;
        writer.flush()?;
        return Ok(0);
    }
    let cap = wires
        .iter()
        .take(crate::ingest::BATCH)
        .map(|w| 4 + w.len())
        .sum::<usize>()
        .saturating_add(8)
        .min(aterm_net::fleet::link::FRAME_MAX);
    let mut buf = Vec::with_capacity(cap);
    for batch in wires.chunks(crate::ingest::BATCH) {
        if buf.len() + 4 > aterm_net::fleet::link::FRAME_MAX {
            writer.write_all(&buf)?;
            buf.clear();
        }
        buf.extend_from_slice(&(batch.len() as u32).to_le_bytes());
        for wire in batch {
            let len = u32::try_from(wire.len())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a frame past 4 GiB"))?;
            if buf.len() + 4 + wire.len() <= aterm_net::fleet::link::FRAME_MAX {
                buf.extend_from_slice(&len.to_le_bytes());
                buf.extend_from_slice(wire);
            } else {
                if buf.len() + 4 > aterm_net::fleet::link::FRAME_MAX {
                    writer.write_all(&buf)?;
                    buf.clear();
                }
                buf.extend_from_slice(&len.to_le_bytes());
                writer.write_all(&buf)?;
                buf.clear();
                writer.write_all(wire)?;
            }
        }
    }
    if buf.len() + 4 > aterm_net::fleet::link::FRAME_MAX {
        writer.write_all(&buf)?;
        buf.clear();
    }
    buf.extend_from_slice(&0u32.to_le_bytes());
    writer.write_all(&buf)?;
    writer.flush()?;
    Ok(wires.len())
}

/// Receive `unit`'s batches into the stores: what they applied, and what waits or found them
/// busy.
fn receive(
    stores: &dyn Stores,
    egress: &Egress,
    stream: &LinkStream,
    unit: Unit,
) -> io::Result<Moved> {
    let mut moved = Moved::default();
    loop {
        let n = read_u32(stream)? as usize;
        if n == 0 {
            return Ok(moved);
        }
        if n > crate::ingest::BATCH {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("an Inventory batch of {n} records"),
            ));
        }
        let mut wires = Vec::with_capacity(n);
        for _ in 0..n {
            wires.push(read_frame(stream, RECORD_MAX)?);
        }
        let verdicts = match unit {
            Unit::Members => egress.delivering(&wires, || stores.ingest_members(&wires)),
            Unit::Proofs(table) => egress.delivering(&wires, || {
                stores.ingest_table(table, Kind::Conflict, &wires)
            }),
            Unit::Table(table) => egress.delivering(&wires, || {
                stores.ingest_table(table, Kind::Operation, &wires)
            }),
        };
        for verdict in verdicts {
            match verdict {
                Verdict::Applied => moved.received += 1,
                Verdict::Waiting(_) => moved.waiting += 1,
                Verdict::Busy(_) => moved.busy += 1,
                Verdict::Refused(_) => {}
            }
        }
    }
}
