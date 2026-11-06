// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ingest consumer (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.6, "Tables arrive through one
//! consumer", and §11 B7b): one consumer group on `tables.log` ([`GROUP`]) that hands each record
//! another Mac originated to its store's own checks ([`Stores::ingest_table`],
//! [`Stores::ingest_members`]), so a write on one Mac projects on every other with no repair
//! round.
//!
//! - A record whose writer is not yet a seat in this Mac's view ([`Verdict::Waiting`]) waits, by
//!   offset, and is tried again after each membership record this Mac applies and after each
//!   change of the membership log's standings ([`Ingest::kick`]). The waiting set is bounded by
//!   the table's record budget ([`WAITING_MAX`]); when it is full the consumer stops taking
//!   records and an `Inventory` exchange of the membership log starts.
//! - The group commits only past records that were applied, refused for good, or set waiting;
//!   one the store was too busy to take ([`Verdict::Busy`]) holds the commit below it and is tried
//!   again. The waiting offsets are kept in [`WAITING_FILE`], so a restart reads them again from
//!   `tables.log`, which the cut keeps ([`Ingest::floor`]).
//! - A record this Mac originated is its own: stepped past.
//!
//! **The cut** (§4.6 "Disk"). After a commit, `tables.log` is cut, carrying its state, below the
//! lowest of the group's position, the first waiting offset, and the cursor of each neighbour
//! relayed to within 7 days; a cursor that would keep the log above [`CUT_BYTES`] is dropped, and
//! that neighbour catches up through `Inventory` when its relay reads the gap.

use crate::egress::Egress;
use crate::inventory::Inventory;
use crate::logs::{Mesh, MeshLog};
use crate::relay::{self, Event};
use crate::stores::{self, Carries, Kind, Stores, Verdict};
use astream_broker::{AnyClient, Closer};
use aterm_net::fleet::link::Bell;
use aterm_types::MutexExt;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

/// The ingest consumer's group on `tables.log`.
pub const GROUP: &str = "/ingest/tables";

/// The prefix the group sits under: the grant that commits it.
pub const GROUP_PREFIX: &str = "/ingest";

/// The most records of one table one store call takes (Commons repair's batch).
pub const BATCH: usize = 16;

/// The most records that wait at once: the table's record budget (`aterm_db::replica::MAX_RECORDS`).
pub const WAITING_MAX: usize = 8192;

/// The file beside `tables.log` that keeps the waiting records' offsets.
pub const WAITING_FILE: &str = "waiting";

/// The most deliveries the reader holds ahead of the worker before it stops reading.
const QUEUE_MAX: usize = 1024;

/// The most deliveries the worker takes at once.
const TAKE: usize = 256;

/// The cut runs once the floor is this many records past the log's base.
pub const CUT_STEP: u64 = 4096;

/// The bytes past which a neighbour's cursor no longer holds the cut back.
pub const CUT_BYTES: u64 = 256 * 1024 * 1024;

/// The first wait before a busy store is tried again, doubled to [`BUSY_MAX`].
const BUSY_FIRST: Duration = Duration::from_millis(100);

/// The longest wait before a busy store is tried again.
const BUSY_MAX: Duration = Duration::from_secs(10);

/// The first wait before a tail that failed is opened again.
const TAIL_FIRST: Duration = Duration::from_millis(100);

/// One record off `tables.log`.
#[derive(Clone, Debug)]
struct Delivered {
    offset: u64,
    /// `None` for a record this consumer steps past (this Mac's own, or one that names no store).
    record: Option<(Carries, Vec<u8>)>,
}

/// What the worker holds.
#[derive(Default)]
struct State {
    /// The records that wait for a membership record, by offset.
    waiting: BTreeMap<u64, (Carries, Vec<u8>)>,
    /// The records a busy store did not take, by offset.
    busy: BTreeMap<u64, (Carries, Vec<u8>)>,
    /// The highest offset the worker settled or set aside.
    through: Option<u64>,
    /// The highest offset committed.
    committed: Option<u64>,
    /// The offsets [`WAITING_FILE`] names that were not read back yet: kept on disk and below
    /// the cut until a read finds them.
    unread: BTreeSet<u64>,
    /// The offsets last written to [`WAITING_FILE`].
    written: BTreeSet<u64>,
    /// Whether the full waiting set asked for an `Inventory` of the membership log.
    asked: bool,
}

