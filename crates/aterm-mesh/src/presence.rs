// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Presence (B7c, `docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.6, "Presence"; §4.12; §10,
//! "Presence"): each Mac's one row `/m/n-<id>/shared/now/presence` on `metrics.log`, and the
//! per-seat reading of every Mac's row that the fleet view (§4.12, B9) shows.
//!
//! **The row** ([`PresenceRow`]) holds this Mac's build, its interface addresses, and the digest
//! and count of its membership log and of its lab tables ([`Held`]). The node service writes it
//! every 30 seconds ([`EVERY`]) through [`Mesh::publish`], so it is signed by the node key and
//! numbered under the log's incarnation lease, and the relay carries it to every Mac. A `now`
//! subject keeps the row with the highest sequence number (`aterm_metrics::rules`, AS1's
//! monotone `Last`), so a late, older row never replaces a newer one on any broker, and a
//! restarted Mac's first row wins at once: its incarnation is new, so its number is higher.
//!
//! **Up is read from rows, never from a probe.** One tail of this Mac's own `metrics.log` takes
//! every presence row as it lands, from a relay or from this Mac's own publisher, and the
//! [`Board`] judges it. A seat is **up** while its row *advanced* within the last 90 seconds
//! ([`UP_FOR`]) of this Mac's clock (the links' clock, injected in a test). Nothing dials,
//! asks or times out to find a Mac down: a Mac that stops pushing reads silent once 90 seconds
//! pass, with the time this Mac last saw it up.
//!
//! A row *advances* a seat when it is above the row held for that seat, it arrived after this
//! Mac's tail started, and its origin timestamp is within 10 minutes ([`FRESH`]) of this Mac's
//! wall clock. The last condition is this step's answer to a gap in §4.6: a Mac that was away gets
//! the rows it missed in a burst (a relay that resumes its cursor delivers every row that landed
//! in the last 7 days, and a new connection sends the newest rows first), and a burst's arrival
//! says nothing about whether their origin is up now. Without it, a Mac back from a weekend
//! would read every Mac that went silent while it slept as up for 90 seconds. The origin's clock
//! decides only that a row is not a backlog; how long a seat has been silent is measured on this
//! Mac's clock alone (§12, "Ages use the origin's clock"). A Mac whose clock differs by more than
//! 10 minutes from this one's reads silent here; `STALE_ROWS_UP` (`stale-rows-up`) is the
//! rule's negative control.
//!
//! **Last seen persists per seat.** The time (this Mac's wall clock) a seat last advanced is
//! kept in `<state_dir>/mesh/seen` ([`SEEN_FILE`]), one line per seat, written when it changed
//! and at most once per 10 minutes ([`SEEN_EVERY`]), so "silent for 3 days" survives a restart
//! to within 10 minutes. It lives beside the two logs' folders, not in either, so a renewed log
//! keeps it. Only the current seats' lines are kept: a removed or re-keyed seat's line goes.
//!
//! **A row that differs asks an exchange** (PF5b's finding H1, §4.6's gap notice). Each live,
//! fresh row of another Mac is handed, with its membership and tables digests and this Mac's own
//! latest row's, to the `Inventory` digest trigger ([`crate::inventory::Inventory::digest_row`]),
//! which asks an exchange with that neighbour where one store's two digests differ two rows in a
//! row while both relays are caught up: a record that reached one Mac by `Inventory` is in no log,
//! so no relay carries it on, and this is what does. The two digests name every record an exchange
//! moves: the membership log's records, and each table's operations and conflict records
//! ([`Held::tables`]). `NO_DIGEST_TRIGGER` is its negative control.
//!
//! **A row is its origin's alone.** The tail judges each row again, beside the broker's own
//! check of the relay: its subject must name a current seat (or this Mac), carry that seat's
//! producer id, and verify under that seat's node key, and its body must be a presence row. So a
//! removed Mac's row is refused here even where it is still held (a retirement that has not run
//! yet, or a log read at a start before the removal reached it), and no relaying Mac can forge
//! another's presence.

use crate::inventory::Holds;
use crate::logs::{Mesh, MeshLog};
use crate::relay;
use aterm_digest::Sha256;
use aterm_metrics::subject::{Node, Now, Subject};
use aterm_net::fleet::link::Bell;
use aterm_types::MutexExt as _;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// How often the node service writes this Mac's row.
pub const EVERY: Duration = Duration::from_secs(30);

/// A seat is up while its row advanced within this much of this Mac's clock.
pub const UP_FOR: Duration = Duration::from_secs(90);

/// The allowed difference between a row's origin timestamp and the reader's wall clock.
/// A row outside this window is held, but advances nothing.
pub const FRESH: Duration = Duration::from_secs(10 * 60);

/// The seen file is written at most once per this much of this Mac's clock.
pub const SEEN_EVERY: Duration = Duration::from_secs(10 * 60);

/// The file, in the mesh folder (`<state_dir>/mesh/`), that holds when each seat was last seen
/// up: `n-<16 hex> <unix ms>` per line.
pub const SEEN_FILE: &str = "seen";

/// The most seats the seen file holds.
pub const SEEN_MAX: usize = 4096;

/// The seen file's bytes, at most: [`SEEN_MAX`] lines.
const SEEN_BYTES_MAX: usize = SEEN_MAX * 40;

/// Every Mac's presence row.
pub const FILTER: &str = "/m/*/shared/now/presence";

/// The kind byte of a presence row. aterm-metrics' rows take `Q`, `A`, `P`, `N`, `U` and `W`.
pub const KIND: u8 = b'R';

/// The one version of the row this build writes and reads: 2 adds the held topics (§11 C3).
pub const VERSION: u8 = 2;

/// The most held topics a row names; a writer names its first ones, in order.
pub const TOPICS_MAX: usize = 32;

/// The most interface addresses a row holds; a writer writes its first ones.
pub const ADDRESSES_MAX: usize = 16;

/// The longest build text, in bytes.
pub const BUILD_MAX: usize = 64;

/// The first wait before the tail opens again after it failed, doubled up to
/// [`READ_RETRY_MAX`].
const READ_RETRY_FIRST: Duration = Duration::from_secs(1);

/// The longest wait between the tail's tries.
const READ_RETRY_MAX: Duration = Duration::from_secs(60);

/// The refusals the journal names, at most, in one run: each `(subject, reason)` once.
const SAID_MAX: usize = 64;

/// How many of this Mac's own holdings the board remembers ([`Behind`]): each time a row of its
/// own says it holds other records than its row before, one more. At one change a period it
/// spans 32 minutes; a seat that lags further reads [`Behind::Apart`].
pub const OWN_KEPT: usize = 64;

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The negative control of the freshness rule (`stale-rows-up`): a board made on a thread
    /// that sets it takes a row's arrival as an advance however long ago its origin wrote it,
    /// as §4.6's rule read alone would, so a Mac back from sleep reads every silent seat whose
    /// backlog it receives as up. A lab instance turns it on for its whole process with
    /// `stale-rows-up` in `ATERM_TEST_NEGATIVE_CONTROL`.
    pub static STALE_ROWS_UP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether [`STALE_ROWS_UP`] is on: set on this thread, or named for this process.
#[cfg(any(test, debug_assertions))]
fn stale_rows_up() -> bool {
    STALE_ROWS_UP.with(std::cell::Cell::get) || aterm_types::negative_control::on("stale-rows-up")
}

