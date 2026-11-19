// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The egress (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.6, "Tables arrive through one
//! consumer", and §11 B7b): every record that enters a store from this Mac, a local write or a
//! record an older peer delivered by an old path, is published to `tables.log` under this Mac's
//! owner segment ([`crate::stores::table_subject`], [`crate::stores::members_subject`]). A record
//! that came in through the relay or `Inventory` is never published again by this Mac.
//!
//! **The feed is an append hook, never a rescan.** A table store hands each durable append to the
//! process's sink (`aterm_db::table::store::set_append_sink`), and the membership log each durable
//! write of its records file (`aterm_secrets::membership_log::on_written`); in the node service
//! both feed [`Egress::feed`] at once, with the record. An append made in another process (a CLI's
//! `db` or `members` write) sends the node service a [`Wake`] on its wake socket ([`WAKE_SOCKET`])
//! naming the table or the log and its new length, and the egress reads the store from its cursor
//! up to that length. No timer reads a store.
//!
//! **Cursors.** Per table store, how many of its records this Mac has published, and for the
//! membership log how many of its complete records, kept in [`CURSORS_FILE`] beside `tables.log`
//! and written after each publish. At each start the egress publishes what lies past each cursor.
//! On the first start of all (no cursor file) it publishes no history: each store's cursor starts
//! at its length, and what the other Macs lack of it reaches them by `Inventory`, which a relay
//! with no cursor starts ([`crate::inventory`]).
//!
//! **A rewrite of the membership log.** The members cursor is a count of the log's complete
//! records in held order, and a rewrite of the records file (a prune at a final checkpoint, a
//! shed of superseded bodies: `aterm_secrets` tracker FL-148) drops records and so moves every
//! one after them. The cursor is kept under the log's rewrite generation
//! ([`Stores::members_generation`]: its number and which rewrite made it; the `g` line of
//! [`CURSORS_FILE`]) with the id of the last record it stepped past
//! ([`Stores::member_id`]), and each time the egress reads the cursor it reads the generation
//! first: where it differs the cursor is placed again ([`MembersCursor`]), just past that
//! record where the rewritten log still holds it, so the egress publishes exactly what it had
//! not, and from the log's first record where it does not (the record was dropped), which
//! publishes duplicates the stores answer by digest and loses nothing. Only a Mac past records
//! format 1 rewrites its records file, and its members lane is shut
//! (`aterm_secrets::client::members_lane_open`), so every generation this egress feeds under
//! is 0, no cursor is placed again and nothing more or less is published than before. The cursor file itself gains its `g` line once a record
//! names the cursor's place (at the first catch-up, or the first record stepped past): a
//! build that does not know the line skips it and writes a file without it, which this build
//! reads as a cursor no record names yet.
//!
//! Not closed here: a vault left and another joined, neither ever rewritten, are both at
//! generation 0 with no rewrite to tell them apart, and nothing removes the cursor file when
//! a vault is left. The cursor then stays at its count of the vault that was left, as it
//! always has, and what the new log holds below it leaves this Mac only by `Inventory`. Once
//! either was rewritten their generations differ and the cursor is placed again.
//!
//! **Provenance.** Every record the mesh delivers (by the relay or by `Inventory`) is noted in
//! [`Egress::arrived`] before its store is written, and the note is on disk ([`ARRIVED_FILE`])
//! before the write. The egress steps past each noted record once: by its append's feed (a table
//! record's also says `aterm_db::table::store::Via::Mesh`), or, where that feed was dropped from a
//! full queue or the node stopped before the egress read it, by the catch-up that reads it from its
//! store. Only records the node service itself ingests come in by the mesh, so a record another
//! process appended is always this Mac's to publish. A crash between a publish and its cursor's
//! write publishes the record a second time at the next start: a duplicate every store answers by
//! its digest.
//!
//! **What the tables hold.** The egress keeps the ids of the operations and of the conflict
//! records each primary table store holds: at its first catch-up it reads each store's records
//! once, and after that it adds each record its cursor passes, the mesh's included. This Mac's
//! presence row reads them ([`Egress::tables_held`]), so the row reads no store.

use crate::logs::{Mesh, MeshLog};
use crate::stores::{self, Digest, Generation, Held, Kind, Stores, TableKey};
use aterm_net::fleet::link::Bell;
use aterm_types::MutexExt;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

/// The file beside `tables.log` that keeps the egress's cursors.
pub const CURSORS_FILE: &str = "egress";

/// The ids of the records the mesh delivered that the egress has not yet stepped past, one lowercase
/// hex digest a line, beside [`CURSORS_FILE`] ([`Egress::arrived`]): what tells a catch-up that a
/// record its store holds came by the mesh, after a feed was dropped or the node stopped between a
/// store's append and the egress reading it.
pub const ARRIVED_FILE: &str = "arrived";

/// The most ids [`ARRIVED_FILE`] keeps. An id whose record never reached its store (a duplicate a
/// store answered without an append, or a stop before the append) is dropped past it, so the worst
/// a full file costs is one record published a second time, which every store answers by its
/// digest.
const ARRIVED_MAX: usize = 65_536;

/// The wake socket in `<state_dir>/mesh/`: a datagram socket another process's store write wakes
/// the egress on.
pub const WAKE_SOCKET: &str = "wake";

/// The most records one read of a store hands the egress at once.
const READ_CHUNK: u64 = 256;

/// The most feeds the egress holds before a sink's feed is dropped (the store's cursor catches it
/// up at the next feed or start).
const QUEUE_MAX: usize = 65_536;

/// The first wait after a publish the local broker refused, doubled to [`RETRY_MAX`].
const RETRY_FIRST: Duration = Duration::from_millis(100);

/// The longest wait between two tries of a publish the local broker refused.
const RETRY_MAX: Duration = Duration::from_secs(30);

/// The longest a wake datagram may be.
const WAKE_MAX: usize = 256;

/// What a store tells the egress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Feed {
    /// A table store's length moved to `length`; `record` is the one appended at `length - 1`
    /// where the append was this process's, with whether the mesh delivered it.
    Table {
        /// The store.
        key: TableKey,
        /// Its length now.
        length: u64,
        /// The record appended, and whether it came by the mesh.
        record: Option<(Held, bool)>,
    },
    /// The membership log holds `length` complete records; `records` are those a write of this
    /// process's handed over.
    Members {
        /// Its complete records' count now.
        length: usize,
        /// The records the write handed over, where it was this process's: taken only where
        /// the write was made under the generation the cursor counts ([`MembersCursor`]).
        records: Option<Handed>,
    },
}

