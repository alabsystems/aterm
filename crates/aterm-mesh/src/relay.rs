// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The push relay and the sequence rule (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.6, "The
//! relay" and "Sequence numbers never repeat").
//!
//! **A `Bus` stream.** A neighbour opens a `Bus` stream on its link, naming the log
//! (`aterm_net::fleet::link::BusLog`). The receiving Mac checks the opener's key against the
//! membership log, then writes its grants at the head of the stream, each minted under that log's
//! guard and bound to the opener's own principal, and hands the rest of the stream to its broker
//! ([`astream_broker::Broker::serve_stream`]) under the opener's key:
//!
//! ```text
//! atm-bus 1
//! cap <filter> <64 lowercase hex: the grant's tag>
//! …
//! end
//! ```
//!
//! The opener reads those lines, keeps any bytes it read past them, and speaks astream's protocol
//! on what follows ([`BusLink`]). A revoked key's stream is refused by the broker and ended at
//! once when the key is revoked (`revoke_key`).
//!
//! **The relay.** For each neighbour and each log, one push relay ([`run`]) follows its own log
//! with a consumer group named for the neighbour (`/relay/n-<neighbour>`), so the broker pushes each
//! record as it lands; it relays each record whose subject travels, keeping its origin's producer
//! id, sequence number and signed body (`astream_broker::FederationBridge` with `relay: true`), up
//! to 256 at a time, and commits its group once per burst, after the neighbour acks. The receiving
//! broker checks each record's signature against the origin keys the membership log gives
//! ([`crate::Mesh::apply`]) and deduplicates on the origin's key, so each broker appends each
//! record once and nothing goes round a cycle.
//!
//! - **The cursor is kept 7 days.** A relay to a neighbour this Mac relayed to within the last 7
//!   days ([`Relayed`], persisted at most once per 10 minutes), whose log is still the one it
//!   relayed to (its `LogIdentity`), or a later generation of it that the log's lineage carries
//!   (an unclean stop or a reboot of the neighbour), resumes from its group, and delivers, once
//!   and in order, every record that landed since; after a generation move it sends the newest
//!   rows first, as a log that flushes lazily may have lost the last ones it held, and leaves the
//!   rest of what the fork took to `Inventory` (B7b). Its group never resumes past a record the
//!   neighbour has not answered (astream's `TlaPushEdge.NoSkip`).
//! - **A record whose origin the neighbour does not take yet is held back.** The neighbour answers
//!   it `ERR_ORIGIN_UNKNOWN` (it has not seated that Mac yet, still holds it removed, its
//!   removal since voided, or reads its key contested): while this Mac seats the origin, the relay holds the record, and every
//!   later one of that origin, and offers the first again with each burst and each idle mark; its
//!   group stays below them, so a restarted relay offers them again. Once this Mac no longer seats
//!   the origin they are let go (`FederationBridge::hold_while`, [`crate::Mesh`]'s `still_seats`).
//! - **A new connection starts with the newest values.** A relay with no cursor, or one older than
//!   7 days, first sends its newest `now` and `quota` rows (on `metrics.log`), each with its
//!   origin's key, then follows its log's head. What is older is `Inventory`'s (B7b).
//! - A relay that fails tries again on the link clock, from 1 second doubling to a minute, each
//!   wait measured from the failed try's start. A membership change or the neighbour's own `Bus` stream (a kick from the overlay) resets it to
//!   1 second from its last try's start, never less, so two relays that fail after each other's
//!   accept cannot drive each other faster than once a second.
//!
//! **The sequence rule** ([`Origin`]). Every record this Mac originates is numbered
//! `incarnation_base(inc) + n` under the log's own incarnation lease (aterm-link's
//! `state::StateDir`: `take_incarnation`, `reserve_sequence_block`), so a start takes the next
//! incarnation and a power cut that lost unflushed numbers never reuses one. A Mac with no lease
//! (rebuilt with its node key kept) takes `inc = max(lease + 1, seconds since 2026-01-01)` and
//! publishes nothing until its first neighbour told it the high water it holds of this Mac (only
//! records this Mac's key signed count); it then moves above it. The wait is on disk (the lease's
//! `unsettled` file, written before the first incarnation is taken), so a restart before any
//! neighbour answered still waits. `REUSED_INCARNATION` is the rule's negative control.

use crate::inventory::Reason;
use crate::logs::{Me, Mesh, MeshLog, Wall};
use astream_broker::{
    AnyClient, Broker, Capability, FederationBridge, FederationBridgeCloser, FederationConfig,
    FederationEndpoint, FederationError, Lineage, LogIdentity, StreamDial, StreamLink,
    SubjectRewrite,
};
use aterm_link::state::StateDir;
use aterm_net::fleet::FleetOp;
use aterm_net::fleet::link::{Bell, Link, LinkStream};
use aterm_types::MutexExt;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write as _};
use std::net::Shutdown;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

/// The prefix of every relay's consumer group: `/relay/n-<neighbour>`.
pub const GROUP_PREFIX: &str = "/relay";

/// How long a neighbour's cursor is kept: a relay to a neighbour last relayed to within it resumes.
pub const CURSOR_KEPT: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How often the time this Mac last relayed to a neighbour is written down, at most.
pub const RELAYED_EVERY: Duration = Duration::from_secs(10 * 60);

/// 2026-01-01T00:00:00Z: the origin of a rebuilt Mac's incarnation floor.
pub const INC_EPOCH_UNIX_SECS: u64 = 1_767_225_600;

/// The bound on every read and write a relay makes on its own (the grants, a high-water read, the
/// newest rows): an I/O bound, not a schedule.
pub const IO_WITHIN: Duration = Duration::from_secs(30);

/// The first retry of a failed relay, doubled each failure up to [`BACKOFF_MAX`].
pub const BACKOFF_FIRST: Duration = Duration::from_secs(1);