// ---------------------------------------------------------------------------
// The row
// ---------------------------------------------------------------------------

/// A build, as `aterm --version`'s token names it (`<version>+g<commit>`): 1..=[`BUILD_MAX`]
/// bytes of `[0-9A-Za-z._+-]`, starting with a letter or digit. It cannot spell a path or a
/// sentence.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Build(Box<str>);

impl Build {
    /// `text`, when the grammar takes it.
    #[must_use]
    pub fn new(text: &str) -> Option<Self> {
        let b = text.as_bytes();
        let first = b.first().is_some_and(u8::is_ascii_alphanumeric);
        let rest = b
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'+' | b'-'));
        (first && rest && b.len() <= BUILD_MAX).then(|| Self(text.into()))
    }

    /// The build this process runs (`aterm_stamp::build_id`, `<version>+g<commit>`), or
    /// `unstamped` where that is not a build word.
    #[must_use]
    pub fn running() -> Self {
        Self::new(&aterm_stamp::build_id()).unwrap_or_else(|| Self("unstamped".into()))
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Its release, `(major, minor)` of `MAJOR.MINOR.PATCH` with optional `+<build metadata>`
    /// (`0.147.0+g1a2b3c4d` is `(0, 147)`); `None` for a build this cannot read as one.
    #[must_use]
    pub fn release(&self) -> Option<(u64, u64)> {
        let core = match self.0.split_once('+') {
            Some((core, meta))
                if !meta.is_empty()
                    && meta
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')) =>
            {
                core
            }
            Some(_) => return None,
            None => &self.0,
        };
        let number = |part: &str| {
            (!part.is_empty()
                && (part.len() == 1 || !part.starts_with('0'))
                && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse::<u64>().ok())
            .flatten()
        };
        let mut parts = core.split('.');
        match (
            parts.next().and_then(number),
            parts.next().and_then(number),
            parts.next().and_then(number),
            parts.next(),
        ) {
            (Some(major), Some(minor), Some(_), None) => Some((major, minor)),
            _ => None,
        }
    }
}

impl fmt::Display for Build {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a Mac holds of one store, as its row says: how many records, and a digest of which.
/// Two Macs whose `Held` is equal hold the same records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Held {
    /// SHA-256 under the store's domain over what it holds, in a canonical order.
    pub digest: [u8; 32],
    /// The records held.
    pub count: u64,
}

/// The membership digest's domain.
const MEMBERSHIP_DOMAIN: &[u8] = b"aterm/mesh/presence/membership/1\0";

/// The tables digest's domain.
const TABLES_DOMAIN: &[u8] = b"aterm/mesh/presence/tables/1\0";

/// The domain of one table's conflict records' digest.
const CONFLICTS_DOMAIN: &[u8] = b"aterm/mesh/presence/conflicts/1\0";

/// Hash `tables` into `h`: their number, then each one's identity, count and digest.
fn put_tables(h: &mut Sha256, tables: &BTreeMap<[u8; 32], Held>) {
    h.update(
        u64::try_from(tables.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    for (table, held) in tables {
        h.update(table);
        h.update(held.count.to_le_bytes());
        h.update(held.digest);
    }
}

impl Held {
    /// The membership log's: every record id it holds, verified (`Store::inventory`), in order.
    #[must_use]
    pub fn membership(ids: &BTreeSet<[u8; 32]>) -> Self {
        let count = u64::try_from(ids.len()).unwrap_or(u64::MAX);
        let mut h = Sha256::new();
        h.update(MEMBERSHIP_DOMAIN);
        h.update(count.to_le_bytes());
        for id in ids {
            h.update(id);
        }
        Self {
            digest: h.finalize(),
            count,
        }
    }

    /// One table's: the operation ids it retains, as aterm-db's `InventorySummary` counts and
    /// fingerprints them, so a row's table equals what a Commons repair report says of it.
    #[must_use]
    pub fn table(ids: &BTreeSet<[u8; 32]>) -> Self {
        let summary = aterm_db::replica::InventorySummary::of(ids);
        Self {
            digest: summary.fingerprint,
            count: u64::try_from(summary.count).unwrap_or(u64::MAX),
        }
    }

    /// One table's conflict records: the ids (each record's wire digest) of the conflict
    /// certificates its store holds, in order.
    #[must_use]
    pub fn conflicts(ids: &BTreeSet<[u8; 32]>) -> Self {
        let count = u64::try_from(ids.len()).unwrap_or(u64::MAX);
        let mut h = Sha256::new();
        h.update(CONFLICTS_DOMAIN);
        h.update(count.to_le_bytes());
        for id in ids {
            h.update(id);
        }
        Self {
            digest: h.finalize(),
            count,
        }
    }

    /// The lab tables': each table's own count and fingerprint of the operation ids it retains
    /// (`operations`, each [`Self::table`]), and of the conflict records it holds (`conflicts`,
    /// each [`Self::conflicts`]), both keyed by the table's identity digest (`Scope::digest`).
    /// The count is every table's operations and conflict records.
    ///
    /// The SHA-256 runs over the domain and `operations` (their number, then each table's
    /// identity, count and digest), then over `conflicts` the same way only where it names a
    /// table. So the encoding reads one way (the first part's length is its number's; the second
    /// is there or not), and a lab that holds no conflict record reads as it did before conflict
    /// records were counted, on every build.
    #[must_use]
    pub fn tables(
        operations: &BTreeMap<[u8; 32], Self>,
        conflicts: &BTreeMap<[u8; 32], Self>,
    ) -> Self {
        let count = operations
            .values()
            .chain(conflicts.values())
            .fold(0u64, |sum, held| sum.saturating_add(held.count));
        let mut h = Sha256::new();
        h.update(TABLES_DOMAIN);
        put_tables(&mut h, operations);
        if !conflicts.is_empty() {
            put_tables(&mut h, conflicts);
        }
        Self {
            digest: h.finalize(),
            count,
        }
    }
}

/// One Mac's presence: what it is running and where, and what it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresenceRow {
    /// When its origin wrote it, by the origin's own clock (Unix milliseconds): what a view
    /// shows as the row's age, and what tells a backlog from a live row ([`FRESH`]). How long a
    /// seat has been silent is never read from it.
    pub at_ms: u64,
    /// The build it runs.
    pub build: Build,
    /// Where it listens: each interface address with its listener's port. An IPv6 address
    /// travels without its zone.
    pub addresses: Vec<SocketAddr>,
    /// Its membership log; `None` where it could not read one.
    pub membership: Option<Held>,
    /// Its lab tables; `None` where it could not read them.
    pub tables: Option<Held>,
    /// The topics its sessions hold, in order, each once: a Mac whose row names one gets the
    /// topic broadcasts of every other Mac, one hop (§4.11, §11 C3).
    pub topics: Vec<String>,
}

/// The topics a row names of `held`: each a topic word, in order, each once, the first
/// [`TOPICS_MAX`].
#[must_use]
pub fn row_topics<'a>(held: impl IntoIterator<Item = &'a String>) -> Vec<String> {
    held.into_iter()
        .map(String::as_str)
        .filter(|topic| aterm_types::fabric_grammar::is_topic(topic))
        .collect::<BTreeSet<&str>>()
        .into_iter()
        .take(TOPICS_MAX)
        .map(str::to_owned)
        .collect()
}