/// The ingest consumer of one mesh.
pub struct Ingest {
    stores: Arc<dyn Stores>,
    egress: Arc<Egress>,
    inventory: Arc<Inventory>,
    dir: PathBuf,
    queue: Mutex<VecDeque<Delivered>>,
    room: Condvar,
    /// The worker's bell: deliveries, marks, kicks and stop. Only the worker waits on it.
    bell: Arc<Bell>,
    /// The reader's bell between tails: stop. Only the reader waits on it.
    retry: Bell,
    state: Mutex<State>,
    /// Notified each time the group commits.
    committed: Condvar,
    kicked: AtomicBool,
    stop: AtomicBool,
    closer: Mutex<Option<Closer>>,
    /// The negative control of the waiting rule: a waiting record is never tried again
    /// ([`Self::no_retry_for_test`]).
    #[cfg(any(test, debug_assertions))]
    no_retry: AtomicBool,
}

impl std::fmt::Debug for Ingest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ingest").finish_non_exhaustive()
    }
}

/// The grants the consumer holds on its own broker: reading every table subject, and committing
/// its group.
fn grants(mesh: &Mesh) -> io::Result<Vec<astream_broker::Capability>> {
    let hosted = mesh.hosted(MeshLog::Tables);
    Ok(vec![
        hosted.mint(&format!("ro:{}", MeshLog::Tables.filter()))?,
        hosted.mint(&format!("rw,p={}:{GROUP_PREFIX}/>", mesh.principal()))?,
    ])
}

/// A client of `tables.log` under this Mac's key holding the consumer's grants, and its closer.
fn client(mesh: &Mesh, within: Option<Duration>) -> io::Result<(AnyClient, Closer)> {
    let hosted = mesh.hosted(MeshLog::Tables);
    let link = hosted.adopt(mesh.key())?;
    link.set_read_timeout(within)?;
    link.set_write_timeout(within)?;
    let (mut client, closer) = astream_broker::connect_stream(link);
    let caps = grants(mesh)?;
    client.attach_all(caps.iter().map(|cap| (cap.filter.as_str(), &cap.tag[..])))?;
    Ok((client, closer))
}

/// One table's consecutive records of one kind, by offset: what one `ingest_table` call checks.
type Run = ([u8; 32], Kind, Vec<(u64, Vec<u8>)>);

impl Ingest {
    pub(crate) fn new(
        stores: Arc<dyn Stores>,
        egress: Arc<Egress>,
        inventory: Arc<Inventory>,
        dir: &std::path::Path,
    ) -> Arc<Self> {
        Arc::new(Self {
            stores,
            egress,
            inventory,
            dir: dir.to_path_buf(),
            queue: Mutex::new(VecDeque::new()),
            room: Condvar::new(),
            bell: Arc::new(Bell::default()),
            retry: Bell::default(),
            state: Mutex::new(State::default()),
            committed: Condvar::new(),
            kicked: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            closer: Mutex::new(None),
            #[cfg(any(test, debug_assertions))]
            no_retry: AtomicBool::new(false),
        })
    }

    /// The membership log's standings changed: try the waiting records again.
    pub fn kick(&self) {
        self.kicked.store(true, Ordering::Release);
        self.bell.ring();
    }

    /// Never try a waiting record again: the negative control of the waiting rule.
    #[cfg(any(test, debug_assertions))]
    pub fn no_retry_for_test(&self, on: bool) {
        self.no_retry.store(on, Ordering::Release);
    }

    #[cfg(any(test, debug_assertions))]
    fn retries(&self) -> bool {
        !self.no_retry.load(Ordering::Acquire)
    }

    #[cfg(not(any(test, debug_assertions)))]
    const fn retries(&self) -> bool {
        true
    }