/// The longest wait between a failed relay's tries.
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// How many sequence numbers one write of the lease's `seq` file reserves.
const SEQ_BLOCK: u64 = 64;

/// The file in a lease's folder that says it waits for its first neighbour's high water.
pub const UNSETTLED_FILE: &str = "unsettled";

/// The first line of a `Bus` stream's grants.
const GRANTS_HEAD: &str = "atm-bus 1";

/// The most bytes the grants at a stream's head may take.
const GRANTS_MAX: usize = 4096;

/// The most grants one stream may carry.
const GRANTS_COUNT_MAX: usize = 8;

/// The records of one burst a relay carries, at most (astream's `FEDERATION_BATCH_CAP`).
const BURST: usize = 256;

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The negative control of the sequence rule (`reused-incarnation`): while set on the thread
    /// that opens a log's lease, a restart takes the incarnation it used last, and a rebuilt Mac
    /// settles at the incarnation of its first neighbour's high water, not above it, so its new
    /// records reuse numbers its neighbours hold and are answered "already held". A lab instance
    /// turns it on for its whole process with `reused-incarnation` in
    /// `ATERM_TEST_NEGATIVE_CONTROL`.
    pub static REUSED_INCARNATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether [`REUSED_INCARNATION`] is on: set on this thread, or named for this process.
#[cfg(any(test, debug_assertions))]
fn reused_incarnation() -> bool {
    REUSED_INCARNATION.with(std::cell::Cell::get)
        || aterm_types::negative_control::on("reused-incarnation")
}

/// What a relay or a publish did, for an observer beside the journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A relay to `peer` follows `log`'s head: what lands now reaches the neighbour.
    Following {
        /// The neighbour's principal.
        peer: String,
        /// The log.
        log: MeshLog,
    },
    /// A relay with no cursor sent its log's newest rows first.
    NewestFirst {
        /// The neighbour's principal.
        peer: String,
        /// The log.
        log: MeshLog,
        /// The rows sent, each with its origin's key.
        rows: usize,
    },
    /// One burst the neighbour acked.
    Burst {
        /// The neighbour's principal.
        peer: String,
        /// The log.
        log: MeshLog,
        /// Records the neighbour appended.
        appended: usize,
        /// Records the neighbour already held.
        deduped: usize,
        /// Records the neighbour refused for good (a record that is not its origin's), and ones of
        /// an origin it did not take that this Mac no longer seats.
        refused: usize,
        /// Records the relay holds back now, for origins the neighbour does not take yet.
        held: usize,
    },
    /// A relay ended; it tries again unless its link ended or it was stopped.
    Ended {
        /// The neighbour's principal.
        peer: String,
        /// The log.
        log: MeshLog,
        /// Why.
        why: String,
    },
    /// A removed Mac's rows were retired from every current view of `metrics.log`.
    Retired {
        /// The removed Mac's principal.
        principal: String,
    },
    /// A relay read that its neighbour's cursor fell below what the log keeps: records
    /// `from..to` cannot reach it by the relay.
    Gap {
        /// The neighbour's principal.
        peer: String,
        /// The log.
        log: MeshLog,
        /// The first offset given up.
        from: u64,
        /// One past the last.
        to: u64,
    },
    /// An `Inventory` exchange with `peer` ended: this Mac started it for `reason`, or with
    /// `None` the neighbour did.
    Inventory {
        /// The neighbour's principal.
        peer: String,
        /// Why this Mac started it.
        reason: Option<crate::inventory::Reason>,
        /// Records this Mac sent.
        sent: usize,
        /// Records this Mac received and applied.
        received: usize,
    },
    /// An `Inventory` with `peer` was asked, and not run: the last one moved nothing and this
    /// Mac's digest has not changed since.
    InventorySkipped {
        /// The neighbour's principal.
        peer: String,
        /// Why it was asked.
        reason: crate::inventory::Reason,
    },
    /// The ingest consumer handed records to a store.
    Ingested {
        /// The membership log's, or a table's.
        members: bool,
        /// Records the store applied (or held already).
        applied: usize,
        /// Records that wait for a membership record.
        waiting: usize,
        /// Records refused for good.
        refused: usize,
    },
    /// A rebuilt Mac's lease settled on its first neighbour's high water.
    Settled {
        /// The log.
        log: MeshLog,
        /// The incarnation its records are numbered in now.
        incarnation: u64,
        /// The highest sequence number of this Mac's the neighbour held.
        high: Option<u64>,
    },
}

/// Where the mesh reports each [`Event`].
pub type Observe = Arc<dyn Fn(&Event) + Send + Sync>;

/// A record this Mac published.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Published {
    /// Its offset in the local log.
    pub offset: u64,
    /// Its sequence number, which the record keeps on every Mac.
    pub seq: u64,
}

/// The consumer group of a relay to `peer`.
#[must_use]
pub fn group_of(peer: &str) -> String {
    format!("{GROUP_PREFIX}/{peer}")
}

pub(crate) fn wall_ms(wall: &Wall) -> u64 {
    wall()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// One log's sequence numbers: its incarnation lease and the block of numbers it holds.
pub struct Origin {
    state: StateDir,
    dir: PathBuf,
    log: MeshLog,
    inc: u64,
    settled: bool,
    /// The next number of the block reserved, and its last.
    block: Option<(u64, u64)>,
    #[cfg(any(test, debug_assertions))]
    reuse: bool,
}

impl std::fmt::Debug for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Origin")
            .field("log", &self.log)
            .field("inc", &self.inc)
            .field("settled", &self.settled)
            .finish_non_exhaustive()
    }
}