/// Why bytes are not a presence row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowError {
    /// The bytes end inside a field.
    Truncated,
    /// Bytes after the last field.
    Trailing,
    /// The kind byte names another row.
    Kind(u8),
    /// A version this build does not read.
    Version(u8),
    /// A number that is not a minimal 64-bit varint.
    Varint,
    /// A flag byte other than 0 or 1.
    Tag,
    /// A build text its grammar refuses.
    Build,
    /// An address family byte other than 4 or 6.
    Family(u8),
    /// More addresses than [`ADDRESSES_MAX`].
    TooMany,
    /// A topic outside its grammar, out of order or twice, or more than [`TOPICS_MAX`].
    Topic,
}

impl fmt::Display for RowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("the row ends inside a field"),
            Self::Trailing => f.write_str("bytes after the row's last field"),
            Self::Kind(found) => write!(
                f,
                "a {:?} row where a presence row belongs",
                char::from(*found)
            ),
            Self::Version(v) => write!(f, "presence row version {v}, this build reads {VERSION}"),
            Self::Varint => f.write_str("a number that is not a minimal 64-bit varint"),
            Self::Tag => f.write_str("a flag byte other than 0 or 1"),
            Self::Build => f.write_str("the build is not in its grammar"),
            Self::Family(b) => write!(f, "address family {b}, not 4 or 6"),
            Self::TooMany => write!(f, "more than {ADDRESSES_MAX} addresses"),
            Self::Topic => write!(
                f,
                "a topic outside its grammar, out of order or twice, or more than {TOPICS_MAX}"
            ),
        }
    }
}

impl std::error::Error for RowError {}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let low = u8::try_from(v & 0x7f).unwrap_or(0);
        v >>= 7;
        if v == 0 {
            out.push(low);
            return;
        }
        out.push(low | 0x80);
    }
}

fn put_held(out: &mut Vec<u8>, held: Option<Held>) {
    out.push(u8::from(held.is_some()));
    if let Some(held) = held {
        out.extend_from_slice(&held.digest);
        put_varint(out, held.count);
    }
}

/// The strict reader of one row.
struct In<'a>(&'a [u8]);

impl<'a> In<'a> {
    fn byte(&mut self) -> Result<u8, RowError> {
        let (&b, rest) = self.0.split_first().ok_or(RowError::Truncated)?;
        self.0 = rest;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], RowError> {
        let (head, rest) = self.0.split_at_checked(n).ok_or(RowError::Truncated)?;
        self.0 = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], RowError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn varint(&mut self) -> Result<u64, RowError> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            let low = u64::from(b & 0x7f);
            if shift == 63 && low > 1 {
                return Err(RowError::Varint);
            }
            v |= low << shift;
            if b & 0x80 == 0 {
                // A last byte of zero after the first is a longer spelling of a smaller number.
                if b == 0 && shift > 0 {
                    return Err(RowError::Varint);
                }
                return Ok(v);
            }
        }
        Err(RowError::Varint)
    }

    fn flag(&mut self) -> Result<bool, RowError> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(RowError::Tag),
        }
    }

    fn held(&mut self) -> Result<Option<Held>, RowError> {
        if !self.flag()? {
            return Ok(None);
        }
        let digest = self.array::<32>()?;
        let count = self.varint()?;
        Ok(Some(Held { digest, count }))
    }

    fn address(&mut self) -> Result<SocketAddr, RowError> {
        let ip = match self.byte()? {
            4 => IpAddr::V4(Ipv4Addr::from(self.array::<4>()?)),
            6 => IpAddr::V6(Ipv6Addr::from(self.array::<16>()?)),
            family => return Err(RowError::Family(family)),
        };
        let port = u16::from_be_bytes(self.array::<2>()?);
        Ok(SocketAddr::new(ip, port))
    }
}

impl PresenceRow {
    /// The canonical encoding:
    ///
    /// ```text
    /// 'R' 2 | at_ms | build (length byte, bytes) | n | n × (4 | 4 bytes | 6 | 16 bytes, then port BE)
    ///       | ?membership (32 bytes | count) | ?tables (32 bytes | count)
    ///       | n | n × topic (length byte, bytes)
    /// ```
    ///
    /// Numbers are minimal unsigned LEB128 varints and every option a tag byte (0 none, 1 some).
    /// Past [`ADDRESSES_MAX`] the first addresses are written, and a zone is not.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let addresses = &self.addresses[..self.addresses.len().min(ADDRESSES_MAX)];
        let mut out = Vec::with_capacity(100 + addresses.len() * 19);
        out.extend_from_slice(&[KIND, VERSION]);
        put_varint(&mut out, self.at_ms);
        let build = self.build.as_str().as_bytes();
        out.push(u8::try_from(build.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(build);
        put_varint(&mut out, u64::try_from(addresses.len()).unwrap_or(0));
        for address in addresses {
            match address.ip() {
                IpAddr::V4(ip) => {
                    out.push(4);
                    out.extend_from_slice(&ip.octets());
                }
                IpAddr::V6(ip) => {
                    out.push(6);
                    out.extend_from_slice(&ip.octets());
                }
            }
            out.extend_from_slice(&address.port().to_be_bytes());
        }
        put_held(&mut out, self.membership);
        put_held(&mut out, self.tables);
        if self.topics.len() <= TOPICS_MAX
            && self
                .topics
                .iter()
                .all(|topic| aterm_types::fabric_grammar::is_topic(topic))
            && self.topics.windows(2).all(|w| w[0] < w[1])
        {
            put_varint(&mut out, u64::try_from(self.topics.len()).unwrap_or(0));
            for topic in &self.topics {
                out.push(u8::try_from(topic.len()).unwrap_or(u8::MAX));
                out.extend_from_slice(topic.as_bytes());
            }
        } else {
            let topics: BTreeSet<&str> = self
                .topics
                .iter()
                .map(String::as_str)
                .filter(|topic| aterm_types::fabric_grammar::is_topic(topic))
                .collect();
            let topic_count = topics.len().min(TOPICS_MAX);
            put_varint(&mut out, u64::try_from(topic_count).unwrap_or(0));
            for topic in topics.into_iter().take(TOPICS_MAX) {
                out.push(u8::try_from(topic.len()).unwrap_or(u8::MAX));
                out.extend_from_slice(topic.as_bytes());
            }
        }
        out
    }

    /// Read a row. Decoding is strict: another kind, an unknown version, a truncation, trailing
    /// bytes, a non-minimal varint, a flag other than 0 or 1, a build its grammar refuses, too
    /// many addresses, or topics that are not in their grammar, in order and once each is
    /// refused, never misread.
    /// # Errors
    /// [`RowError`] naming the first field refused.
    pub fn decode(bytes: &[u8]) -> Result<Self, RowError> {
        let mut r = In(bytes);
        let kind = r.byte()?;
        if kind != KIND {
            return Err(RowError::Kind(kind));
        }
        let version = r.byte()?;
        if version != VERSION {
            return Err(RowError::Version(version));
        }
        let at_ms = r.varint()?;
        let len = usize::from(r.byte()?);
        let build = std::str::from_utf8(r.take(len)?)
            .ok()
            .and_then(Build::new)
            .ok_or(RowError::Build)?;
        let n = r.varint()?;
        if n > u64::try_from(ADDRESSES_MAX).unwrap_or(u64::MAX) {
            return Err(RowError::TooMany);
        }
        let addresses = (0..n).map(|_| r.address()).collect::<Result<Vec<_>, _>>()?;
        let membership = r.held()?;
        let tables = r.held()?;
        let n = r.varint()?;
        if n > u64::try_from(TOPICS_MAX).unwrap_or(u64::MAX) {
            return Err(RowError::Topic);
        }
        let mut topics: Vec<String> = Vec::with_capacity(usize::try_from(n).unwrap_or(0));
        for _ in 0..n {
            let len = usize::from(r.byte()?);
            let topic = std::str::from_utf8(r.take(len)?).map_err(|_| RowError::Topic)?;
            if !aterm_types::fabric_grammar::is_topic(topic)
                || topics.last().is_some_and(|last| last.as_str() >= topic)
            {
                return Err(RowError::Topic);
            }
            topics.push(topic.to_owned());
        }
        if !r.0.is_empty() {
            return Err(RowError::Trailing);
        }
        Ok(Self {
            at_ms,
            build,
            addresses,
            membership,
            tables,
            topics,
        })
    }

    /// Whether `other` holds what this row holds: the same membership log and the same lab
    /// tables, each known on both (§4.12, "In step").
    #[must_use]
    pub fn in_step_with(&self, other: &Self) -> bool {
        let same = |a: Option<Held>, b: Option<Held>| a.is_some() && a == b;
        same(self.membership, other.membership) && same(self.tables, other.tables)
    }

    /// What it says its Mac holds of each store `Inventory` moves: what the digest trigger
    /// compares.
    pub(crate) const fn holds(&self) -> Holds {
        Holds {
            membership: self.membership,
            tables: self.tables,
        }
    }
}

/// An address as a row carries it: an IPv6 address without its zone or flow.
fn travelling(address: SocketAddr) -> SocketAddr {
    match address {
        SocketAddr::V6(v6) => SocketAddr::V6(SocketAddrV6::new(*v6.ip(), v6.port(), 0, 0)),
        v4 @ SocketAddr::V4(_) => v4,
    }
}

// ---------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------

/// What [`Board::offer`] did with a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Took {
    /// It is held now (the first row of its seat, or one above the held row), but it advanced
    /// nothing: it was in the log before the tail started, or it is a backlog ([`FRESH`]).
    Held,
    /// It is above the held row, arrived live and fresh: the seat is up.
    Advanced,
    /// It is at or below the row held: nothing changed.
    Kept,
}