/// The records a write of the membership log handed the egress ([`Feed::Members`]): its
/// complete records from an index on, in the held order of the log as that write wrote it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handed {
    /// The index of the first of them in that order: the cursor as the write's hook read it.
    pub from: usize,
    /// The rewrite generation the write was made under
    /// (`aterm_secrets::membership_log::Written::generation`), which names that order.
    pub generation: Generation,
    /// The records.
    pub records: Vec<Vec<u8>>,
}

/// The egress's place in the membership log: how many of its complete records, in held order,
/// the egress has stepped past, and the log's rewrite generation ([`Stores::members_generation`])
/// that count is of.
///
/// A rewrite drops records, so the count holds only under its generation. Decided here:
/// - **The generation is read before the cursor is.** A feed or a catch-up reads the log's
///   generation first, and a cursor placed under another is placed again before any record is
///   published or skipped by it: a write after a rewrite would otherwise read as published (its
///   log is shorter than the cursor) and never leave this Mac but by `Inventory`. A feed whose
///   length is no more than the cursor is answered only after that read, so a generation that
///   cannot be read fails the feed, which is tried again, as a store that cannot be read does.
/// - **The generation is the rewrite, not only its number** ([`Generation`]). A rewrite whose
///   commit a power cut took back, after this egress placed its cursor under it, is made again
///   from what the log holds by then, under the same number and another order; a vault left
///   and joined again reaches a number a second time. A cursor compared by the number alone
///   would stay where it was and skip the writes below it.
/// - **Placed just past the last record stepped past**, found by its id from the log's end
///   ([`Stores::member_id`]). A rewrite keeps the order of the records it keeps and places
///   what it newly holds after them (`aterm_secrets::membership_log::rooted`: each record
///   placed afresh, in the log's order), so the records after that one are exactly those not
///   yet stepped past. A settled checkpoint's shares follow its cut, so the newest record a
///   cursor passed is one a prune keeps; where it is not held the cursor starts at the log's
///   first record. Starting there after every rewrite would have each Mac publish its whole
///   log again at each prune. By its id, not the digest of its bytes: a shed keeps a
///   version-2 record's id and drops its ciphertext, so its bytes change under it.
/// - **A record names the cursor's place before any is stepped past.** A cursor that starts
///   at the log's length (the first start of all), or that a build before this one wrote, has
///   stepped past no record this egress saw. The first read of the log under the cursor's own
///   generation reads the record before the cursor too and keeps its id, so a Mac that
///   publishes nothing between its start and the log's first rewrite does not publish its
///   whole log then.
/// - **Records read between two equal reads of the generation.** The generation and the
///   records are two reads; while this process runs its number only grows (only a power cut
///   takes a rewrite back), so records read between two reads that agree are of that
///   generation, and a read a rewrite crossed is made again.
/// - **Records a write handed over are taken only under the generation it wrote them.** A
///   store's hook reads the cursor and hands the records after it with the generation the
///   store it wrote was written under ([`Handed`]). Records read before a rewrite (its own
///   write's, and writes queued before the cursor was placed again) index another order even
///   where their count happens to be the cursor's now: so they are taken only where the
///   write's generation is the cursor's, which is also the log's as this feed reads it, and
///   their index is the cursor's count; any other is dropped and the store read, as for a
///   write another process made. Two equal generations name one order, so records of the
///   cursor's generation from the cursor's count are exactly those the cursor has not stepped
///   past, of a log never rewritten (generation 0, every write today) and of a rewritten one
///   alike. Until FL-148E a write said nothing of its generation and its records were taken
///   only of a log never rewritten.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MembersCursor {
    /// The complete records stepped past, counted from the log's first.
    pub from: usize,
    /// The log's rewrite generation `from` counts under.
    pub generation: Generation,
}

/// How many times a read of the membership log a rewrite crossed is made again before the
/// feed that asked fails and is tried later.
const PLACE_TRIES: usize = 4;

/// A broken members cursor, for the tests' negative controls ([`Egress::break_for_test`]).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mutant {
    /// The log's generation is not read: the cursor stays where it was under the log before
    /// its rewrite, as before FL-148D.
    GenerationUnread,
    /// A cursor placed again starts at the log's first record, whatever it stepped past.
    FromFirst,
    /// Records a write handed over are taken where their count is the cursor's, whatever
    /// generation the write says it wrote them under.
    AnyGeneration,
    /// Records a write handed over are taken only of a log never rewritten, as before FL-148E:
    /// the generation the write says it wrote them under is not read.
    UnrewrittenAlone,
    /// Two generations of one number are one: the rewrite that made each is not compared.
    NumberAlone,
    /// The last record stepped past is known by the digest of its bytes, not by its id.
    PlacedByBytes,
    /// No record names the place of a cursor that has stepped past none.
    Unanchored,
    /// A members feed is published without asking whether this Mac's records ride the lane
    /// ([`Stores::members_lane`]), as before the slice B review of FL-148E: the records a
    /// write handed over while the lane was open are published after it shut.
    LaneUnasked,
}

/// A wake another process sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// A table store's new length.
    Table {
        /// The store.
        key: TableKey,
        /// Its length.
        length: u64,
    },
    /// The membership log's complete records' count.
    Members {
        /// The count.
        length: usize,
    },
}

impl Wake {
    /// Its datagram.
    #[must_use]
    pub fn encode(&self) -> String {
        use std::fmt::Write as _;
        match self {
            Self::Table { key, length } => {
                let mut out = String::with_capacity(96);
                out.push_str("atm-wake 1 table ");
                aterm_codec::hex::push_lower(&mut out, &key.table);
                let _ = writeln!(out, " {} {length}", if key.recovery { 'r' } else { 'p' });
                out
            }
            Self::Members { length } => format!("atm-wake 1 members {length}\n"),
        }
    }

    /// The wake `datagram` holds, `None` for anything else.
    #[must_use]
    pub fn decode(datagram: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(datagram).ok()?.strip_suffix('\n')?;
        let mut words = text.split(' ');
        if words.next()? != "atm-wake" || words.next()? != "1" {
            return None;
        }
        let wake = match words.next()? {
            "table" => {
                let hex = words.next()?;
                let mut table = [0u8; 32];
                if !aterm_codec::hex::is_lower(hex, 64)
                    || !aterm_codec::hex::decode_into(hex.as_bytes(), &mut table)
                {
                    return None;
                }
                let recovery = match words.next()? {
                    "p" => false,
                    "r" => true,
                    _ => return None,
                };
                Self::Table {
                    key: TableKey { table, recovery },
                    length: words.next()?.parse().ok()?,
                }
            }
            "members" => Self::Members {
                length: words.next()?.parse().ok()?,
            },
            _ => return None,
        };
        words.next().is_none().then_some(wake)
    }