impl Origin {
    /// The lease in `dir`: a start takes its next incarnation, settled at once; a fresh lease (no
    /// `inc` file: a rebuilt Mac, or a new one), or one still marked [`UNSETTLED_FILE`], takes one
    /// at or above the seconds since 2026-01-01 on `wall`, unsettled until its first neighbour's
    /// high water ([`Self::settle`]) or the membership log seats no other Mac
    /// ([`Self::settle_alone`]). The mark is written, and flushed, before a fresh lease takes its
    /// first incarnation, so no crash leaves a fresh lease that reads settled.
    /// # Errors
    /// Where the lease or its mark cannot be read or written, or the incarnation space is spent.
    pub(crate) fn open(dir: &Path, wall: &Wall, log: MeshLog) -> io::Result<Self> {
        // Private before the lease makes its own folders in it: the mark is a private file.
        aterm_uds::private_file::ensure_private_dir(dir)?;
        let fresh = std::fs::symlink_metadata(dir.join("inc")).is_err();
        let marked = std::fs::symlink_metadata(dir.join(UNSETTLED_FILE)).is_ok();
        let state = StateDir::open(dir)?;
        #[cfg(any(test, debug_assertions))]
        let reuse = reused_incarnation();
        #[cfg(not(any(test, debug_assertions)))]
        let reuse = false;
        let waits = fresh || marked;
        if waits && !marked {
            aterm_uds::private_file::write_private(
                dir,
                UNSETTLED_FILE,
                b"this lease waits for its first neighbour's high water\n",
            )?;
            aterm_durable::durable_dir(dir)?;
        }
        let inc = if waits {
            let since = wall()
                .duration_since(UNIX_EPOCH + Duration::from_secs(INC_EPOCH_UNIX_SECS))
                .map_or(0, |d| d.as_secs());
            state.take_incarnation(since.saturating_sub(1))?
        } else if reuse {
            state.incarnation()
        } else {
            state.take_incarnation(0)?
        };
        Ok(Self {
            state,
            dir: dir.to_path_buf(),
            log,
            inc,
            settled: !waits,
            block: None,
            #[cfg(any(test, debug_assertions))]
            reuse,
        })
    }

    #[cfg(any(test, debug_assertions))]
    const fn reuses(&self) -> bool {
        self.reuse
    }

    #[cfg(not(any(test, debug_assertions)))]
    const fn reuses(&self) -> bool {
        false
    }

    /// The incarnation this log's records are numbered in.
    #[must_use]
    pub const fn incarnation(&self) -> u64 {
        self.inc
    }

    /// Whether records may be numbered.
    #[must_use]
    pub const fn is_settled(&self) -> bool {
        self.settled
    }

    /// The next sequence number.
    fn next_seq(&mut self) -> io::Result<u64> {
        if !self.settled {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "{}'s lease is new: it waits for the high water its first neighbour holds",
                    self.log.file()
                ),
            ));
        }
        let (next, last) = match self.block {
            Some((next, last)) if next <= last => (next, last),
            _ => {
                let block = self.state.reserve_sequence_block(self.inc, SEQ_BLOCK)?;
                (*block.start(), *block.end())
            }
        };
        self.block = Some((next.saturating_add(1), last));
        Ok(next)
    }

    /// Move to an incarnation above this one: a fresh number was answered "already held".
    fn roll(&mut self) -> io::Result<()> {
        self.inc = self.state.take_incarnation(self.inc)?;
        self.block = None;
        Ok(())
    }

    /// Settle a fresh lease on `high`, the highest sequence number of this Mac's a neighbour
    /// holds: an incarnation at or above the one numbered now moves this one above it.
    pub(crate) fn settle(&mut self, high: Option<u64>) -> io::Result<()> {
        if self.settled {
            return Ok(());
        }
        if let Some(held) = high.and_then(astream_broker::incarnation_of)
            && held >= self.inc
        {
            self.inc = if self.reuses() {
                held
            } else {
                self.state.take_incarnation(held)?
            };
            self.block = None;
        }
        self.settled = true;
        self.unmark();
        Ok(())
    }

    /// Settle with no neighbour's word: the membership log seats no other Mac.
    pub(crate) fn settle_alone(&mut self) {
        if !self.settled {
            self.settled = true;
            self.unmark();
        }
    }

    /// Take the lease's [`UNSETTLED_FILE`] away. Where that fails the lease is settled for this
    /// run, and the next open waits again: the safe way round.
    fn unmark(&self) {
        let removed = aterm_uds::private_file::remove_private(&self.dir, UNSETTLED_FILE)
            .and_then(|_| aterm_durable::durable_dir(&self.dir));
        if let Err(e) = removed {
            aterm_log::warn!(
                "mesh: {}'s lease could not drop its {UNSETTLED_FILE} mark: {e}",
                self.log.file()
            );
        }
    }
}

/// Whether `subject` is one `principal` publishes on `log`: `/t/<table>/<principal>`, or any
/// subject under `/m/<principal>/`.
fn owns(log: MeshLog, principal: &str, subject: &str) -> bool {
    match log {
        MeshLog::Tables => subject
            .strip_prefix("/t/")
            .and_then(|rest| rest.split_once('/'))
            .is_some_and(|(table, owner)| !table.is_empty() && owner == principal),
        MeshLog::Metrics => subject
            .strip_prefix("/m/")
            .and_then(|rest| rest.strip_prefix(principal))
            .and_then(|rest| rest.strip_prefix('/'))
            .is_some_and(|rest| !rest.is_empty()),
    }
}