/// Whether a seat is up, as this Mac reads its rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// Its row advanced within [`UP_FOR`].
    Up,
    /// Its row did not: when this Mac last saw it up (Unix ms, its own wall clock), where it
    /// ever did.
    Silent {
        /// The last time it read up here.
        last_seen_ms: Option<u64>,
    },
}

/// How far a seat's newest row says it is from what this Mac holds (§4.12, "In step" and
/// "slowest"): read from the two digests alone, against the holdings of this Mac's own rows the
/// board remembers ([`OWN_KEPT`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Behind {
    /// It holds the membership log and the lab tables this Mac's newest row holds.
    InStep,
    /// Of each store it holds what this Mac held at some point the board remembers, so it lacks
    /// only records that reached this Mac since: the oldest of them reached it at least
    /// `at_least` ago, by this Mac's clock (it came no later than the first row of this Mac's
    /// that held it).
    Behind {
        /// The least age, on this Mac's clock, of the oldest record it lacks.
        at_least: Duration,
    },
    /// Of some store it holds what none of this Mac's remembered rows held: records this Mac
    /// lacks, or a lag older than the board remembers.
    Apart,
    /// Its row or this Mac's does not name a digest (no row yet, a store that did not read).
    Unknown,
}

/// What [`Behind`] reads of one store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lag {
    Same,
    Behind(Duration),
    Apart,
    Unknown,
}

/// `theirs` against this Mac's remembered holdings `own` of one store (`pick`), at `now`.
fn lag_of(
    own: &VecDeque<(Holds, Duration)>,
    pick: fn(&Holds) -> Option<Held>,
    theirs: Option<Held>,
    now: Duration,
) -> Lag {
    let (Some(theirs), Some(mine)) = (theirs, own.back().and_then(|(holds, _)| pick(holds))) else {
        return Lag::Unknown;
    };
    if theirs == mine {
        return Lag::Same;
    }
    // The latest point this Mac held what they hold, then the first row after it that held
    // more: the records they lack reached this Mac no later than that row.
    let Some(held_at) = own
        .iter()
        .rposition(|(holds, _)| pick(holds) == Some(theirs))
    else {
        return Lag::Apart;
    };
    // A remembered row whose store did not read says nothing of what it held.
    own.iter()
        .skip(held_at + 1)
        .find(|(holds, _)| pick(holds).is_some_and(|held| held != theirs))
        .map_or(Lag::Apart, |(_, since)| {
            Lag::Behind(now.saturating_sub(*since))
        })
}

/// What this Mac holds of one seat.
#[derive(Clone, Debug)]
struct Mark {
    seq: u64,
    row: PresenceRow,
    /// When it last advanced, on this Mac's links' clock.
    advanced: Option<Duration>,
}

/// The per-seat rule, a pure state machine fed rows and times: which seats are up, each one's
/// newest row, and when each was last seen up. It reads no clock; [`Presence`] hands it the
/// links' clock and the wall clock.
#[derive(Debug)]
pub struct Board {
    marks: BTreeMap<String, Mark>,
    /// When each seat last advanced, by this Mac's wall clock (Unix ms).
    seen: BTreeMap<String, u64>,
    /// What the seen file holds.
    written: BTreeMap<String, u64>,
    /// When it was last written, on the links' clock.
    wrote_at: Option<Duration>,
    /// This Mac's own holdings, oldest first, each with when a row of its own first held it (the
    /// links' clock): at most [`OWN_KEPT`].
    own: VecDeque<(Holds, Duration)>,
    #[cfg(any(test, debug_assertions))]
    stale_rows_up: bool,
}

impl Board {
    /// A board that last saw each seat of `seen` up at its time (the seen file's lines), and
    /// holds no row yet.
    #[must_use]
    pub fn new(seen: BTreeMap<String, u64>) -> Self {
        Self {
            marks: BTreeMap::new(),
            written: seen.clone(),
            seen,
            wrote_at: None,
            own: VecDeque::new(),
            #[cfg(any(test, debug_assertions))]
            stale_rows_up: stale_rows_up(),
        }
    }

    /// A row of this Mac's own, taken at `now`, holds `holds`: remembered where it differs from
    /// what the last one held.
    fn note_own(&mut self, holds: Holds, now: Duration) {
        if self.own.back().is_some_and(|(last, _)| *last == holds) {
            return;
        }
        if self.own.len() == OWN_KEPT {
            self.own.pop_front();
        }
        self.own.push_back((holds, now));
    }