    fn feed(self) -> Feed {
        match self {
            Self::Table { key, length } => Feed::Table {
                key,
                length,
                record: None,
            },
            Self::Members { length } => Feed::Members {
                length,
                records: None,
            },
        }
    }
}

/// Where the wake socket of the node service whose state dir is `state_dir` is.
#[must_use]
pub fn wake_path(state_dir: &Path) -> PathBuf {
    state_dir.join(crate::logs::MESH_DIR).join(WAKE_SOCKET)
}

/// Send `wake` to the node service whose state dir is `state_dir`, without waiting: where none
/// listens, or its socket is full, nothing happens, and its egress reads the store at its next
/// feed or start.
pub fn wake(state_dir: &Path, wake: &Wake) {
    let Ok(socket) = std::os::unix::net::UnixDatagram::unbound() else {
        return;
    };
    if socket.set_nonblocking(true).is_err() {
        return;
    }
    let _ = socket.send_to(wake.encode().as_bytes(), wake_path(state_dir));
}

/// What the egress has published, per store, and what it keeps on disk.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Cursors {
    tables: BTreeMap<TableKey, u64>,
    members: Option<usize>,
    /// The membership log's rewrite generation `members` counts under ([`MembersCursor`]):
    /// that of a log never rewritten in a file with no `g` line, which is every file a build
    /// before FL-148D wrote, under a log none of them rewrote.
    members_generation: Generation,
    /// The id ([`Stores::member_id`]) of the last membership record before the cursor, where
    /// one names its place: where a rewritten log is read from.
    members_last: Option<Digest>,
    /// The `tables.log` offset of the last record this Mac published of each table: what a push
    /// waits for a neighbour to ack, across restarts.
    last: BTreeMap<Digest, u64>,
}

impl Cursors {
    fn encode(&self) -> String {
        use std::fmt::Write as _;
        let mut text = String::with_capacity((self.tables.len() + self.last.len()) * 88 + 24);
        for (key, published) in &self.tables {
            text.push_str("t ");
            aterm_codec::hex::push_lower(&mut text, &key.table);
            let _ = writeln!(
                text,
                " {} {published}",
                if key.recovery { 'r' } else { 'p' }
            );
        }
        if let Some(members) = self.members {
            let _ = writeln!(text, "m {members}");
        }
        // Its own line, so `m` reads as it always has; a build that does not know it skips it,
        // and writes a file without it, which this build reads as a log never rewritten with
        // no record: under a rewritten log, a cursor that starts at the log's first record.
        // `g <number> <rewrite> <last>`, each of the two ids 64 hex digits or `-` for none.
        if self.members_generation != Generation::default() || self.members_last.is_some() {
            let _ = write!(text, "g {}", self.members_generation.number);
            for id in [&self.members_generation.rewrite, &self.members_last] {
                text.push(' ');
                match id {
                    Some(id) => aterm_codec::hex::push_lower(&mut text, id),
                    None => text.push('-'),
                }
            }
            text.push('\n');
        }
        for (table, offset) in &self.last {
            text.push_str("l ");
            aterm_codec::hex::push_lower(&mut text, table);
            let _ = writeln!(text, " {offset}");
        }
        text
    }

    /// The cursors `text` holds; a damaged line is skipped, so its store starts again from 0 and
    /// publishes duplicates the stores answer by digest.
    fn decode(text: &str) -> Self {
        let mut cursors = Self::default();
        for line in text.lines() {
            let mut words = line.split(' ');
            match words.next() {
                Some("t") => {
                    let (Some(hex), Some(region), Some(published), None) =
                        (words.next(), words.next(), words.next(), words.next())
                    else {
                        continue;
                    };
                    let mut table = [0u8; 32];
                    let recovery = match region {
                        "p" => false,
                        "r" => true,
                        _ => continue,
                    };
                    if aterm_codec::hex::is_lower(hex, 64)
                        && aterm_codec::hex::decode_into(hex.as_bytes(), &mut table)
                        && let Ok(published) = published.parse()
                    {
                        cursors
                            .tables
                            .insert(TableKey { table, recovery }, published);
                    }
                }
                Some("m") => {
                    if let (Some(published), None) = (words.next(), words.next()) {
                        cursors.members = published.parse().ok();
                    }
                }
                Some("g") => {
                    let (Some(number), Some(rewrite), Some(last), None) =
                        (words.next(), words.next(), words.next(), words.next())
                    else {
                        continue;
                    };
                    // An id of the line: `Some(None)` for `-`, `None` for anything else that
                    // is not 64 lowercase hex digits.
                    let id = |word: &str| -> Option<Option<Digest>> {
                        let mut id = [0u8; 32];
                        if word == "-" {
                            Some(None)
                        } else {
                            (aterm_codec::hex::is_lower(word, 64)
                                && aterm_codec::hex::decode_into(word.as_bytes(), &mut id))
                            .then_some(Some(id))
                        }
                    };
                    let (Ok(number), Some(rewrite), Some(last)) =
                        (number.parse::<u64>(), id(rewrite), id(last))
                    else {
                        continue;
                    };
                    // A rewrite is named exactly where one was made.
                    if (number == 0) != rewrite.is_none() {
                        continue;
                    }
                    cursors.members_generation = Generation { number, rewrite };
                    cursors.members_last = last;
                }
                Some("l") => {
                    let (Some(hex), Some(offset), None) =
                        (words.next(), words.next(), words.next())
                    else {
                        continue;
                    };
                    let mut table = [0u8; 32];
                    if aterm_codec::hex::is_lower(hex, 64)
                        && aterm_codec::hex::decode_into(hex.as_bytes(), &mut table)
                        && let Ok(offset) = offset.parse()
                    {
                        cursors.last.insert(table, offset);
                    }
                }
                _ => {}
            }
        }
        cursors
    }
}