/// [`Mesh::publish`]: sign `payload` for `subject`, number it, and publish it on the local log. A
/// fresh number answered "already held" means another life of this Mac used it: the lease moves
/// to a new incarnation and the record goes once more, above it.
pub(crate) fn publish(
    mesh: &Mesh,
    log: MeshLog,
    subject: &str,
    payload: &[u8],
) -> io::Result<Published> {
    let me = &mesh.me;
    if !owns(log, &me.principal, subject) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{subject} is not a subject {} publishes on {}",
                me.principal,
                log.file()
            ),
        ));
    }
    let hosted = mesh.hosted(log);
    let mut origin = hosted.origin.lock_or_recover();
    for _ in 0..2 {
        let seq = origin.next_seq()?;
        let body = astream_broker::origin::seal(me.seed(), subject, seq, payload);
        let (offset, held) = hosted.publish_local(me, seq, subject, &body)?;
        if !held {
            return Ok(Published { offset, seq });
        }
        aterm_log::warn!(
            "mesh: {} sequence {seq} was used before by this Mac: taking a new incarnation",
            log.file()
        );
        origin.roll()?;
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "{}: two fresh sequence numbers were held already",
            log.file()
        ),
    ))
}

/// When this Mac last relayed to each neighbour, and the neighbour's log it relayed to, kept in
/// `relayed` beside the log: one line per neighbour, `n-<id> <unix ms> <log identity>` (`-` for
/// none), written at once when the identity changes and otherwise at most once per
/// [`RELAYED_EVERY`] each.
pub(crate) struct Relayed {
    dir: PathBuf,
    at: BTreeMap<String, (u64, Option<LogIdentity>)>,
    written: BTreeMap<String, (u64, Option<LogIdentity>)>,
}

/// The file [`Relayed`] keeps.
pub const RELAYED_FILE: &str = "relayed";

/// What a relay does with the cursor it kept ([`Relayed::cursor`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cursor {
    /// The same log: resume.
    Kept,
    /// A later generation of the same log, carried by its lineage: resume, after the newest rows
    /// again, as the fork may have taken the last ones the neighbour held.
    Moved,
    /// None, too old, or another log: start at the head, with the newest rows.
    Gone,
}

impl Relayed {
    /// What `dir` holds; an unreadable or damaged file holds nothing, so every relay starts with
    /// the newest rows.
    pub(crate) fn read(dir: &Path) -> Self {
        let at: BTreeMap<String, (u64, Option<LogIdentity>)> =
            aterm_uds::private_file::read_private(dir, RELAYED_FILE, 64 * 1024)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .map(|text| {
                    text.lines()
                        .filter_map(|line| {
                            let mut fields = line.split(' ');
                            let (peer, ms, dest) = (fields.next()?, fields.next()?, fields.next()?);
                            if fields.next().is_some() || !aterm_net::fleet::is_node_id(peer) {
                                return None;
                            }
                            let dest = match dest {
                                "-" => None,
                                text => Some(LogIdentity::parse(text)?),
                            };
                            Some((peer.to_owned(), (ms.parse().ok()?, dest)))
                        })
                        .collect()
                })
                .unwrap_or_default();
        Self {
            dir: dir.to_path_buf(),
            written: at.clone(),
            at,
        }
    }

    /// What a relay to `peer`, whose log is `dest` now (with the `lineage` told beside it), does
    /// with its cursor at `now_ms`. It resumes where this Mac relayed to that log within
    /// [`CURSOR_KEPT`], and where it relayed to an earlier generation of it that `lineage`
    /// carries to this one (an unclean stop or a reboot moves a log's generation with lineage,
    /// and the records this Mac relayed are still there, but those after the fork of a log that
    /// flushes lazily may not be). A neighbour rebuilt or renewed, or one whose lineage does not
    /// reach back, is a new log, whose relay starts again.
    pub(crate) fn cursor(
        &self,
        peer: &str,
        dest: Option<LogIdentity>,
        lineage: Option<&Lineage>,
        now_ms: u64,
    ) -> Cursor {
        let kept = u64::try_from(CURSOR_KEPT.as_millis()).unwrap_or(u64::MAX);
        let Some((at, was)) = self.at.get(peer) else {
            return Cursor::Gone;
        };
        if now_ms.saturating_sub(*at) >= kept {
            return Cursor::Gone;
        }
        match (*was, dest) {
            (was, dest) if was == dest => Cursor::Kept,
            (Some(was), Some(dest))
                if lineage.is_some_and(|lineage| dest.continues(was, lineage).is_some()) =>
            {
                Cursor::Moved
            }
            _ => Cursor::Gone,
        }
    }

    /// This Mac relays to `peer`'s log `dest` at `now_ms`.
    pub(crate) fn relays_to(&mut self, peer: &str, dest: Option<LogIdentity>, now_ms: u64) {
        if let Some(entry) = self.at.get_mut(peer) {
            *entry = (now_ms, dest);
        } else {
            self.at.insert(peer.to_owned(), (now_ms, dest));
        }
        self.write(peer, now_ms);
    }

    /// The neighbours this Mac relayed to within [`CURSOR_KEPT`] of `now_ms`: the cursors the
    /// tables cut keeps.
    pub(crate) fn recent(&self, now_ms: u64) -> Vec<String> {
        let kept = u64::try_from(CURSOR_KEPT.as_millis()).unwrap_or(u64::MAX);
        self.at
            .iter()
            .filter(|(_, (at, _))| now_ms.saturating_sub(*at) < kept)
            .map(|(peer, _)| peer.clone())
            .collect()
    }

    /// This Mac still relays to `peer` at `now_ms`, to the log it relayed to.
    pub(crate) fn touch(&mut self, peer: &str, now_ms: u64) {
        let Some(entry) = self.at.get_mut(peer) else {
            return;
        };
        entry.0 = now_ms;
        self.write(peer, now_ms);
    }