    /// How far `principal`'s newest row is from what this Mac holds, at `now`: in step where both
    /// stores are, apart or unknown where either store is, else behind by the larger lag.
    #[must_use]
    pub fn behind(&self, principal: &str, now: Duration) -> Behind {
        let Some(theirs) = self.marks.get(principal).map(|mark| mark.row.holds()) else {
            return Behind::Unknown;
        };
        let lags = [
            lag_of(&self.own, |holds| holds.membership, theirs.membership, now),
            lag_of(&self.own, |holds| holds.tables, theirs.tables, now),
        ];
        if lags.contains(&Lag::Unknown) {
            return Behind::Unknown;
        }
        if lags.contains(&Lag::Apart) {
            return Behind::Apart;
        }
        lags.iter()
            .filter_map(|lag| match lag {
                Lag::Behind(at_least) => Some(*at_least),
                _ => None,
            })
            .max()
            .map_or(Behind::InStep, |at_least| Behind::Behind { at_least })
    }

    /// Whether `row` is no backlog at `wall_ms`: its origin wrote it within [`FRESH`].
    fn fresh(&self, row: &PresenceRow, wall_ms: u64) -> bool {
        #[cfg(any(test, debug_assertions))]
        if self.stale_rows_up {
            return true;
        }
        u128::from(wall_ms.abs_diff(row.at_ms)) <= FRESH.as_millis()
    }

    /// Offer `principal`'s row numbered `seq`, which arrived at `now` (the links' clock) and
    /// `wall_ms`; `live` where it landed after the tail started. It advances the seat where it is
    /// above the row held (or no row is held), live and fresh.
    pub fn offer(
        &mut self,
        principal: &str,
        seq: u64,
        row: PresenceRow,
        live: bool,
        now: Duration,
        wall_ms: u64,
    ) -> Took {
        let advances = live && self.fresh(&row, wall_ms);
        match self.marks.get_mut(principal) {
            Some(mark) if seq <= mark.seq => return Took::Kept,
            Some(mark) => {
                mark.seq = seq;
                mark.row = row;
                if advances {
                    mark.advanced = Some(now);
                }
            }
            None => {
                self.marks.insert(
                    principal.to_owned(),
                    Mark {
                        seq,
                        row,
                        advanced: advances.then_some(now),
                    },
                );
            }
        }
        if advances {
            if let Some(seen) = self.seen.get_mut(principal) {
                *seen = wall_ms;
            } else {
                self.seen.insert(principal.to_owned(), wall_ms);
            }
            Took::Advanced
        } else {
            Took::Held
        }
    }

    /// Whether `principal` is up at `now`.
    #[must_use]
    pub fn is_up(&self, principal: &str, now: Duration) -> bool {
        self.marks
            .get(principal)
            .and_then(|mark| mark.advanced)
            .is_some_and(|at| now.saturating_sub(at) < UP_FOR)
    }

    /// `principal`'s standing at `now`.
    #[must_use]
    pub fn standing(&self, principal: &str, now: Duration) -> Standing {
        if self.is_up(principal, now) {
            Standing::Up
        } else {
            Standing::Silent {
                last_seen_ms: self.last_seen(principal),
            }
        }
    }

    /// When `principal` was last seen up (Unix ms, this Mac's wall clock).
    #[must_use]
    pub fn last_seen(&self, principal: &str) -> Option<u64> {
        self.seen.get(principal).copied()
    }

    /// `principal`'s newest row and its sequence number.
    #[must_use]
    pub fn row(&self, principal: &str) -> Option<(u64, &PresenceRow)> {
        self.marks.get(principal).map(|mark| (mark.seq, &mark.row))
    }

    /// The seen file's text, where it is due at `now`: what it would hold differs from what it
    /// holds, and it was not written within [`SEEN_EVERY`]. Only the seats of `keep` stay, where
    /// it is known (a removed or re-keyed seat's line goes), and at most [`SEEN_MAX`].
    fn due(&mut self, now: Duration, keep: Option<&BTreeSet<String>>) -> Option<String> {
        if let Some(keep) = keep {
            self.seen.retain(|principal, _| keep.contains(principal));
        }
        while self.seen.len() > SEEN_MAX {
            // The seat seen longest ago goes first.
            let oldest = self
                .seen
                .iter()
                .min_by_key(|(_, ms)| **ms)
                .map(|(principal, _)| principal.clone())?;
            self.seen.remove(&oldest);
        }
        if self.seen == self.written
            || self
                .wrote_at
                .is_some_and(|at| now.saturating_sub(at) < SEEN_EVERY)
        {
            return None;
        }
        use std::fmt::Write as _;
        let mut text = String::with_capacity(self.seen.len() * 34);
        for (principal, ms) in &self.seen {
            let _ = writeln!(text, "{principal} {ms}");
        }
        Some(text)
    }

    /// The text [`Self::due`] gave was written at `now`.
    fn wrote(&mut self, now: Duration) {
        self.written.clone_from(&self.seen);
        self.wrote_at = Some(now);
    }
}

/// The seen file's lines in `dir`: none where it is missing or damaged (each damaged line goes).
#[must_use]
pub fn read_seen(dir: &Path) -> BTreeMap<String, u64> {
    let Ok(bytes) = aterm_uds::private_file::read_private(dir, SEEN_FILE, SEEN_BYTES_MAX) else {
        return BTreeMap::new();
    };
    bytes
        .split(|&b| b == b'\n')
        .filter_map(|raw| {
            let line = std::str::from_utf8(raw.strip_suffix(b"\r").unwrap_or(raw)).ok()?;
            let (principal, ms) = line.split_once(' ')?;
            Node::parse(principal)?;
            Some((principal.to_owned(), ms.parse().ok()?))
        })
        .take(SEEN_MAX)
        .collect()
}

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

/// What this Mac reports about itself, read before each row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    /// The build it runs.
    pub build: Build,
    /// Where it listens.
    pub addresses: Vec<SocketAddr>,
    /// Its membership log, where it can read one.
    pub membership: Option<Held>,
    /// Its lab tables, where it can read them.
    pub tables: Option<Held>,
    /// The topics its sessions hold (any order; the row names them in order, each once).
    pub topics: Vec<String>,
}

/// What reads [`Facts`] now.
pub type ReadFacts = Arc<dyn Fn() -> Facts + Send + Sync>;

/// This Mac's fleet addresses with the node listener's `port`: each address of an interface that
/// is up and not loopback which a peer on its LAN can dial (a private or link-local IPv4
/// address, a unique-local IPv6 one; an IPv6 link-local address dials only by a zone the peer's
/// own interface names, so it travels as nothing a peer could use), sorted, at most
/// [`ADDRESSES_MAX`].
#[must_use]
pub fn fleet_addresses(port: u16) -> Vec<SocketAddr> {
    let Ok(interfaces) = aterm_uds::network::interfaces() else {
        return Vec::new();
    };
    let mut addresses: Vec<SocketAddr> = interfaces
        .into_iter()
        .map(|interface| interface.address.to_canonical())
        .filter(|ip| dialable(*ip))
        .map(|ip| SocketAddr::new(ip, port))
        .collect();
    addresses.sort_unstable();
    addresses.dedup();
    addresses.truncate(ADDRESSES_MAX);
    addresses
}

/// Whether a peer on this Mac's LAN can dial `ip` ([`fleet_addresses`]).
fn dialable(ip: IpAddr) -> bool {
    let v6_link_local = matches!(ip, IpAddr::V6(v6) if v6.segments()[0] & 0xffc0 == 0xfe80);
    !ip.is_loopback() && !v6_link_local && aterm_net::fleet::is_fleet_source(ip)
}