/// The egress of one mesh: its feed queue, its cursors, and the membership ids the mesh delivered.
pub struct Egress {
    stores: Arc<dyn Stores>,
    dir: PathBuf,
    queue: Mutex<VecDeque<Feed>>,
    bell: Arc<Bell>,
    state: Mutex<State>,
    /// Rung each time a cursor moves: a push waits on it.
    moved: Condvar,
    /// The ids in [`ARRIVED_FILE`], and whether they changed since it was last written.
    arrived: Mutex<(BTreeSet<Digest>, bool)>,
    /// The operations each primary table store holds, as far as this egress has read them.
    table_holdings: Mutex<TableHoldings>,
    /// A feed was dropped from a full queue: the next pass catches every store up from its
    /// cursor.
    overflowed: AtomicBool,
    stop: AtomicBool,
    /// The negative control of "never published again": a record the mesh delivered is
    /// published as if it were local ([`Self::republish_for_test`]).
    #[cfg(any(test, debug_assertions))]
    republish: AtomicBool,
    /// The negative controls of the members cursor's placing ([`Mutant`]).
    #[cfg(test)]
    mutant: Mutex<Option<Mutant>>,
}

/// The records each primary table store holds, as far as the egress has read them
/// ([`Egress::tables_held`]).
#[derive(Default)]
struct TableHoldings {
    /// Each table's operations, by its digest.
    operations: BTreeMap<Digest, Holding>,
    /// Each table's conflict records, by its digest.
    conflicts: BTreeMap<Digest, Holding>,
    /// The combined summary over `operations` and `conflicts`, until any table's set changes.
    summary: Option<crate::presence::Held>,
    /// Whether the first catch-up read every store's records.
    seeded: bool,
}

/// The ids of one table's records of one kind.
#[derive(Default)]
struct Holding {
    ids: BTreeSet<Digest>,
    /// Its summary, until another id joins.
    summary: Option<crate::presence::Held>,
}

impl TableHoldings {
    /// Add the records among `records` of `key`'s store, operations and conflict records apart: a
    /// recovery store's are left out.
    fn hold(&mut self, key: TableKey, records: &[Held]) {
        if key.recovery {
            return;
        }
        for record in records {
            let kind = match record.kind {
                Kind::Operation => &mut self.operations,
                Kind::Conflict => &mut self.conflicts,
            };
            let holding = kind.entry(key.table).or_default();
            if holding.ids.insert(stores::digest(&record.wire)) {
                holding.summary = None;
                self.summary = None;
            }
        }
    }

    /// Each table that holds a record of the kind `of` keeps, with its summary under `summarize`.
    fn summaries(
        of: &mut BTreeMap<Digest, Holding>,
        summarize: fn(&BTreeSet<Digest>) -> crate::presence::Held,
    ) -> BTreeMap<Digest, crate::presence::Held> {
        of.iter_mut()
            .filter(|(_, holding)| !holding.ids.is_empty())
            .map(|(table, holding)| {
                let ids = &holding.ids;
                (
                    *table,
                    *holding.summary.get_or_insert_with(|| summarize(ids)),
                )
            })
            .collect()
    }
}

/// The egress's cursors and what it published last per table.
#[derive(Default)]
struct State {
    cursors: Cursors,
    /// Whether a cursor file was read at start: on the first start of all, history is not
    /// published.
    had_file: bool,
    /// Records published since start, for an observer.
    published: u64,
}

impl std::fmt::Debug for Egress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Egress").finish_non_exhaustive()
    }
}