    /// Write the file where `peer`'s line changed its log, or was last written
    /// [`RELAYED_EVERY`] ago or more.
    fn write(&mut self, peer: &str, now_ms: u64) {
        let every = u64::try_from(RELAYED_EVERY.as_millis()).unwrap_or(u64::MAX);
        if let (Some((written, was)), Some((_, dest))) = (self.written.get(peer), self.at.get(peer))
            && was == dest
            && now_ms.saturating_sub(*written) < every
        {
            return;
        }
        use std::fmt::Write as _;
        let mut text = String::with_capacity(self.at.len() * 64);
        for (peer, (at, dest)) in &self.at {
            match dest {
                Some(dest) => {
                    let _ = writeln!(text, "{peer} {at} {dest}");
                }
                None => {
                    let _ = writeln!(text, "{peer} {at} -");
                }
            }
        }
        match aterm_uds::private_file::write_private(&self.dir, RELAYED_FILE, text.as_bytes()) {
            Ok(()) => self.written.clone_from(&self.at),
            Err(e) => aterm_log::warn!("mesh: {RELAYED_FILE} could not be written: {e}"),
        }
    }
}

/// Write `grants` at the head of a `Bus` stream this Mac accepted.
pub(crate) fn hand_grants(stream: &LinkStream, grants: &[Capability]) -> io::Result<()> {
    let mut text = String::with_capacity(GRANTS_HEAD.len() + 5 + grants.len() * 96);
    text.push_str(GRANTS_HEAD);
    text.push('\n');
    for cap in grants {
        text.push_str("cap ");
        text.push_str(&cap.filter);
        text.push(' ');
        aterm_codec::hex::push_lower(&mut text, &cap.tag);
        text.push('\n');
    }
    text.push_str("end\n");
    stream.set_write_timeout(Some(IO_WITHIN));
    let mut writer = stream;
    let wrote = writer.write_all(text.as_bytes());
    stream.set_write_timeout(None);
    wrote
}

fn damaged(why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("the grants at the head of the Bus stream: {why}"),
    )
}

/// One line of the grants.
fn grant_of(line: &str) -> io::Result<Capability> {
    let rest = line
        .strip_prefix("cap ")
        .ok_or_else(|| damaged("a line is no grant"))?;
    let (filter, tag) = rest
        .rsplit_once(' ')
        .ok_or_else(|| damaged("a grant has no tag"))?;
    let mut bytes = [0u8; 32];
    if filter.is_empty()
        || !aterm_codec::hex::is_lower(tag, 64)
        || !aterm_codec::hex::decode_into(tag.as_bytes(), &mut bytes)
    {
        return Err(damaged("a grant is malformed"));
    }
    Ok(Capability {
        filter: filter.to_owned(),
        tag: bytes,
    })
}

/// Read the grants at the head of a `Bus` stream this Mac opened: the grants, and the bytes read
/// past them, which belong to the stream.
pub(crate) fn read_grants(stream: &LinkStream) -> io::Result<(Vec<Capability>, Vec<u8>)> {
    let mut held = Vec::with_capacity(256);
    let mut scanned = 0usize;
    let mut chunk = [0u8; 1024];
    stream.set_read_timeout(Some(IO_WITHIN));
    let end = loop {
        if held.len() > GRANTS_MAX {
            stream.set_read_timeout(None);
            return Err(damaged("they run past their bound"));
        }
        let n = match stream.read_some(&mut chunk) {
            Ok(0) => {
                stream.set_read_timeout(None);
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the Bus stream ended before its grants",
                ));
            }
            Ok(n) => n,
            Err(e) => {
                stream.set_read_timeout(None);
                return Err(e);
            }
        };
        let start = scanned.saturating_sub(4);
        held.extend_from_slice(&chunk[..n]);
        if let Some(at) = held[start..]
            .windows(5)
            .position(|w| w == b"\nend\n")
            .map(|at| start + at + 5)
        {
            break at;
        }
        scanned = held.len();
    };
    stream.set_read_timeout(None);
    let ahead = held.split_off(end);
    let text = std::str::from_utf8(&held).map_err(|_| damaged("they are not text"))?;
    let mut lines = text.lines();
    if lines.next() != Some(GRANTS_HEAD) {
        return Err(damaged("they speak another version"));
    }
    let mut grants = Vec::with_capacity(GRANTS_COUNT_MAX);
    for line in lines.take_while(|line| *line != "end") {
        if grants.len() >= GRANTS_COUNT_MAX {
            return Err(damaged("there are too many"));
        }
        grants.push(grant_of(line)?);
    }
    Ok((grants, ahead))
}

/// A `Bus` stream as astream's [`StreamLink`]: what was read past the grants first, then the
/// stream. A read waits on the link's state with no lock of its own held, so a write and a
/// shutdown from other threads go on meanwhile, and a shutdown wakes a parked read.
pub struct BusLink {
    stream: LinkStream,
    has_ahead: AtomicBool,
    ahead: Mutex<Vec<u8>>,
}

impl BusLink {
    /// `stream`, whose first bytes are `ahead`.
    #[must_use]
    pub const fn new(stream: LinkStream, ahead: Vec<u8>) -> Self {
        Self {
            stream,
            has_ahead: AtomicBool::new(!ahead.is_empty()),
            ahead: Mutex::new(ahead),
        }
    }
}

impl StreamLink for BusLink {
    fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        if self.has_ahead.load(Ordering::Acquire) {
            let mut ahead = self.ahead.lock_or_recover();
            if !ahead.is_empty() {
                let n = buf.len().min(ahead.len());
                buf[..n].copy_from_slice(&ahead[..n]);
                ahead.drain(..n);
                if ahead.is_empty() {
                    self.has_ahead.store(false, Ordering::Release);
                }
                return Ok(n);
            }
            self.has_ahead.store(false, Ordering::Release);
        }
        self.stream.read_some(buf)
    }

    fn write(&self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write_some(buf)
    }

    fn flush(&self) -> io::Result<()> {
        Ok(())
    }

    fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(d);
        Ok(())
    }

    fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.stream.set_write_timeout(d);
        Ok(())
    }

    fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        match self.stream.shutdown(how) {
            // The link ended: the stream is closed both ways already.
            Err(e) if e.kind() == io::ErrorKind::ConnectionAborted => Ok(()),
            other => other,
        }
    }
}