    /// The records waiting now.
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.state.lock_or_recover().waiting.len()
    }

    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(closer) = self.closer.lock_or_recover().as_ref() {
            closer.close();
        }
        self.bell.ring();
        self.retry.ring();
        // Under the queue's lock, so a reader between its check of the flag and its wait for
        // room cannot miss the notice.
        drop(self.queue.lock_or_recover());
        self.room.notify_all();
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// The reader's body: tail the group, handing each delivery to the worker, opening the tail
    /// again after it fails, until stopped.
    pub(crate) fn read(self: &Arc<Self>, mesh: &Weak<Mesh>) {
        let mut backoff = TAIL_FIRST;
        while !self.stopped() {
            let tailed = self.tail(mesh);
            if self.stopped() || mesh.upgrade().is_none_or(|mesh| mesh.is_stopped()) {
                break;
            }
            if let Err(e) = tailed {
                aterm_log::info!("mesh: the ingest tail ended ({e}); it opens again");
            }
            self.retry.wait(Some(backoff));
            backoff = backoff.saturating_mul(2).min(BUSY_MAX);
        }
    }

    /// Tail the group. The mesh is held only to open the client: a mesh dropped without a
    /// close still drops, and its close ends this tail by its closer.
    fn tail(&self, mesh: &Weak<Mesh>) -> io::Result<()> {
        let (mut client, closer, me) = {
            let Some(mesh) = mesh.upgrade() else {
                return Ok(());
            };
            if mesh.is_stopped() {
                return Ok(());
            }
            let (client, closer) = client(&mesh, None)?;
            (client, closer, mesh.principal().to_owned())
        };
        {
            let mut held = self.closer.lock_or_recover();
            if self.stopped() {
                return Ok(());
            }
            *held = Some(closer);
        }
        client.burst_marks()?;
        let mut sub = client.subscribe_group(GROUP, MeshLog::Tables.filter())?;
        loop {
            let event = sub.recv_event()?;
            match event {
                Some(astream_broker::Event::Delivery {
                    offset,
                    subject,
                    body,
                }) => {
                    let record = stores::carries(&subject)
                        .filter(|(_, owner)| *owner != me)
                        .and_then(|(carries, _)| {
                            astream_broker::origin::payload(&body)
                                .map(|payload| (carries, payload.to_vec()))
                        });
                    let mut queue = self.queue.lock_or_recover();
                    while queue.len() >= QUEUE_MAX && !self.stopped() {
                        self.bell.ring();
                        queue = self
                            .room
                            .wait(queue)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                    queue.push_back(Delivered { offset, record });
                }
                Some(astream_broker::Event::Mark { .. }) => {
                    // The head moved: a neighbour's relay commits its group on this log, which
                    // a push waits on ([`Mesh::push_table`]).
                    if let Some(mesh) = mesh.upgrade() {
                        mesh.acked();
                    }
                    self.bell.ring();
                }
                Some(astream_broker::Event::Gap(gap)) => {
                    // The cut never passes the group's position: a gap here means the log was
                    // renewed beneath it. What it held reaches this Mac by `Inventory`.
                    aterm_log::warn!(
                        "mesh: the ingest group lost records {}..{} of tables.log",
                        gap.from,
                        gap.to
                    );
                    self.inventory.lost();
                }
                None => return Ok(()),
            }
            if self.stopped() {
                return Ok(());
            }
            self.bell.ring();
        }
    }

    /// The worker's body: settle what the reader delivered, try the waiting and busy records again
    /// when a membership record lands or a store frees, and commit, until stopped.
    pub(crate) fn work(self: &Arc<Self>, mesh: &Weak<Mesh>) {
        self.load();
        let mut committer: Option<AnyClient> = None;
        let mut busy_wait = BUSY_FIRST;
        while !self.stopped() {
            let Some(mesh) = mesh.upgrade() else { break };
            if mesh.is_stopped() {
                break;
            }
            if committer.is_none() {
                committer = match client(&mesh, Some(relay::IO_WITHIN)) {
                    Ok((client, _)) => Some(client),
                    Err(e) => {
                        aterm_log::info!("mesh: the ingest consumer could not commit yet: {e}");
                        None
                    }
                };
            }
            if let Some(reader) = committer.as_mut()
                && !self.state.lock_or_recover().unread.is_empty()
                && self.reload(&mesh, reader).is_err()
            {
                committer = None;
            }
            let mut landed = self.kicked.swap(false, Ordering::AcqRel);
            let full = self.state.lock_or_recover().waiting.len() >= WAITING_MAX;
            if !full {
                let took: Vec<Delivered> = {
                    let mut queue = self.queue.lock_or_recover();
                    let n = queue.len().min(TAKE);
                    queue.drain(..n).collect()
                };
                self.room.notify_all();
                if !took.is_empty() {
                    landed |= self.settle(&mesh, took);
                }
            } else if !std::mem::replace(&mut self.state.lock_or_recover().asked, true) {
                aterm_log::warn!(
                    "mesh: {WAITING_MAX} table records wait for membership records; an Inventory \
                     of the membership log starts"
                );
                self.inventory.members_due();
            }
            if landed {
                self.inventory.members_landed();
            }
            while landed {
                landed = self.retry_waiting(&mesh);
            }
            let busy_now = !self.state.lock_or_recover().busy.is_empty();
            if busy_now {
                self.retry_busy(&mesh);
            }
            self.commit(&mesh, committer.as_mut());
            let more =
                !self.queue.lock_or_recover().is_empty() && !self.state.lock_or_recover().asked;
            drop(mesh);
            if more {
                continue;
            }
            let pending = {
                let state = self.state.lock_or_recover();
                !state.busy.is_empty() || !state.unread.is_empty()
            } || committer.is_none();
            if !pending {
                busy_wait = BUSY_FIRST;
                self.bell.wait(None);
            } else {
                self.bell.wait(Some(busy_wait));
                busy_wait = busy_wait.saturating_mul(2).min(BUSY_MAX);
            }
        }
    }

    /// Hand `took` to the stores: the membership records first, then each table's in runs of at
    /// most [`BATCH`]. Answers whether a membership record was applied.
    fn settle(&self, mesh: &Mesh, took: Vec<Delivered>) -> bool {
        let mut members: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut tables: Vec<(u64, [u8; 32], Vec<u8>)> = Vec::new();
        let mut through = None;
        for delivered in took {
            through = through.max(Some(delivered.offset));
            match delivered.record {
                Some((Carries::Members, payload)) => members.push((delivered.offset, payload)),
                Some((Carries::Table(table), payload)) => {
                    tables.push((delivered.offset, table, payload));
                }
                None => {}
            }
        }
        let landed = self.ingest_members(mesh, members);
        self.ingest_tables(mesh, tables);
        let mut state = self.state.lock_or_recover();
        state.through = state.through.max(through);
        landed
    }

    /// Ingest membership records: whether one was applied.
    fn ingest_members(&self, mesh: &Mesh, records: Vec<(u64, Vec<u8>)>) -> bool {
        if records.is_empty() {
            return false;
        }
        let (offsets, wires): (Vec<u64>, Vec<Vec<u8>>) = records.into_iter().unzip();
        let verdicts = self
            .egress
            .delivering(&wires, || self.stores.ingest_members(&wires));
        let mut landed = false;
        let (mut applied, mut waiting, mut refused) = (0, 0, 0);
        let mut state = self.state.lock_or_recover();
        for ((offset, record), verdict) in offsets.into_iter().zip(wires).zip(verdicts) {
            state.waiting.remove(&offset);
            state.busy.remove(&offset);
            match verdict {
                Verdict::Applied => {
                    applied += 1;
                    landed = true;
                }
                Verdict::Waiting(_) => {
                    waiting += 1;
                    state.waiting.insert(offset, (Carries::Members, record));
                }
                Verdict::Refused(why) => {
                    refused += 1;
                    aterm_log::info!("mesh: a membership record at {offset} is refused: {why}");
                }
                Verdict::Busy(_) => {
                    state.busy.insert(offset, (Carries::Members, record));
                }
            }
        }
        drop(state);
        mesh.report(&Event::Ingested {
            members: true,
            applied,
            waiting,
            refused,
        });
        landed
    }

    /// Ingest table records, each table's consecutive records of one kind in runs of at most
    /// [`BATCH`].
    fn ingest_tables(&self, mesh: &Mesh, records: Vec<(u64, [u8; 32], Vec<u8>)>) {
        let mut runs: Vec<Run> = Vec::new();
        for (offset, table, payload) in records {
            let Some((kind, wire)) = stores::read_table_payload(&payload) else {
                let mut state = self.state.lock_or_recover();
                state.waiting.remove(&offset);
                state.busy.remove(&offset);
                aterm_log::info!("mesh: a table record at {offset} names no kind: refused");
                continue;
            };
            match runs.last_mut() {
                Some((t, k, run)) if *t == table && *k == kind && run.len() < BATCH => {
                    run.push((offset, wire.to_vec()));
                }
                _ => runs.push((table, kind, vec![(offset, wire.to_vec())])),
            }
        }
        for (table, kind, run) in runs {
            let (offsets, wires): (Vec<u64>, Vec<Vec<u8>>) = run.into_iter().unzip();
            let verdicts = self
                .egress
                .delivering(&wires, || self.stores.ingest_table(table, kind, &wires));
            let (mut applied, mut waiting, mut refused) = (0, 0, 0);
            let mut state = self.state.lock_or_recover();
            for ((offset, wire), verdict) in offsets.into_iter().zip(wires).zip(verdicts) {
                state.waiting.remove(&offset);
                state.busy.remove(&offset);
                match verdict {
                    Verdict::Applied => applied += 1,
                    Verdict::Waiting(_) => {
                        waiting += 1;
                        state.waiting.insert(
                            offset,
                            (Carries::Table(table), stores::table_payload(kind, &wire)),
                        );
                    }
                    Verdict::Refused(why) => {
                        refused += 1;
                        aterm_log::debug!("mesh: a table record at {offset} is refused: {why}");
                    }
                    Verdict::Busy(_) => {
                        state.busy.insert(
                            offset,
                            (Carries::Table(table), stores::table_payload(kind, &wire)),
                        );
                    }
                }
            }
            drop(state);
            mesh.report(&Event::Ingested {
                members: false,
                applied,
                waiting,
                refused,
            });
        }
    }

    /// Try every waiting record again: whether a membership record was applied.
    fn retry_waiting(&self, mesh: &Mesh) -> bool {
        if !self.retries() {
            return false;
        }
        let waiting: Vec<(u64, (Carries, Vec<u8>))> = {
            let state = self.state.lock_or_recover();
            state.waiting.iter().map(|(o, r)| (*o, r.clone())).collect()
        };
        if waiting.is_empty() {
            return false;
        }
        let before = waiting.len();
        let landed = self.again(mesh, waiting);
        let mut state = self.state.lock_or_recover();
        if state.waiting.len() < before {
            state.asked = false;
        }
        landed
    }

    fn retry_busy(&self, mesh: &Mesh) {
        let busy: Vec<(u64, (Carries, Vec<u8>))> = {
            let state = self.state.lock_or_recover();
            state.busy.iter().map(|(o, r)| (*o, r.clone())).collect()
        };
        if self.again(mesh, busy) {
            self.bell.ring();
            self.kicked.store(true, Ordering::Release);
        }
    }

    fn again(&self, mesh: &Mesh, records: Vec<(u64, (Carries, Vec<u8>))>) -> bool {
        let mut members = Vec::new();
        let mut tables = Vec::new();
        for (offset, (carries, payload)) in records {
            match carries {
                Carries::Members => members.push((offset, payload)),
                Carries::Table(table) => tables.push((offset, table, payload)),
            }
        }
        let landed = self.ingest_members(mesh, members);
        self.ingest_tables(mesh, tables);
        landed
    }

    /// The lowest offset still unsettled (busy), or one past what was settled.
    fn commit_upto(state: &State) -> Option<u64> {
        match state.busy.keys().next() {
            Some(first) => first.checked_sub(1),
            None => state.through,
        }
    }

    /// Commit the group past what was settled, write the waiting offsets where they changed, and
    /// cut the log where its floor moved far enough.
    fn commit(&self, mesh: &Mesh, committer: Option<&mut AnyClient>) {
        let (upto, waiting) = {
            let state = self.state.lock_or_recover();
            // Every offset not yet settled for good: a busy record may sit below the group's
            // position (one that waited, then found its store busy), and an unread one is not
            // known yet.
            let waiting: BTreeSet<u64> = state
                .waiting
                .keys()
                .chain(state.busy.keys())
                .chain(state.unread.iter())
                .copied()
                .collect();
            let upto =
                Self::commit_upto(&state).filter(|upto| state.committed.is_none_or(|c| *upto > c));
            (upto, (waiting != state.written).then_some(waiting))
        };
        // The waiting offsets go to disk before the commit passes them.
        if let Some(waiting) = waiting {
            use std::fmt::Write as _;
            let mut text = String::with_capacity(waiting.len() * 12);
            for o in &waiting {
                let _ = writeln!(text, "{o}");
            }
            match aterm_uds::private_file::write_private(&self.dir, WAITING_FILE, text.as_bytes()) {
                Ok(()) => self.state.lock_or_recover().written = waiting,
                Err(e) => {
                    aterm_log::warn!("mesh: the ingest's {WAITING_FILE} could not be written: {e}");
                    return;
                }
            }
        }
        let (Some(upto), Some(committer)) = (upto, committer) else {
            return;
        };
        match committer.commit(GROUP, upto) {
            Ok(_) => {
                self.state.lock_or_recover().committed = Some(upto);
                self.committed.notify_all();
                self.cut(mesh);
            }
            Err(e) => aterm_log::info!("mesh: the ingest group could not commit {upto}: {e}"),
        }
    }

    /// The lowest offset `tables.log` must keep for this consumer: its group's position, or the
    /// first waiting, busy or unread record's.
    pub(crate) fn floor(&self, mesh: &Mesh) -> u64 {
        let start = mesh.broker(MeshLog::Tables).group_start(GROUP);
        let state = self.state.lock_or_recover();
        [
            state.waiting.keys().next(),
            state.busy.keys().next(),
            state.unread.iter().next(),
        ]
        .into_iter()
        .flatten()
        .copied()
        .fold(start, u64::min)
    }

    /// Wait until the group committed through `offset`, or `deadline`: whether it did.
    #[cfg(test)]
    pub(crate) fn wait_committed_for_test(
        &self,
        offset: u64,
        deadline: std::time::Instant,
    ) -> bool {
        let mut state = self.state.lock_or_recover();
        loop {
            if state.committed.is_some_and(|c| c >= offset) {
                return true;
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            state = self
                .committed
                .wait_timeout(state, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    /// Cut `tables.log`, carrying its state, below this consumer's floor and the cursors of the
    /// neighbours relayed to within 7 days, once that floor is [`CUT_STEP`] past its base.
    fn cut(&self, mesh: &Mesh) {
        let broker = mesh.broker(MeshLog::Tables);
        let threshold = broker.base().saturating_add(CUT_STEP);
        let mut floor = self.floor(mesh).min(broker.visible_head());
        if floor < threshold {
            return;
        }
        let file = mesh.log_file(MeshLog::Tables);
        let big = std::fs::metadata(&file).is_ok_and(|meta| meta.len() > CUT_BYTES);
        if !big {
            let peers = mesh
                .hosted(MeshLog::Tables)
                .relayed
                .lock_or_recover()
                .recent(relay::wall_ms(&mesh.wall));
            for peer in peers {
                floor = floor.min(broker.group_start(&relay::group_of(&peer)));
            }
        }
        if floor < threshold {
            return;
        }
        match broker.retain_before_carrying_state(floor) {
            Ok(dropped) => aterm_log::info!(
                "mesh: tables.log is cut below {floor} ({dropped} records){}",
                if big {
                    "; the neighbours behind it catch up by Inventory"
                } else {
                    ""
                }
            ),
            Err(e) => aterm_log::warn!("mesh: tables.log could not be cut below {floor}: {e}"),
        }
    }

    /// Take the offsets [`WAITING_FILE`] names as unread: kept on disk and below the cut until
    /// [`Self::reload`] reads them back.
    fn load(&self) {
        let Ok(bytes) = aterm_uds::private_file::read_private(&self.dir, WAITING_FILE, 1 << 20)
        else {
            return;
        };
        let offsets: BTreeSet<u64> = String::from_utf8_lossy(&bytes)
            .lines()
            .filter_map(|line| line.parse().ok())
            .collect();
        let mut state = self.state.lock_or_recover();
        state.written.clone_from(&offsets);
        state.unread = offsets;
    }

    /// Read the unread offsets back from `tables.log` with `reader`: each found becomes a
    /// waiting record again, each the log no longer holds (or this Mac's own) is dropped, and
    /// the first read that fails keeps the rest unread.
    fn reload(&self, mesh: &Mesh, reader: &mut AnyClient) -> io::Result<()> {
        let unread: Vec<u64> = self
            .state
            .lock_or_recover()
            .unread
            .iter()
            .copied()
            .collect();
        let me = mesh.principal().to_owned();
        let mut found = 0;
        for offset in unread {
            let (page, _) = reader.fetch(offset, MeshLog::Tables.filter(), 1)?;
            let record = page
                .into_iter()
                .filter(|(at, _, _)| *at == offset)
                .find_map(|(_, subject, body)| {
                    let (carries, owner) = stores::carries(&subject)?;
                    (owner != me).then_some(())?;
                    astream_broker::origin::payload(&body)
                        .map(|payload| (carries, payload.to_vec()))
                });
            let mut state = self.state.lock_or_recover();
            state.unread.remove(&offset);
            if let Some(record) = record {
                state.waiting.insert(offset, record);
                found += 1;
            }
        }
        if found > 0 {
            aterm_log::info!("mesh: {found} table records wait again for membership records");
            self.kick();
        }
        Ok(())
    }
}