/// What this Mac's vault holds of the membership log: every record its store holds
/// (`aterm_secrets::client::load_records`, what a Mac states it holds, not the acknowledged view
/// an authority decision reads); `None` where the store does not read.
#[must_use]
pub fn membership_of_vault(paths: &aterm_secrets::client::Paths) -> Option<Held> {
    let store = aterm_secrets::client::load_records(paths).ok()?;
    Some(Held::membership(&store.inventory()))
}

/// What a record store file holds of the membership log (a node service with no vault: a lab's
/// own, `aterm_agent::fleet_log::LocalLog::founded_at`): every record it holds; `None` with no
/// file, one past 16 MiB, or one that does not decode.
#[must_use]
pub fn membership_of_records(file: &Path) -> Option<Held> {
    use std::io::Read as _;
    const MAX_STORE_BYTES: u64 = 16 * 1024 * 1024;
    let (open, meta) = aterm_uds::regular_file::open(file).ok()?;
    if meta.len() > MAX_STORE_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).ok()?.saturating_add(1));
    open.take(MAX_STORE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
        return None;
    }
    let records = aterm_secrets::membership_log::decode_records(&bytes).ok()?;
    Some(Held::membership(
        &records.iter().map(|record| *record.id()).collect(),
    ))
}

/// Why the tail refused a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Its subject is not a presence subject.
    Subject,
    /// It carried no origin stamp.
    Unstamped,
    /// Its owner is not a current seat: removed, retired, unknown, or the standings were not
    /// read yet.
    NotSeated,
    /// Its producer id is not its owner's.
    Producer,
    /// Its signature does not verify under its owner's key.
    Signature,
    /// Its body is not a presence row.
    Row(RowError),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Subject => f.write_str("not a presence subject"),
            Self::Unstamped => f.write_str("no origin stamp"),
            Self::NotSeated => f.write_str("its owner is not a current seat"),
            Self::Producer => f.write_str("its producer is not its owner"),
            Self::Signature => f.write_str("its signature does not verify under its owner's key"),
            Self::Row(e) => write!(f, "{e}"),
        }
    }
}

/// What presence did, for an observer beside the journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// This Mac's row was published, numbered `seq`.
    Published {
        /// Its sequence number.
        seq: u64,
    },
    /// A tick could not publish this Mac's row.
    NotPublished {
        /// Why.
        why: String,
    },
    /// The tail started: rows below `splice` were in the log already.
    Tailing {
        /// The first offset whose rows are live.
        splice: u64,
    },
    /// The board took a row.
    Took {
        /// Its seat.
        principal: String,
        /// Its sequence number.
        seq: u64,
        /// What it did.
        took: Took,
    },
    /// The tail refused a row.
    Refused {
        /// Its subject.
        subject: String,
        /// Why.
        why: Refusal,
    },
    /// The seen file was written.
    Wrote {
        /// The seats it holds.
        seats: usize,
    },
}

/// Where presence reports each [`Event`].
pub type Observe = Arc<dyn Fn(&Event) + Send + Sync>;

/// How the node service runs presence.
#[derive(Clone)]
pub struct PresenceConfig {
    /// The folder the seen file lives in: the mesh folder, `<state_dir>/mesh/`.
    pub dir: PathBuf,
    /// Reads this Mac's facts for each row.
    pub facts: ReadFacts,
    /// Where presence reports what it did.
    pub observe: Option<Observe>,
}

/// One seat as this Mac reads it: the per-seat view's presence columns (§4.12).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seat {
    /// Its principal, `n-<id>`.
    pub principal: String,
    /// Its node key.
    pub key: [u8; 32],
    /// Whether it is this Mac.
    pub me: bool,
    /// Up, or silent since when.
    pub standing: Standing,
    /// How far its newest row is from what this Mac holds.
    pub behind: Behind,
    /// Its newest row and that row's sequence number, where this Mac holds one.
    pub row: Option<(u64, PresenceRow)>,
}

/// What presence's threads share.
pub(crate) struct Shared {
    mesh: Arc<Mesh>,
    subject: String,
    dir: PathBuf,
    facts: ReadFacts,
    observe: Option<Observe>,
    board: Mutex<Board>,
    stop: AtomicBool,
    /// The publisher parks on this bell.
    tick: Arc<Bell>,
    /// The tail parks on this one between tries.
    retry: Arc<Bell>,
    /// The tail's connection, closed to end its read.
    closer: Mutex<Option<astream_broker::Closer>>,
    /// The refusals and failures journaled once.
    said: Mutex<BTreeSet<String>>,
}