/// Opens `Bus` streams for one log on one link: what a relay's destination dials.
pub struct BusDial {
    link: Arc<Link>,
    log: MeshLog,
}

impl BusDial {
    /// Streams of `log` over `link`.
    #[must_use]
    pub fn new(link: Arc<Link>, log: MeshLog) -> Self {
        Self { link, log }
    }

    /// Open one stream and read its grants.
    /// # Errors
    /// What [`Link::open`] answers ([`io::ErrorKind::PermissionDenied`] with the neighbour's
    /// reason where it refused), or grants that cannot be read.
    pub fn open(&self) -> io::Result<(BusLink, Vec<Capability>)> {
        let stream = self
            .link
            .open(FleetOp::Bus, Some(self.log.bus()), IO_WITHIN)?;
        let (grants, ahead) = read_grants(&stream)?;
        Ok((BusLink::new(stream, ahead), grants))
    }
}

impl StreamDial for BusDial {
    fn dial(&self) -> io::Result<Arc<dyn StreamLink>> {
        Ok(Arc::new(self.open()?.0))
    }
}

/// Streams this Mac's own broker adopts under this Mac's key: what a relay's source dials.
struct Adopting {
    broker: Arc<Broker>,
    key: [u8; 32],
}

impl StreamDial for Adopting {
    fn dial(&self) -> io::Result<Arc<dyn StreamLink>> {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
        drop(self.broker.serve_stream(Arc::new(theirs), self.key)?);
        Ok(Arc::new(ours))
    }
}

fn io_of(error: FederationError) -> io::Error {
    match error {
        FederationError::Io(e) => e,
        other => io::Error::other(other.to_string()),
    }
}

/// Whether a relayed record's refusal is for the record alone, not the connection: it is not its
/// origin's, its origin is over its subject budget there, or the neighbour does not take its origin
/// yet.
fn for_the_record_alone(error: &io::Error) -> bool {
    astream_broker::broker_refusal(error).is_some_and(|refusal| {
        refusal.code == astream_broker::proto::ERR_ORIGIN_REFUSED
            || refusal.is_subject_bound()
            || refusal.is_origin_unknown()
    })
}

/// A client over `link`, holding `grants`, each read and write bounded by [`IO_WITHIN`].
fn client_over(link: Arc<dyn StreamLink>, grants: &[Capability]) -> io::Result<AnyClient> {
    link.set_read_timeout(Some(IO_WITHIN))?;
    link.set_write_timeout(Some(IO_WITHIN))?;
    let (mut client, _closer) = astream_broker::connect_stream(link);
    client.attach_all(grants.iter().map(|cap| (cap.filter.as_str(), &cap.tag[..])))?;
    Ok(client)
}

/// The highest sequence number below the ack half that the neighbour behind `client` holds of this
/// Mac on `log`, read with the grant it handed this Mac for its own subjects. Only a record this
/// Mac's key signed counts: a neighbour cannot push the lease up with a number it made up.
fn high_water(me: &Me, log: MeshLog, mut client: AnyClient) -> io::Result<Option<u64>> {
    client.origin_stamps()?;
    client.burst_marks()?;
    let mut sub = client.subscribe(0, &log.own_filter(&me.principal))?;
    let mut high = None;
    loop {
        match sub.recv_event()? {
            Some(astream_broker::Event::Delivery { subject, body, .. }) => {
                if let Some((pid, seq)) = sub.last_origin()
                    && pid == me.pid
                    && seq < astream_broker::ACK_SEQ_BASE
                    && astream_broker::origin::verify(&me.key, &subject, seq, &body)
                {
                    high = high.max(Some(seq));
                }
            }
            Some(astream_broker::Event::Mark { next, head, .. }) if next >= head => {
                return Ok(high);
            }
            Some(_) => {}
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the neighbour ended the high-water read",
                ));
            }
        }
    }
}

/// One newest row, with its origin's key.
struct Row {
    pid: u64,
    seq: u64,
    subject: String,
    body: Vec<u8>,
}

/// The newest row of every `now` and `quota` subject `log` holds, each with its origin's key.
fn newest_rows(mesh: &Mesh, log: MeshLog) -> io::Result<Vec<Row>> {
    let hosted = mesh.hosted(log);
    let mut rows = Vec::new();
    if log != MeshLog::Metrics {
        return Ok(rows);
    }
    for filter in [
        aterm_metrics::rules::NOW_FILTER,
        aterm_metrics::rules::QUOTA_FILTER,
    ] {
        let grant = [hosted.mint(&format!("ro:{filter}"))?];
        let mut reader = client_over(hosted.adopt(mesh.me.key)?, &grant)?;
        let (last, _) = reader.last_all(filter)?;
        let wanted: BTreeSet<u64> = last.iter().map(|(offset, _, _)| *offset).collect();
        rows.extend(stamped(reader, filter, &wanted)?);
    }
    Ok(rows)
}

/// The records at `wanted` offsets of `client`'s log under `filter`, each with its origin's key:
/// an offset that holds no such record (a commit, a subject outside `filter`) yields nothing.
fn stamped(mut client: AnyClient, filter: &str, wanted: &BTreeSet<u64>) -> io::Result<Vec<Row>> {
    let mut rows = Vec::new();
    let (Some(from), Some(upto)) = (wanted.first().copied(), wanted.last().copied()) else {
        return Ok(rows);
    };
    client.origin_stamps()?;
    client.burst_marks()?;
    let mut sub = client.subscribe(from, filter)?;
    loop {
        match sub.recv_event()? {
            Some(astream_broker::Event::Delivery {
                offset,
                subject,
                body,
            }) => {
                if wanted.contains(&offset)
                    && let Some((pid, seq)) = sub.last_origin()
                {
                    rows.push(Row {
                        pid,
                        seq,
                        subject,
                        body,
                    });
                }
                if offset >= upto {
                    return Ok(rows);
                }
            }
            Some(astream_broker::Event::Mark { next, .. }) if next > upto => return Ok(rows),
            Some(_) => {}
            None => return Ok(rows),
        }
    }
}