impl Egress {
    /// The egress over `stores`, keeping its cursors in `dir` (`tables.log`'s folder).
    pub(crate) fn new(stores: Arc<dyn Stores>, dir: &Path) -> Arc<Self> {
        let read = aterm_uds::private_file::read_private(dir, CURSORS_FILE, 1 << 20)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok());
        let state = State {
            had_file: read.is_some(),
            cursors: read.as_deref().map(Cursors::decode).unwrap_or_default(),
            ..State::default()
        };
        Arc::new(Self {
            stores,
            dir: dir.to_path_buf(),
            queue: Mutex::new(VecDeque::new()),
            bell: Arc::new(Bell::default()),
            state: Mutex::new(state),
            moved: Condvar::new(),
            arrived: Mutex::new((read_arrived(dir), false)),
            table_holdings: Mutex::new(TableHoldings::default()),
            overflowed: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            #[cfg(any(test, debug_assertions))]
            republish: AtomicBool::new(false),
            #[cfg(test)]
            mutant: Mutex::new(None),
        })
    }

    /// On the first start of all (no cursor file), start each store's cursor at its length now,
    /// before any sink feeds this egress: what a store held before the mesh is history, which
    /// reaches the other Macs by `Inventory`.
    /// # Errors
    /// A store that could not be read: the flow does not start, so no write is taken for
    /// history.
    pub(crate) fn begin(&self) -> io::Result<()> {
        if self.state.lock_or_recover().had_file {
            return Ok(());
        }
        let lengths = self.stores.table_lengths()?;
        let (generation, members) = self.members_read(usize::MAX)?;
        let mut state = self.state.lock_or_recover();
        state.cursors.tables.extend(lengths);
        state.cursors.members = Some(members.1);
        state.cursors.members_generation = generation;
        state.had_file = true;
        let encoded = state.cursors.encode();
        drop(state);
        self.write_cursors(&encoded);
        Ok(())
    }

    /// Hand the egress `feed`. It never waits: a store's sink calls it under the store's locks.
    pub fn feed(&self, feed: Feed) {
        let mut queue = self.queue.lock_or_recover();
        if queue.len() >= QUEUE_MAX {
            // The next pass reads what this feed named from its store's cursor.
            queue.pop_front();
            self.overflowed.store(true, Ordering::Release);
        }
        queue.push_back(feed);
        drop(queue);
        self.bell.ring();
    }

    /// The egress's members cursor: an in-process write of the records file hands
    /// [`Self::feed`] the log's complete records from it on, in held order, with the
    /// generation it wrote them under ([`Feed::Members`], [`Handed`]).
    #[must_use]
    pub fn members_cursor(&self) -> MembersCursor {
        let state = self.state.lock_or_recover();
        MembersCursor {
            from: state.cursors.members.unwrap_or(0),
            generation: state.cursors.members_generation,
        }
    }

    /// Break the members cursor's placing as `mutant` says, or mend it: a negative control.
    #[cfg(test)]
    pub(crate) fn break_for_test(&self, mutant: Option<Mutant>) {
        *self.mutant.lock_or_recover() = mutant;
    }

    #[cfg(test)]
    fn broken(&self, mutant: Mutant) -> bool {
        *self.mutant.lock_or_recover() == Some(mutant)
    }

    /// Whether the membership log at `generation` is the one a cursor placed under `placed`
    /// counts: the same rewrite, not only the same number ([`MembersCursor`]).
    fn same(&self, generation: &Generation, placed: &Generation) -> bool {
        #[cfg(test)]
        if self.broken(Mutant::NumberAlone) {
            return generation.number == placed.number;
        }
        let _ = self;
        generation == placed
    }

    /// The id the membership log knows `record` by ([`Stores::member_id`]): what names the
    /// cursor's place through a rewrite.
    fn member_id(&self, record: &[u8]) -> Digest {
        #[cfg(test)]
        if self.broken(Mutant::PlacedByBytes) {
            return stores::digest(record);
        }
        self.stores.member_id(record)
    }

    /// The membership log's generation, and its complete records from index `from` with its
    /// held count, read under that generation: between two reads of it that agree
    /// ([`MembersCursor`]).
    fn members_read(&self, from: usize) -> io::Result<(Generation, (Vec<Vec<u8>>, usize))> {
        for _ in 0..PLACE_TRIES {
            let generation = self.stores.members_generation()?;
            let held = self.stores.members_held(from)?;
            if self.stores.members_generation()? == generation {
                return Ok((generation, held));
            }
        }
        Err(io::Error::other(
            "the membership log was rewritten during each read of it",
        ))
    }

    /// The membership log's complete records the egress has not stepped past, the index of the
    /// first of them and the log's held count, with the cursor placed again first where the
    /// log was rewritten since it was placed, and the record before the cursor read with them
    /// where none names its place yet ([`MembersCursor`]).
    /// # Errors
    /// Where the log or its generation cannot be read, or each read of it was crossed by a
    /// rewrite.
    pub(crate) fn members_unpublished(&self) -> io::Result<(usize, Vec<Vec<u8>>, usize)> {
        for _ in 0..PLACE_TRIES {
            let (cursor, last) = {
                let state = self.state.lock_or_recover();
                (
                    MembersCursor {
                        from: state.cursors.members.unwrap_or(0),
                        generation: state.cursors.members_generation,
                    },
                    state.cursors.members_last,
                )
            };
            let generation = self.stores.members_generation()?;
            #[cfg(test)]
            let generation = if self.broken(Mutant::GenerationUnread) {
                cursor.generation
            } else {
                generation
            };
            if self.same(&generation, &cursor.generation) {
                // No record names this cursor's place: the one before it is read too.
                let naming = last.is_none() && cursor.from > 0;
                #[cfg(test)]
                let naming = naming && !self.broken(Mutant::Unanchored);
                let (read, (mut records, length)) =
                    self.members_read(cursor.from - usize::from(naming))?;
                #[cfg(test)]
                let read = if self.broken(Mutant::GenerationUnread) {
                    generation
                } else {
                    read
                };
                if !self.same(&read, &generation) {
                    continue;
                }
                if naming && !records.is_empty() {
                    let before = self.member_id(&records.remove(0));
                    self.name_members_place(cursor, before);
                }
                return Ok((cursor.from, records, length));
            }
            // Rewritten since the cursor was placed: read it whole and place the cursor again.
            let (read, (mut records, length)) = self.members_read(0)?;
            let place = last
                .and_then(|last| {
                    records
                        .iter()
                        .rposition(|record| self.member_id(record) == last)
                })
                .map_or(0, |at| at + 1);
            #[cfg(test)]
            let place = if self.broken(Mutant::FromFirst) {
                0
            } else {
                place
            };
            let mut state = self.state.lock_or_recover();
            state.cursors.members = Some(place);
            state.cursors.members_generation = read;
            let encoded = state.cursors.encode();
            drop(state);
            self.write_cursors(&encoded);
            self.moved.notify_all();
            records.drain(..place);
            return Ok((place, records, length));
        }
        Err(io::Error::other(
            "the membership log was rewritten during each read of it",
        ))
    }

    /// Keep `before`, the id of the record before `cursor`, as what names the cursor's place,
    /// where the cursor still stands there with none.
    fn name_members_place(&self, cursor: MembersCursor, before: Digest) {
        let mut state = self.state.lock_or_recover();
        if state.cursors.members_last.is_some()
            || state.cursors.members != Some(cursor.from)
            || state.cursors.members_generation != cursor.generation
        {
            return;
        }
        state.cursors.members_last = Some(before);
        let encoded = state.cursors.encode();
        drop(state);
        self.write_cursors(&encoded);
    }

    /// Note the records `ids` the mesh delivered (membership records and table records alike, by
    /// the digest of their bytes), before their store is written: each is stepped past once, by
    /// the feed of its append or by a catch-up that reads it from its store. They are on disk
    /// ([`ARRIVED_FILE`]) before this returns, so a catch-up after a stop steps past them too.
    pub fn arrived(&self, ids: impl IntoIterator<Item = Digest>) {
        let mut arrived = self.arrived.lock_or_recover();
        let before = arrived.0.len();
        arrived.0.extend(ids);
        while arrived.0.len() > ARRIVED_MAX {
            arrived.0.pop_first();
        }
        if arrived.0.len() != before {
            arrived.1 = true;
        }
        self.flush_arrived(&mut arrived);
    }

    /// Hand `wires`, which the mesh delivered, to their store by `ingest`, noting each as arrived
    /// first ([`Self::arrived`]) and forgetting each the store did not apply ([`Self::forget`]).
    pub(crate) fn delivering(
        &self,
        wires: &[Vec<u8>],
        ingest: impl FnOnce() -> Vec<crate::stores::Verdict>,
    ) -> Vec<crate::stores::Verdict> {
        let ids: Vec<Digest> = wires.iter().map(|wire| stores::digest(wire)).collect();
        self.arrived(ids.iter().copied());
        let verdicts = ingest();
        self.forget(
            ids.iter()
                .zip(&verdicts)
                .filter(|(_, verdict)| !matches!(verdict, crate::stores::Verdict::Applied))
                .map(|(id, _)| *id),
        );
        verdicts
    }

    /// The noted `ids` whose records their store did not take (refused, waiting or busy): nothing
    /// will step past them. A record a store held already answers as applied, so its id stays
    /// until [`ARRIVED_MAX`] drops it.
    pub fn forget(&self, ids: impl IntoIterator<Item = Digest>) {
        let mut arrived = self.arrived.lock_or_recover();
        for id in ids {
            arrived.1 |= arrived.0.remove(&id);
        }
        self.flush_arrived(&mut arrived);
    }

    /// Whether the mesh delivered the record `id`, stepping past it. The file is written with the
    /// next cursors, after them: a stop between the two leaves an id that steps past nothing,
    /// never a delivered record that is published.
    fn take_arrived(&self, id: &Digest) -> bool {
        let mut arrived = self.arrived.lock_or_recover();
        let took = arrived.0.remove(id);
        arrived.1 |= took;
        took
    }

    fn flush_arrived(&self, arrived: &mut (BTreeSet<Digest>, bool)) {
        if !arrived.1 {
            return;
        }
        let mut text = String::with_capacity(arrived.0.len() * 65);
        for id in &arrived.0 {
            aterm_codec::hex::push_lower(&mut text, id);
            text.push('\n');
        }
        match aterm_uds::private_file::write_private(&self.dir, ARRIVED_FILE, text.as_bytes()) {
            Ok(()) => arrived.1 = false,
            Err(e) => {
                aterm_log::warn!("mesh: the egress's {ARRIVED_FILE} could not be written: {e}")
            }
        }
    }

    /// Wake the egress: a lease settled, or it should look at its queue now.
    pub fn ring(&self) {
        self.bell.ring();
    }

    /// What this Mac's primary table stores hold, as far as this egress has read them: each table
    /// that holds an operation, with how many and the fingerprint of their ids (aterm-db's
    /// `InventorySummary`), and each that holds a conflict record, with how many and the digest
    /// of their ids, under [`crate::presence::Held::tables`]. `None` until the first catch-up has
    /// read every store. It reads no store.
    ///
    /// It names every kind of table record `Inventory` moves (PF5b's finding H1): an exchange
    /// lists operations by id, and conflict proofs by a key the store derives from these records
    /// (a fork from two operations or a certificate; a discarded source from its certificate, or
    /// from the operation and the recovery that discarded it), so a proof one Mac lists and its
    /// neighbour does not comes with a record one holds and the other does not. The converse need
    /// not hold: a store keeps one certificate per key and refuses another, so two Macs whose
    /// certificates for one key came from different exporters read apart while an exchange moves
    /// nothing between them, which costs one greeting per pair of digests (the quiet rule). The
    /// keys themselves are derived state a store reads only in a session.
    #[must_use]
    pub fn tables_held(&self) -> Option<crate::presence::Held> {
        let mut holdings = self.table_holdings.lock_or_recover();
        if !holdings.seeded {
            return None;
        }
        if let Some(summary) = holdings.summary {
            return Some(summary);
        }
        let operations =
            TableHoldings::summaries(&mut holdings.operations, crate::presence::Held::table);
        let conflicts =
            TableHoldings::summaries(&mut holdings.conflicts, crate::presence::Held::conflicts);
        let summary = crate::presence::Held::tables(&operations, &conflicts);
        holdings.summary = Some(summary);
        Some(summary)
    }

    /// Note records `key`'s store holds ([`Self::tables_held`]).
    fn hold_table_records(&self, key: TableKey, records: &[Held]) {
        self.table_holdings.lock_or_recover().hold(key, records);
    }

    /// Read each primary store's records up to its length once, before the first catch-up
    /// publishes: what each held before this egress began ([`Self::tables_held`]).
    fn seed_table_holdings(&self, lengths: &[(TableKey, u64)]) -> io::Result<()> {
        if self.table_holdings.lock_or_recover().seeded {
            return Ok(());
        }
        for (key, length) in lengths.iter().filter(|(key, _)| !key.recovery) {
            let records = self.stores.table_held(*key, 0, *length)?;
            self.hold_table_records(*key, &records);
        }
        self.table_holdings.lock_or_recover().seeded = true;
        Ok(())
    }

    /// Records published since start.
    #[must_use]
    pub fn published(&self) -> u64 {
        self.state.lock_or_recover().published
    }

    /// The `tables.log` offset of the last record this Mac published of `table`.
    #[must_use]
    pub fn last_published(&self, table: &Digest) -> Option<u64> {
        self.state
            .lock_or_recover()
            .cursors
            .last
            .get(table)
            .copied()
    }

    /// Wait until `key`'s cursor reaches `length`, or `deadline`: whether it did.
    #[must_use]
    pub fn wait_published(&self, key: TableKey, length: u64, deadline: Instant) -> bool {
        let mut state = self.state.lock_or_recover();
        loop {
            if state.cursors.tables.get(&key).copied().unwrap_or(0) >= length {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || self.stop.load(Ordering::Acquire) {
                return false;
            }
            state = self
                .moved
                .wait_timeout(state, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    /// Wait until the membership cursor reaches `length`, or `deadline`: whether it did.
    #[must_use]
    pub fn wait_members_published(&self, length: usize, deadline: Instant) -> bool {
        let mut state = self.state.lock_or_recover();
        loop {
            if state.cursors.members.unwrap_or(0) >= length {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || self.stop.load(Ordering::Acquire) {
                return false;
            }
            state = self
                .moved
                .wait_timeout(state, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    /// Make the egress publish records the mesh delivered as if local: the negative control of
    /// "never published again".
    #[cfg(any(test, debug_assertions))]
    pub fn republish_for_test(&self, on: bool) {
        self.republish.store(on, Ordering::Release);
    }

    #[cfg(any(test, debug_assertions))]
    fn republishes(&self) -> bool {
        self.republish.load(Ordering::Acquire)
    }

    #[cfg(not(any(test, debug_assertions)))]
    const fn republishes(&self) -> bool {
        false
    }

    pub(crate) fn stop(&self) {
        {
            // Under the state's lock, so a push between its check of the flag and its wait
            // cannot miss the notice.
            let _state = self.state.lock_or_recover();
            self.stop.store(true, Ordering::Release);
        }
        self.bell.ring();
        self.moved.notify_all();
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    fn write_cursors(&self, encoded: &str) {
        if let Err(e) =
            aterm_uds::private_file::write_private(&self.dir, CURSORS_FILE, encoded.as_bytes())
        {
            aterm_log::warn!("mesh: the egress's {CURSORS_FILE} could not be written: {e}");
        }
        // The ids stepped past go after the cursors that passed them ([`Self::take_arrived`]).
        self.flush_arrived(&mut self.arrived.lock_or_recover());
    }

    /// The thread's body: catch up every store from its cursor, then publish what each feed
    /// names, until stopped or the mesh is gone.
    pub(crate) fn run(self: &Arc<Self>, mesh: &Weak<Mesh>) {
        let mut retry = RETRY_FIRST;
        let mut caught_up = false;
        while !self.stopped() {
            let Some(mesh) = mesh.upgrade() else { break };
            if mesh.is_stopped() {
                break;
            }
            if self.overflowed.swap(false, Ordering::AcqRel) {
                caught_up = false;
            }
            let ran = if caught_up {
                self.drain(&mesh, true)
            } else {
                self.catch_up(&mesh).map(|()| caught_up = true)
            };
            drop(mesh);
            match ran {
                Ok(()) => {
                    retry = RETRY_FIRST;
                    if self.queue.lock_or_recover().is_empty() {
                        self.bell.wait(None);
                    }
                }
                // A lease that waits for its first neighbour: the relay's settling rings the bell.
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => self.bell.wait(None),
                Err(e) => {
                    aterm_log::warn!("mesh: the egress could not publish ({e}); it tries again");
                    self.bell.wait(Some(retry));
                    retry = retry.saturating_mul(2).min(RETRY_MAX);
                }
            }
        }
        // Stopped with the logs still open (the flow stops before them): what the queue holds
        // is published now, not left to the next start's catch-up, which reads no provenance.
        if caught_up
            && let Some(mesh) = mesh.upgrade()
            && let Err(e) = self.drain(&mesh, false)
        {
            aterm_log::info!(
                "mesh: the egress stopped with feeds left ({e}); the next start reads them"
            );
        }
    }

    /// Publish what lies past each store's cursor. On the first start of all, each store's cursor
    /// starts at its length.
    fn catch_up(&self, mesh: &Mesh) -> io::Result<()> {
        // [`Self::begin`] set every cursor on the first start of all: a table with none now is
        // new, published from its first record.
        let lengths = self.stores.table_lengths()?;
        self.seed_table_holdings(&lengths)?;
        for (key, length) in lengths {
            self.publish_range(mesh, key, length)?;
        }
        let (from, records, length) = self.members_unpublished()?;
        self.publish_members(mesh, from, &records)?;
        {
            let mut state = self.state.lock_or_recover();
            state.cursors.members = Some(length.max(state.cursors.members.unwrap_or(0)));
            let encoded = state.cursors.encode();
            drop(state);
            self.write_cursors(&encoded);
        }
        self.moved.notify_all();
        Ok(())
    }

    /// Publish every feed in the queue; a feed that fails goes back to the queue's head. With
    /// `heed_stop`, a stop ends it between feeds.
    fn drain(&self, mesh: &Mesh, heed_stop: bool) -> io::Result<()> {
        loop {
            let Some(feed) = self.queue.lock_or_recover().pop_front() else {
                return Ok(());
            };
            if let Err(e) = self.publish_feed(mesh, &feed) {
                self.queue.lock_or_recover().push_front(feed);
                return Err(e);
            }
            if heed_stop && self.stopped() {
                return Ok(());
            }
        }
    }

    fn publish_feed(&self, mesh: &Mesh, feed: &Feed) -> io::Result<()> {
        match feed {
            Feed::Table {
                key,
                length,
                record,
            } => {
                match record {
                    Some((held, mesh_borne)) if held.offset + 1 == *length => {
                        self.hold_table_records(*key, std::slice::from_ref(held));
                        // What another process appended before this one is local.
                        self.publish_range(mesh, *key, held.offset)?;
                        let cursor = self
                            .state
                            .lock_or_recover()
                            .cursors
                            .tables
                            .get(key)
                            .copied();
                        if cursor.unwrap_or(0) > held.offset {
                            return Ok(());
                        }
                        // Its id steps past once either way: the feed says how it came.
                        let delivered =
                            self.take_arrived(&stores::digest(&held.wire)) || *mesh_borne;
                        if !delivered || self.republishes() {
                            self.publish_one(mesh, *key, held)?;
                        }
                        self.set_cursor(*key, held.offset + 1);
                        Ok(())
                    }
                    _ => self.publish_range(mesh, *key, *length),
                }
            }
            Feed::Members { length, records } => {
                // The lane first: a Mac whose records ride no members lane (they are past
                // format 1, [`Stores::members_lane`]) feeds it nothing. A write asks before it
                // hands its records over, and this feed may be read after the lane shut (the
                // format mark stored by another process, or by this one before this thread
                // read its queue): the records it carries are then dropped unpublished, and no
                // store is read, which would read an empty log (ATERM-KI-1116-1471).
                let open = self.stores.members_lane()?;
                #[cfg(test)]
                let open = open || self.broken(Mutant::LaneUnasked);
                if !open {
                    return Ok(());
                }
                // The generation next: under another, neither the length nor the records this
                // feed names are of the order the cursor counts ([`MembersCursor`]).
                let cursor = self.members_cursor();
                let generation = self.stores.members_generation()?;
                #[cfg(test)]
                let generation = if self.broken(Mutant::GenerationUnread) {
                    cursor.generation
                } else {
                    generation
                };
                if self.same(&generation, &cursor.generation) {
                    if *length <= cursor.from {
                        return Ok(());
                    }
                    if let Some(handed) = records {
                        // The write's own generation: records written under another index
                        // another order, whatever their count ([`MembersCursor`]).
                        let of_cursor = self.same(&handed.generation, &cursor.generation);
                        #[cfg(test)]
                        let of_cursor = (of_cursor || self.broken(Mutant::AnyGeneration))
                            && !(self.broken(Mutant::UnrewrittenAlone) && generation.number != 0);
                        if of_cursor && handed.from == cursor.from {
                            return self.publish_members(mesh, cursor.from, &handed.records);
                        }
                    }
                }
                let (from, records, _) = self.members_unpublished()?;
                self.publish_members(mesh, from, &records)
            }
        }
    }

    /// Publish `key`'s records from its cursor up to `upto`, reading its store: all of them
    /// another process's appends, or appends the egress was not running for.
    fn publish_range(&self, mesh: &Mesh, key: TableKey, upto: u64) -> io::Result<()> {
        loop {
            let cursor = self
                .state
                .lock_or_recover()
                .cursors
                .tables
                .get(&key)
                .copied()
                .unwrap_or(0);
            if cursor >= upto {
                return Ok(());
            }
            let held =
                self.stores
                    .table_held(key, cursor, upto.min(cursor.saturating_add(READ_CHUNK)))?;
            if held.is_empty() {
                // The store acknowledged less than the wake named: nothing more to read now.
                return Ok(());
            }
            self.hold_table_records(key, &held);
            for record in &held {
                if record.offset < cursor {
                    continue;
                }
                // A record the mesh delivered, whose feed this egress never read.
                let delivered = self.take_arrived(&stores::digest(&record.wire));
                if !delivered || self.republishes() {
                    self.publish_one(mesh, key, record)?;
                }
                self.set_cursor(key, record.offset + 1);
            }
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "MeshRelay",
            action = "Publish",
            project = "aterm_mesh::tests::tables::relay_conformance::mesh_relay_transition"
        )
    )]
    fn publish_one(&self, mesh: &Mesh, key: TableKey, held: &Held) -> io::Result<()> {
        let subject = stores::table_subject(&key.table, mesh.principal());
        let published = mesh.publish(
            MeshLog::Tables,
            &subject,
            &stores::table_payload(held.kind, &held.wire),
        )?;
        let mut state = self.state.lock_or_recover();
        state.cursors.last.insert(key.table, published.offset);
        state.published += 1;
        Ok(())
    }

    fn set_cursor(&self, key: TableKey, to: u64) {
        let mut state = self.state.lock_or_recover();
        let cursor = state.cursors.tables.entry(key).or_insert(0);
        if to <= *cursor {
            return;
        }
        *cursor = to;
        let encoded = state.cursors.encode();
        drop(state);
        self.write_cursors(&encoded);
        self.moved.notify_all();
    }

    /// Publish the membership `records` held from index `from` on, stepping past each id the
    /// mesh delivered, moving the cursor past each.
    fn publish_members(&self, mesh: &Mesh, from: usize, records: &[Vec<u8>]) -> io::Result<()> {
        let subject = stores::members_subject(mesh.principal());
        for (at, record) in records.iter().enumerate() {
            let index = from + at;
            if self.state.lock_or_recover().cursors.members.unwrap_or(0) > index {
                continue;
            }
            let id = stores::digest(record);
            let delivered = self.take_arrived(&id);
            let published = !delivered || self.republishes();
            if published {
                mesh.publish(MeshLog::Tables, &subject, record)?;
            }
            // What names the cursor's place once it is past this record ([`MembersCursor`]).
            let named = self.member_id(record);
            let mut state = self.state.lock_or_recover();
            if published {
                state.published += 1;
            }
            state.cursors.members = Some(index + 1);
            state.cursors.members_last = Some(named);
            let encoded = state.cursors.encode();
            drop(state);
            self.write_cursors(&encoded);
        }
        self.moved.notify_all();
        Ok(())
    }
}

/// The ids [`ARRIVED_FILE`] in `dir` holds, each a lowercase hex digest a line; a line that is not
/// one is skipped, and a missing or unreadable file holds none.
fn read_arrived(dir: &Path) -> BTreeSet<Digest> {
    let Ok(bytes) =
        aterm_uds::private_file::read_private(dir, ARRIVED_FILE, (ARRIVED_MAX + 1) * 65)
    else {
        return BTreeSet::new();
    };
    bytes
        .split(|&b| b == b'\n')
        .filter_map(|line| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let line = std::str::from_utf8(line).ok()?;
            let mut id = [0u8; 32];
            (aterm_codec::hex::is_lower(line, 64)
                && aterm_codec::hex::decode_into(line.as_bytes(), &mut id))
            .then_some(id)
        })
        .take(ARRIVED_MAX)
        .collect()
}

/// The wake socket's reader: bound in `<state_dir>/mesh/`, it hands each wake to the egress, and
/// ends when the egress stops (a datagram to itself wakes it).
pub(crate) struct WakeSocket {
    path: PathBuf,
    socket: Arc<std::os::unix::net::UnixDatagram>,
}

impl WakeSocket {
    /// Bind the wake socket at `path`, replacing a stale one a previous run left.
    pub(crate) fn bind(path: &Path) -> io::Result<Self> {
        if std::fs::symlink_metadata(path).is_ok_and(|meta| {
            use std::os::unix::fs::FileTypeExt as _;
            meta.file_type().is_socket()
        }) {
            std::fs::remove_file(path)?;
        }
        let socket = std::os::unix::net::UnixDatagram::bind(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            socket: Arc::new(socket),
        })
    }

    /// Read wakes until `egress` stops, and answer each push another process asks
    /// ([`crate::push`]).
    pub(crate) fn run(&self, egress: &Egress, mesh: &std::sync::Weak<Mesh>) {
        let mut buf = [0u8; WAKE_MAX];
        while !egress.stopped() {
            match self.socket.recv_from(&mut buf) {
                Ok((n, from)) => {
                    if let Some(wake) = Wake::decode(&buf[..n]) {
                        egress.feed(wake.feed());
                    } else if let Some((table, within)) = crate::push::read_ask(&buf[..n]) {
                        crate::push::answer(&self.socket, mesh, table, within, &from);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    aterm_log::warn!("mesh: the egress's wake socket failed: {e}");
                    return;
                }
            }
        }
    }

    /// Wake the reader so it sees the egress stopped.
    pub(crate) fn nudge(&self) {
        let _ = self.socket.send_to(b"stop\n", &self.path);
    }
}

impl Drop for WakeSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The stores' sinks of this process, installed once: each forwards to the egress open now, if
/// any. The node service opens and closes its mesh across generations; the sinks stay.
pub(crate) mod sinks {
    use super::Egress;
    use aterm_types::MutexExt;
    use std::sync::{Mutex, OnceLock, Weak};

    static OPEN: Mutex<Option<Weak<Egress>>> = Mutex::new(None);
    static INSTALLED: OnceLock<()> = OnceLock::new();

    /// The egress open now.
    pub(crate) fn open() -> Option<std::sync::Arc<Egress>> {
        OPEN.lock_or_recover().as_ref().and_then(Weak::upgrade)
    }

    /// `egress` is the one this process's stores feed; with `None`, none is.
    pub(crate) fn serve(egress: Option<&std::sync::Arc<Egress>>) {
        *OPEN.lock_or_recover() = egress.map(std::sync::Arc::downgrade);
    }

    /// Install the stores' sinks once, with `set` (the vault's: [`crate::stores::vault`]).
    pub(crate) fn install_once(set: impl FnOnce()) {
        INSTALLED.get_or_init(set);
    }
}

#[cfg(test)]
#[path = "egress_tests.rs"]
mod tests;