impl Shared {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire) || self.mesh.is_stopped()
    }

    fn report(&self, event: &Event) {
        if let Some(observe) = &self.observe {
            observe(event);
        }
    }

    /// Journal `line` once a run under `key`.
    fn say_once(&self, key: fmt::Arguments<'_>, line: fmt::Arguments<'_>) {
        let first = {
            let mut said = self.said.lock_or_recover();
            said.len() < SAID_MAX && said.insert(key.to_string())
        };
        if first {
            aterm_log::info!("mesh: presence: {line}");
        }
    }

    /// Publish this Mac's row once.
    fn publish_once(&self) {
        let mut facts = (self.facts)();
        facts.addresses.truncate(ADDRESSES_MAX);
        for address in &mut facts.addresses {
            *address = travelling(*address);
        }
        let row = PresenceRow {
            at_ms: relay::wall_ms(&self.mesh.wall),
            build: facts.build,
            addresses: facts.addresses,
            membership: facts.membership,
            tables: facts.tables,
            topics: facts.topics,
        };
        match self
            .mesh
            .publish(MeshLog::Metrics, &self.subject, &row.encode())
        {
            Ok(published) => self.report(&Event::Published { seq: published.seq }),
            Err(e) => {
                let why = e.to_string();
                self.say_once(
                    format_args!("publish:{why}"),
                    format_args!("no row this tick: {why}"),
                );
                self.report(&Event::NotPublished { why });
            }
        }
    }

    /// Judge one delivered row: its subject is a presence subject, its origin stamp carries its
    /// owner's producer id, its envelope verifies under its owner's node key (this Mac's own, or
    /// a current seat's whose principal the subject names), and its body is a presence row.
    /// Answers its owner, its sequence number and the row.
    fn judge<'a>(
        &self,
        subject: &'a str,
        origin: Option<(u64, u64)>,
        body: &[u8],
    ) -> Result<(&'a str, u64, PresenceRow), Refusal> {
        let Ok(Subject::Now {
            node: _,
            now: Now::Presence,
        }) = Subject::parse(subject)
        else {
            return Err(Refusal::Subject);
        };
        let principal = &subject[3..21];
        let (seq, payload) = signed_by(&self.mesh, principal, subject, origin, body)?;
        let row = PresenceRow::decode(payload).map_err(Refusal::Row)?;
        Ok((principal, seq, row))
    }

    /// Take one delivered row: judge it, offer it to the board, and write the seen file where
    /// that is due.
    fn take_row(&self, subject: &str, origin: Option<(u64, u64)>, body: &[u8], live: bool) {
        let (principal, seq, row) = match self.judge(subject, origin, body) {
            Ok(judged) => judged,
            Err(why) => {
                self.say_once(
                    format_args!("refused:{subject}:{why}"),
                    format_args!("refused a row on {subject}: {why}"),
                );
                if let Some(observe) = &self.observe {
                    observe(&Event::Refused {
                        subject: subject.to_owned(),
                        why,
                    });
                }
                return;
            }
        };
        let now = self.mesh.clock.now();
        let wall = relay::wall_ms(&self.mesh.wall);
        let theirs = row.holds();
        let (took, mine) = {
            let mut board = self.board.lock_or_recover();
            let took = board.offer(principal, seq, row, live, now, wall);
            if took != Took::Kept && principal == self.mesh.me.principal {
                board.note_own(theirs, now);
            }
            let mine = board
                .row(&self.mesh.me.principal)
                .map(|(_, row)| row.holds())
                .unwrap_or_default();
            (took, mine)
        };
        // Before the row is reported, so a reader of the report knows what it asked.
        if took == Took::Advanced && principal != self.mesh.me.principal {
            self.digest_row(principal, theirs, mine);
        }
        if let Some(observe) = &self.observe {
            observe(&Event::Took {
                principal: principal.to_owned(),
                seq,
                took,
            });
        }
        if took == Took::Advanced {
            self.write_seen(now);
        }
    }

    /// Hand `principal`'s row that advanced, what it holds `theirs` against this Mac's own latest
    /// row's `mine`, to the digest trigger of this mesh's `Inventory`, where it moves tables: with
    /// whether a tables relay to `principal` follows now.
    fn digest_row(&self, principal: &str, theirs: Holds, mine: Holds) {
        let Some(inventory) = self.mesh.inventory() else {
            return;
        };
        let follows = self.mesh.heard.lock_or_recover().contains_key(principal);
        inventory.digest_row(principal, theirs, mine, follows);
    }

    /// The current seats' principals, this Mac's among them; `None` before the standings were
    /// read.
    fn current_seats(&self) -> Option<BTreeSet<String>> {
        let mut seats: BTreeSet<String> = self.mesh.seats_by_principal()?.into_keys().collect();
        seats.insert(self.mesh.me.principal.clone());
        Some(seats)
    }

    /// Write the seen file where it is due at `now`.
    fn write_seen(&self, now: Duration) {
        if self
            .board
            .lock_or_recover()
            .wrote_at
            .is_some_and(|at| now.saturating_sub(at) < SEEN_EVERY)
        {
            return;
        }
        let keep = self.current_seats();
        let Some((text, seats)) = ({
            let mut board = self.board.lock_or_recover();
            board
                .due(now, keep.as_ref())
                .map(|text| (text, board.seen.len()))
        }) else {
            return;
        };
        match aterm_uds::private_file::write_private(&self.dir, SEEN_FILE, text.as_bytes()) {
            Ok(()) => {
                self.board.lock_or_recover().wrote(now);
                self.report(&Event::Wrote { seats });
            }
            Err(e) => self.say_once(
                format_args!("seen:{e}"),
                format_args!("{SEEN_FILE} could not be written: {e}"),
            ),
        }
    }
}

/// The sequence number and payload of `body`, delivered on `principal`'s `subject` with `origin`,
/// where its origin stamp carries `principal`'s producer id and its envelope verifies under
/// `principal`'s node key: this Mac's own, or a current seat's.
pub(crate) fn signed_by<'b>(
    mesh: &Mesh,
    principal: &str,
    subject: &str,
    origin: Option<(u64, u64)>,
    body: &'b [u8],
) -> Result<(u64, &'b [u8]), Refusal> {
    let (pid, seq) = origin.ok_or(Refusal::Unstamped)?;
    if pid != astream_broker::producer_id_of(principal) {
        return Err(Refusal::Producer);
    }
    let key = if principal == mesh.me.principal {
        mesh.me.key
    } else {
        mesh.seat_key(principal).ok_or(Refusal::NotSeated)?
    };
    if !astream_broker::origin::verify(&key, subject, seq, body) {
        return Err(Refusal::Signature);
    }
    let payload = astream_broker::origin::payload(body).ok_or(Refusal::Signature)?;
    Ok((seq, payload))
}

/// Presence running in the node service: the publisher of this Mac's row, the tail that reads
/// every Mac's, and the view's server ([`crate::view`]) that answers a reader on this Mac what
/// they hold. Dropping it stops all three.
pub struct Presence {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
    view: Option<Arc<crate::view::Server>>,
}

impl fmt::Debug for Presence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Presence")
            .field("me", &self.shared.mesh.me.principal)
            .finish_non_exhaustive()
    }
}

impl Presence {
    /// Start presence on `mesh`: read the seen file, then publish this Mac's row at once and
    /// every [`EVERY`] after, and tail every Mac's row.
    /// # Errors
    /// Where the folder cannot be made private or a thread cannot start.
    pub fn start(mesh: &Arc<Mesh>, cfg: PresenceConfig) -> io::Result<Self> {
        aterm_uds::private_file::ensure_private_dir(&cfg.dir)?;
        let node = Node::parse(&mesh.me.principal).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "the mesh's principal is no node id",
            )
        })?;
        let subject = Subject::Now {
            node,
            now: Now::Presence,
        }
        .to_string();
        let board = Board::new(read_seen(&cfg.dir));
        let shared = Arc::new(Shared {
            mesh: Arc::clone(mesh),
            subject,
            dir: cfg.dir,
            facts: cfg.facts,
            observe: cfg.observe,
            board: Mutex::new(board),
            stop: AtomicBool::new(false),
            tick: Arc::new(Bell::default()),
            retry: Arc::new(Bell::default()),
            closer: Mutex::new(None),
            said: Mutex::new(BTreeSet::new()),
        });
        *mesh.presence.lock_or_recover() = Arc::downgrade(&shared);
        let mut presence = Self {
            shared: Arc::clone(&shared),
            threads: Vec::new(),
            view: match crate::view::Server::bind(&shared.dir) {
                Ok(server) => server.map(Arc::new),
                Err(e) => {
                    aterm_log::warn!("mesh: presence: the view is not served: {e}");
                    None
                }
            },
        };
        let reader = Arc::clone(&shared);
        presence.threads.push(
            std::thread::Builder::new()
                .name("aterm-mesh-presence-tail".to_owned())
                .spawn(move || tail(&reader))?,
        );
        let publisher = Arc::clone(&shared);
        presence.threads.push(
            std::thread::Builder::new()
                .name("aterm-mesh-presence".to_owned())
                .spawn(move || publish(&publisher))?,
        );
        if let Some(server) = &presence.view {
            let (server, answering) = (Arc::clone(server), Arc::clone(&shared));
            presence.threads.push(
                std::thread::Builder::new()
                    .name("aterm-mesh-view".to_owned())
                    .spawn(move || server.run(&|| answering.stopped(), &|| answering.view()))?,
            );
        }
        aterm_log::info!(
            "mesh: presence: {} writes its row every {} s",
            mesh.me.principal,
            EVERY.as_secs()
        );
        Ok(presence)
    }

    /// Every current seat as this Mac reads it now, this Mac's included, by principal.
    #[must_use]
    pub fn seats(&self) -> Vec<Seat> {
        self.shared.seats()
    }

    /// The seat `principal` as this Mac reads it now, where it is a current seat.
    #[must_use]
    pub fn seat(&self, principal: &str) -> Option<Seat> {
        let mesh = &self.shared.mesh;
        let key = if principal == mesh.me.principal {
            mesh.me.key
        } else {
            mesh.seat_key(principal)?
        };
        let now = mesh.clock.now();
        let board = self.shared.board.lock_or_recover();
        Some(Seat {
            me: key == mesh.me.key,
            standing: board.standing(principal, now),
            behind: board.behind(principal, now),
            row: board.row(principal).map(|(seq, row)| (seq, row.clone())),
            principal: principal.to_owned(),
            key,
        })
    }

    /// Stop both threads and wait for them.
    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.tick.ring();
        self.shared.retry.ring();
        if let Some(closer) = self.shared.closer.lock_or_recover().as_ref() {
            closer.close();
        }
        if let Some(server) = &self.view {
            server.nudge();
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        self.view = None;
    }
}