/// Relay `rows` to the neighbour behind `dial`, each under its origin's key: a row the neighbour
/// refuses for itself alone is skipped. They are the newest rows of a relay that starts at its
/// log's head or after a fork, below where its group starts: what they miss is `Inventory`'s.
fn send_rows(dial: &BusDial, grants: &[Capability], rows: &[Row]) -> io::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let (link, _) = dial.open()?;
    let mut dest = client_over(Arc::new(link), grants)?;
    for row in rows {
        match dest.relay(row.pid, row.seq, &row.subject, &row.body) {
            Ok(_) => {}
            Err(e) if for_the_record_alone(&e) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// What one relay is: its neighbour, its log, the link it rides, and how it is stopped or kicked.
pub(crate) struct Slot {
    pub(crate) peer: String,
    pub(crate) log: MeshLog,
    pub(crate) link: Arc<Link>,
    /// The relay follows its log's head now.
    pub(crate) following: AtomicBool,
    stop: AtomicBool,
    kicked: AtomicBool,
    bell: Arc<Bell>,
    closer: Mutex<Option<FederationBridgeCloser>>,
}

impl Slot {
    pub(crate) fn new(peer: String, log: MeshLog, link: Arc<Link>) -> Arc<Self> {
        Arc::new(Self {
            peer,
            log,
            link,
            following: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            kicked: AtomicBool::new(false),
            bell: Arc::new(Bell::default()),
            closer: Mutex::new(None),
        })
    }

    /// End the relay: a running bridge closes, a waiting one wakes.
    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(closer) = self.closer.lock_or_recover().as_ref() {
            closer.close();
        }
        self.bell.ring();
    }

    /// Cut this relay's back-off to its first step, from its last try's start.
    pub(crate) fn kick(&self) {
        self.kicked.store(true, Ordering::Release);
        self.bell.ring();
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

/// A tables relay that follows now: its neighbour speaks `Bus` with this Mac while one does
/// ([`Mesh::speaks_bus`]), and a push waits for its ack ([`Mesh::push_table`]).
struct Live<'a> {
    mesh: &'a Mesh,
    peer: &'a str,
}

impl<'a> Live<'a> {
    fn new(mesh: &'a Mesh, peer: &'a str, key: [u8; 32]) -> Self {
        mesh.heard
            .lock_or_recover()
            .entry(peer.to_owned())
            .or_insert((key, 0))
            .1 += 1;
        Self { mesh, peer }
    }
}

impl Drop for Live<'_> {
    fn drop(&mut self) {
        let mut heard = self.mesh.heard.lock_or_recover();
        if let Some((_, count)) = heard.get_mut(self.peer) {
            *count -= 1;
            if *count == 0 {
                heard.remove(self.peer);
            }
        }
    }
}

/// Run the relay `slot` names until it is stopped, its link ends, or the mesh closes; the overlay
/// is poked as it ends.
pub(crate) fn run(mesh: &Arc<Mesh>, slot: &Slot) {
    let live = || !slot.stopped() && !mesh.is_stopped() && slot.link.closed().is_none();
    let mut backoff = BACKOFF_FIRST;
    'relay: while live() {
        slot.kicked.store(false, Ordering::Release);
        let started = mesh.clock.now();
        let ran = relay_once(mesh, slot);
        slot.following.store(false, Ordering::Release);
        mesh.report(&Event::Ended {
            peer: slot.peer.clone(),
            log: slot.log,
            why: ran
                .as_ref()
                .err()
                .map_or_else(|| "closed".to_owned(), ToString::to_string),
        });
        let Err(why) = ran else {
            break;
        };
        if !live() {
            break;
        }
        aterm_log::info!(
            "mesh: the {} relay to {} ended ({why}); it tries again",
            slot.log.word(),
            slot.peer
        );
        let mut kicked = false;
        loop {
            kicked |= slot.kicked.swap(false, Ordering::AcqRel);
            if !live() {
                break 'relay;
            }
            let until = retry_at(started, backoff, kicked);
            if mesh.clock.now() >= until {
                break;
            }
            mesh.clock.park(until, &slot.bell);
        }
        backoff = next_backoff(backoff, kicked);
    }
    mesh.poke();
}

/// When a relay whose try `started` and failed tries again: its `backoff` after that start, so
/// tries are spaced by it however long each ran. A kick cuts it to the first step, never shorter:
/// a kick says the neighbour may take the relay now, not that it did, and two relays that fail
/// after each other's accept kick each other on every try.
pub(crate) fn retry_at(started: Duration, backoff: Duration, kicked: bool) -> Duration {
    started.saturating_add(if kicked {
        backoff.min(BACKOFF_FIRST)
    } else {
        backoff
    })
}

/// The back-off after a failed try: doubled up to [`BACKOFF_MAX`], or the first step again
/// where a kick came while it waited.
pub(crate) fn next_backoff(backoff: Duration, kicked: bool) -> Duration {
    if kicked {
        BACKOFF_FIRST
    } else {
        backoff.saturating_mul(2).min(BACKOFF_MAX)
    }
}

/// One run of a relay: open, settle a fresh lease, resume the cursor or start at the head, then
/// follow.
fn relay_once(mesh: &Arc<Mesh>, slot: &Slot) -> io::Result<()> {
    let hosted = mesh.hosted(slot.log);
    let dial = Arc::new(BusDial::new(Arc::clone(&slot.link), slot.log));
    let (probe, grants) = dial.open()?;
    let probe = client_over(Arc::new(probe), &grants)?;
    // The neighbour's log, and how its generations agree, as its broker answered the attach.
    let dest = probe.seen_log_identity();
    let lineage = probe.log_lineage().cloned();
    if hosted.origin.lock_or_recover().is_settled() {
        drop(probe);
    } else {
        let high = high_water(&mesh.me, slot.log, probe)?;
        let mut origin = hosted.origin.lock_or_recover();
        origin.settle(high)?;
        let incarnation = origin.incarnation();
        drop(origin);
        mesh.report(&Event::Settled {
            log: slot.log,
            incarnation,
            high,
        });
    }
    if slot.stopped() {
        return Ok(());
    }
    let group = group_of(&slot.peer);
    let cursor = hosted.relayed.lock_or_recover().cursor(
        &slot.peer,
        dest,
        lineage.as_ref(),
        wall_ms(&mesh.wall),
    );
    match cursor {
        Cursor::Kept => {}
        Cursor::Moved => send_newest(mesh, slot, &dial, &grants)?,
        Cursor::Gone => start_at_head(mesh, slot, &dial, &grants, &group)?,
    }
    if slot.stopped() {
        return Ok(());
    }
    let cfg = FederationConfig {
        source: FederationEndpoint::stream(
            mesh.principal(),
            Arc::new(Adopting {
                broker: Arc::clone(&hosted.broker),
                key: mesh.me.key,
            }),
        )
        .with_caps(hosted.source_grants(&mesh.me)?),
        dest: FederationEndpoint::stream(&slot.peer, dial).with_caps(grants),
        group,
        filter: slot.log.filter().to_owned(),
        producer_id: 0,
        rewrite: SubjectRewrite::passthrough(),
        relay: true,
    };
    let mut bridge = FederationBridge::open(cfg).map_err(io_of)?;
    let seats = Arc::downgrade(mesh);
    bridge.hold_while(Arc::new(move |owner: &str| {
        seats.upgrade().is_none_or(|mesh| mesh.still_seats(owner))
    }));
    {
        let mut closer = slot.closer.lock_or_recover();
        if slot.stopped() {
            return Ok(());
        }
        *closer = Some(bridge.closer());
    }
    hosted
        .relayed
        .lock_or_recover()
        .relays_to(&slot.peer, dest, wall_ms(&mesh.wall));
    slot.following.store(true, Ordering::Release);
    mesh.report(&Event::Following {
        peer: slot.peer.clone(),
        log: slot.log,
    });
    let inventory = (slot.log == MeshLog::Tables)
        .then(|| mesh.inventory())
        .flatten();
    let _live = inventory
        .is_some()
        .then(|| Live::new(mesh, &slot.peer, *slot.link.peer().key.bytes()));
    if let Some(inventory) = &inventory {
        inventory.linked(&slot.peer, &slot.link);
        // What the relay cannot carry: the records before the head it starts at (no cursor), or
        // the last ones a later generation of the neighbour's log may have lost.
        match cursor {
            Cursor::Kept => {}
            Cursor::Moved => inventory.request(&slot.peer, &slot.link, Reason::Lineage),
            Cursor::Gone => inventory.request(&slot.peer, &slot.link, Reason::NewLink),
        }
    }
    let mut held = 0;
    bridge
        .run(BURST, &mut |report| {
            if let Some(inventory) = &inventory {
                if report.mirrored > 0 {
                    mesh.acked();
                }
                for gap in &report.gaps {
                    mesh.report(&Event::Gap {
                        peer: slot.peer.clone(),
                        log: slot.log,
                        from: gap.from,
                        to: gap.to,
                    });
                }
                if !report.gaps.is_empty() {
                    inventory.request(&slot.peer, &slot.link, Reason::Gap);
                }
            }
            hosted
                .relayed
                .lock_or_recover()
                .touch(&slot.peer, wall_ms(&mesh.wall));
            if report.mirrored > 0 || report.refused > 0 || report.held != held {
                held = report.held;
                mesh.report(&Event::Burst {
                    peer: slot.peer.clone(),
                    log: slot.log,
                    appended: report.mirrored.saturating_sub(report.deduped),
                    deduped: report.deduped,
                    refused: report.refused,
                    held: report.held,
                });
            }
        })
        .map_err(io_of)
}

/// Send `slot`'s log's newest rows, each with its origin's key ([`newest_rows`]), where it has
/// any.
fn send_newest(mesh: &Mesh, slot: &Slot, dial: &BusDial, grants: &[Capability]) -> io::Result<()> {
    let rows = newest_rows(mesh, slot.log)?;
    if !rows.is_empty() {
        send_rows(dial, grants, &rows)?;
        mesh.report(&Event::NewestFirst {
            peer: slot.peer.clone(),
            log: slot.log,
            rows: rows.len(),
        });
    }
    Ok(())
}

/// A relay with no cursor kept: send the log's newest rows, each with its origin's key, then
/// commit the neighbour's group through the last record before them, so the relay follows from
/// the head as it was read: a record that lands while the rows go is relayed after them.
fn start_at_head(
    mesh: &Mesh,
    slot: &Slot,
    dial: &BusDial,
    grants: &[Capability],
    group: &str,
) -> io::Result<()> {
    let hosted = mesh.hosted(slot.log);
    let mut local = client_over(hosted.adopt(mesh.me.key)?, &hosted.source_grants(&mesh.me)?)?;
    let (_, (_, head)) = local.fetch(0, slot.log.filter(), 0)?;
    send_newest(mesh, slot, dial, grants)?;
    local.commit_before(group, head)?;
    Ok(())
}