impl Shared {
    /// [`Presence::seats`].
    pub(crate) fn seats(&self) -> Vec<Seat> {
        let mesh = &self.mesh;
        let mut keys = mesh.seats_by_principal().unwrap_or_default();
        keys.insert(mesh.me.principal.clone(), mesh.me.key);
        let now = mesh.clock.now();
        let board = self.board.lock_or_recover();
        keys.into_iter()
            .map(|(principal, key)| Seat {
                me: key == mesh.me.key,
                standing: board.standing(&principal, now),
                behind: board.behind(&principal, now),
                row: board.row(&principal).map(|(seq, row)| (seq, row.clone())),
                principal,
                key,
            })
            .collect()
    }

    /// The view a reader on this Mac is answered ([`crate::view`]): every seat as the board reads
    /// it now, and every seat's newest needs row and activity row, each read on its own under one
    /// deadline both share, so one that fails leaves the other and the two together stay within
    /// the bound a reader waits ([`crate::view::WITHIN`]).
    pub(crate) fn view(&self) -> crate::view::View {
        let deadline = crate::view::rows_deadline();
        let needs = match crate::view::needs_rows(&self.mesh, deadline) {
            Ok(needs) => Some(needs),
            Err(e) => {
                self.say_once(
                    format_args!("needs:{e}"),
                    format_args!("the view holds no needs rows: {e}"),
                );
                None
            }
        };
        let activity = match crate::view::activity_rows(&self.mesh, deadline) {
            Ok(activity) => Some(activity),
            Err(e) => {
                self.say_once(
                    format_args!("activity:{e}"),
                    format_args!("the view holds no activity rows: {e}"),
                );
                None
            }
        };
        crate::view::View {
            me: self.mesh.me.principal.clone(),
            at_ms: relay::wall_ms(&self.mesh.wall),
            standings: self.mesh.seats_by_principal().is_some(),
            seats: self.seats(),
            needs,
            activity,
        }
    }

    /// [`Mesh::holders`].
    pub(crate) fn holders(&self) -> BTreeSet<String> {
        let candidates: Vec<String> = {
            let board = self.board.lock_or_recover();
            board
                .marks
                .iter()
                .filter(|(principal, mark)| {
                    *principal != &self.mesh.me.principal && !mark.row.topics.is_empty()
                })
                .map(|(principal, _)| principal.clone())
                .collect()
        };
        if candidates.is_empty() {
            return BTreeSet::new();
        }
        candidates
            .into_iter()
            .filter(|principal| self.mesh.seat_key(principal).is_some())
            .collect()
    }
}

impl Drop for Presence {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The publisher: this Mac's row at once, then every [`EVERY`] of the links' clock.
fn publish(shared: &Shared) {
    let clock = &shared.mesh.clock;
    let mut next = clock.now();
    while !shared.stopped() {
        let now = clock.now();
        if now >= next {
            shared.publish_once();
            next = now.saturating_add(EVERY);
        }
        clock.park(next, &shared.tick);
    }
}

/// Where the tail stands across its runs: the offset to resume from, and the first offset whose
/// rows are live (the first run's splice point). Rows below the splice were in the log before
/// presence started, so they are held without advancing a seat, whichever run reads them.
#[derive(Clone, Copy, Debug, Default)]
struct Cursor {
    resume: Option<u64>,
    splice: Option<u64>,
}

/// The tail: one reader of every presence row on this Mac's `metrics.log`, opened again after a
/// failure on a doubling back-off of the links' clock, resuming where it stopped.
fn tail(shared: &Shared) {
    let clock = &shared.mesh.clock;
    let mut cursor = Cursor::default();
    let mut backoff = READ_RETRY_FIRST;
    while !shared.stopped() {
        let started = clock.now();
        match tail_once(shared, &mut cursor) {
            Ok(()) => backoff = READ_RETRY_FIRST,
            Err(e) => {
                if shared.stopped() {
                    break;
                }
                shared.say_once(
                    format_args!("tail:{e}"),
                    format_args!("the tail ended ({e}); it opens again"),
                );
            }
        }
        let until = started.saturating_add(backoff);
        while !shared.stopped() && clock.now() < until {
            clock.park(until, &shared.retry);
        }
        backoff = backoff.saturating_mul(2).min(READ_RETRY_MAX);
    }
}

/// One run of the tail. The first reads the face of every presence row (the newest of each
/// seat), takes its splice point, and follows from the oldest of them, so each seat's newest
/// row is held before anything live arrives; a later run resumes where the last one stopped.
fn tail_once(shared: &Shared, cursor: &mut Cursor) -> io::Result<()> {
    let hosted = shared.mesh.hosted(MeshLog::Metrics);
    let grant = hosted.mint(&format!("ro:{FILTER}"))?;
    let link = hosted.adopt(shared.mesh.me.key)?;
    // An idle tail is marked every 10 s, so the bound fires only on a broker that went quiet.
    link.set_read_timeout(Some(relay::IO_WITHIN))?;
    link.set_write_timeout(Some(relay::IO_WITHIN))?;
    let (mut client, closer) = astream_broker::connect_stream(link);
    {
        let mut held = shared.closer.lock_or_recover();
        if shared.stopped() {
            closer.close();
            return Ok(());
        }
        *held = Some(closer);
    }
    client.attach_all([(grant.filter.as_str(), &grant.tag[..])])?;
    let (from, splice) = match (cursor.resume, cursor.splice) {
        (Some(resume), Some(splice)) => (resume, splice),
        _ => {
            let (face, (next, _)) = client.last_all(FILTER)?;
            let from = face
                .iter()
                .map(|(offset, _, _)| *offset)
                .min()
                .unwrap_or(next);
            cursor.splice = Some(next);
            (from, next)
        }
    };
    client.origin_stamps()?;
    let mut sub = client.subscribe(from, FILTER)?;
    shared.report(&Event::Tailing { splice });
    loop {
        if shared.stopped() {
            return Ok(());
        }
        match sub.recv_event()? {
            Some(astream_broker::Event::Delivery {
                offset,
                subject,
                body,
            }) => {
                cursor.resume = Some(offset.saturating_add(1));
                shared.take_row(&subject, sub.last_origin(), &body, offset >= splice);
            }
            Some(astream_broker::Event::Mark { next, .. }) => {
                if cursor.resume.is_none_or(|at| next > at) {
                    cursor.resume = Some(next);
                }
            }
            Some(astream_broker::Event::Gap(_)) => {}
            None => return Ok(()),
        }
    }
}

#[cfg(test)]
#[path = "presence_tests.rs"]
mod tests;
