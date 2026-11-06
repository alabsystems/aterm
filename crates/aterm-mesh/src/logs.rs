// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The two mesh logs the node service hosts, and the membership log's verdict on them
//! (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.6, "Three logs per Mac" and "Removal follows the
//! log").
//!
//! Each log lives in a folder of its own under `<state_dir>/mesh/`, because aterm-link's rules
//! keep one `rest.key` per folder (`aterm_link::at_rest::key_path`), and a retire of one log would
//! otherwise strand the other's seal:
//!
//! ```text
//! <state_dir>/mesh/tables/   tables.log  rest.key  guard.secret  sock  lease/  relayed
//! <state_dir>/mesh/metrics/  metrics.log rest.key  guard.secret  sock  lease/  relayed
//! ```
//!
//! - `tables.log` is Ordered (100 ms) with `/t/>` escalated, so each table record is flushed
//!   before its ack; `metrics.log` is Relaxed and retained by `aterm_metrics::rules`, whose
//!   `last` rules make a `now` or `quota` subject keep its highest sequence number.
//! - Both are sealed at rest (`aterm_link::at_rest::ensure_key`, then `prepare`).
//! - Each broker is guarded by a secret of its own (`guard.secret`): every connection attaches a
//!   grant minted under it, and a neighbour gets its grants, each bound to its own key, at the head
//!   of its `Bus` stream ([`crate::relay`]). astream's rule "a relaying broker binds its grants"
//!   holds: no other principal's grant can take an origin's dedup key.
//! - Each is served on a local socket (`sock`) in its 0700 folder, for the node service's own
//!   readers.
//!
//! The origin keys and the revoked set follow the membership log ([`Standings`]): at start and
//! after every change, each key that entered the revoked set (`Removed` or `Retired`) is revoked on
//! both brokers, each that left it is un-revoked, the origin keys become the current seats' (this
//! Mac's own included), and only a `Removed` key's subjects are retired on `metrics.log`. A
//! superseded seat (a Mac rebuilt with its key kept, `Decision::Replace`) reads `Member` of its new
//! seat, so its key is never revoked; a voided removal reads `Member` again, so the next pass
//! un-revokes it on every broker with no restart. The revoked set is held in memory only: astream
//! keeps none across a reopen, and the pass at start recomputes it.
//!
//! **`bus.log`** (§11 C2, §4.11). With the mesh on, the node service hosts the fabric's log too
//! ([`BusKeeper`]): it retires the separate broker job (`aterm_link::enable::retire_broker_job`)
//! and opens `bus.log` itself where the job did, in the fabric root, under the root's
//! `mint.secret` (every `node.cap` and person cap was minted under it), sealed by aterm-link's
//! rules, retained and rate limited by `aterm_link::broker_rules`, and served on the same
//! `bus.sock`, so local clients and the rendezvous file stay as they were ([`open_bus`],
//! [`BusHost`]). A poisoned log is reopened in process. The revocation pass covers it beside the
//! two mesh logs ([`Mesh::join_bus`]), and a neighbour's `Bus` stream for it is served with the
//! ring of §4.11 ([`BusHost::ring_for`]): mail to this Mac only and only as itself, its own
//! topics, the halt rows, and the relay of origin-signed halts.

use crate::relay::{self, Event, Observe, Origin, Published, Relayed};
use astream_broker::{
    AnyClient, Boot, Broker, BrokerHandle, Capability, Escalate, Escalation, StreamLink,
};
use aterm_link::at_rest;
/// The tier `bus.log` acks under, and aterm-link's verdicts the keeper reads: re-exported, so the
/// node service names them without depending on aterm-link.
pub use aterm_link::durability::Durability;
pub use aterm_link::enable::{BusHosting, RetiredJob};
use aterm_net::fleet::link::{Bell, BusLog, Incoming, LinkClock};
use aterm_secrets::membership_log::{Log, NodeStanding};
use aterm_types::MutexExt;
use aterm_uds::backup::Backups;
use aterm_uds::secret::Secret;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, SystemTime};

/// The folder under the state dir that holds both logs.
pub const MESH_DIR: &str = "mesh";

/// Each log's guard secret, beside it: 32 random bytes, 0600, never replaced.
pub const GUARD_SECRET: &str = "guard.secret";

/// Each log's local socket, beside it.
pub const SOCKET: &str = "sock";

/// The bytes a socket path may take (`sun_path`, `aterm_link::enable::SUN_PATH_MAX`): a longer
/// one is not served, and the log is reached through the node service alone.
pub(crate) const SUN_PATH_MAX: usize = 104;

/// A guard secret's length.
const GUARD_LEN: usize = 32;

/// The wall clock the mesh reads: the incarnation floor of a rebuilt Mac and the 7-day cursor
/// rule. Injected, so a test moves it.
pub type Wall = Arc<dyn Fn() -> SystemTime + Send + Sync>;

/// What reads the membership log's standings now: `None` while it cannot be read, which keeps
/// the standings last applied (a moment's fault never un-revokes a key).
pub type ReadStandings = Arc<dyn Fn() -> Option<Standings> + Send + Sync>;

/// Whether the membership log may have changed since the last call: a cheap probe the watcher
/// polls (the node service's `LocalLog::keys`, compared by pointer), before the dear read.
pub type Changed = Arc<dyn Fn() -> bool + Send + Sync>;

/// One of the two logs the mesh hosts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MeshLog {
    /// `tables.log`: the membership log's records and every lab table's operations.
    Tables,
    /// `metrics.log`: each Mac's own rows.
    Metrics,
}

impl MeshLog {
    /// Both, in wire order.
    pub const ALL: [Self; 2] = [Self::Tables, Self::Metrics];

    /// Its word: the folder it lives in and the word its `Bus` stream names.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Tables => "tables",
            Self::Metrics => "metrics",
        }
    }

    /// The log a `Bus` stream names.
    #[must_use]
    pub const fn bus(self) -> BusLog {
        match self {
            Self::Tables => BusLog::Tables,
            Self::Metrics => BusLog::Metrics,
        }
    }

    /// The mesh log a `Bus` stream's log is: `None` for `bus.log`, which the node service hosts
    /// from R3.
    #[must_use]
    pub const fn of_bus(log: BusLog) -> Option<Self> {
        match log {
            BusLog::Tables => Some(Self::Tables),
            BusLog::Metrics => Some(Self::Metrics),
            BusLog::Bus => None,
        }
    }

    /// The log's file name.
    #[must_use]
    pub const fn file(self) -> &'static str {
        match self {
            Self::Tables => "tables.log",
            Self::Metrics => "metrics.log",
        }
    }

    /// Every subject of the log that travels: what its relays carry.
    #[must_use]
    pub const fn filter(self) -> &'static str {
        match self {
            Self::Tables => "/t/>",
            Self::Metrics => aterm_metrics::subject::SHARED_FILTER,
        }
    }

    /// The subjects of the log that `principal` originates and that travel: a neighbour reads
    /// them to find the high water it holds of that origin.
    #[must_use]
    pub fn own_filter(self, principal: &str) -> String {
        match self {
            Self::Tables => format!("/t/*/{principal}"),
            Self::Metrics => format!("/m/{principal}/shared/>"),
        }
    }

    /// Every subject of the log that `principal` may publish, local ones included.
    #[must_use]
    pub fn publish_filter(self, principal: &str) -> String {
        match self {
            Self::Tables => format!("/t/*/{principal}"),
            Self::Metrics => format!("/m/{principal}/>"),
        }
    }

    /// The subjects a removed Mac's rows are on, which the revocation pass retires.
    fn retired_filter(principal: &str) -> String {
        format!("/m/{principal}/>")
    }

    /// The tier the log is opened at.
    const fn durability(self) -> Durability {
        match self {
            Self::Tables => Durability::Ordered,
            Self::Metrics => Durability::Relaxed,
        }
    }

    /// What the log flushes before it acks: every table record on `tables.log`, nothing on the
    /// Relaxed `metrics.log`. Each replaces the bus's rules, which `Durability::options` carries.
    fn escalation(self) -> Escalation {
        match self {
            Self::Tables => Escalation::new([Escalate::Subject(self.filter().to_owned())]),
            Self::Metrics => Escalation::default(),
        }
    }
}

/// The membership log's verdict on node keys, as the mesh reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Standings {
    /// The current seats' node keys: the origins.
    pub current: BTreeSet<[u8; 32]>,
    /// Keys of Macs the log took out (`NodeStanding::Removed`): revoked, and their rows retired.
    pub removed: BTreeSet<[u8; 32]>,
    /// Keys a member bound and since replaced (`NodeStanding::Retired`): revoked only.
    pub retired: BTreeSet<[u8; 32]>,
    /// Each retired key whose seat is current, and the seat's node key now: the key rotations a
    /// sender carries held mail across (§11 C3).
    pub rotated: BTreeMap<[u8; 32], [u8; 32]>,
}

impl Standings {
    /// The standings `log` evaluates: a `Member` key whose seat is current (its node key and
    /// writer key not removed, its recipient not removed) is an origin; a `Removed` or `Retired`
    /// key is revoked; a contested key, and a member's key whose seat is not current, is neither.
    #[must_use]
    pub fn of_log(log: &Log) -> Self {
        let membership = log.membership();
        let mut standings = Self::default();
        let mut seated = BTreeMap::new();
        let mut retired_of = Vec::new();
        for (key, standing) in membership.nodes() {
            match standing {
                NodeStanding::Member(id) => {
                    let current = membership.get(id).is_some_and(|seat| {
                        !membership.member_node_removed(seat)
                            && !log.recipient_is_removed(&seat.binding.recipient())
                    });
                    if current {
                        standings.current.insert(*key);
                        seated.insert(id, *key);
                    }
                }
                NodeStanding::Removed(_) => {
                    standings.removed.insert(*key);
                }
                NodeStanding::Retired(id) => {
                    standings.retired.insert(*key);
                    retired_of.push((*key, id));
                }
                NodeStanding::Contested | NodeStanding::Unknown => {}
            }
        }
        for (old, id) in retired_of {
            if let Some(new) = seated.get(&id) {
                standings.rotated.insert(old, *new);
            }
        }
        standings
    }

    /// The standings of a read admission makes (`load_acknowledged_and_ahead`): what the
    /// acknowledged log grants, with every key the records ahead of it hold out held out too.
    /// Those records verify and this Mac made them durable, so a removal they hold stands at once;
    /// they grant nothing until a writer acknowledges them.
    #[must_use]
    pub fn of_read(acknowledged: &Log, ahead: Option<&Log>) -> Self {
        let mut standings = Self::of_log(acknowledged);
        if let Some(ahead) = ahead {
            let outs = Self::of_log(ahead);
            for key in &outs.removed {
                standings.current.remove(key);
                standings.retired.remove(key);
                standings.removed.insert(*key);
            }
            standings
                .rotated
                .retain(|_, new| !outs.removed.contains(new) && !outs.retired.contains(new));
            for key in &outs.retired {
                if !standings.removed.contains(key) {
                    standings.current.remove(key);
                    standings.retired.insert(*key);
                }
            }
        }
        standings
    }

    /// The standings of the vault `paths` keeps, as authority reads it; `None` with no log or
    /// while the store cannot be read.
    #[must_use]
    pub fn read_vault(paths: &aterm_secrets::client::Paths) -> Option<Self> {
        let read = aterm_secrets::client::load_acknowledged_and_ahead(paths).ok()?;
        let log = read.acknowledged.log()?;
        Some(Self::of_read(
            log,
            read.ahead.as_ref().and_then(|store| store.log()),
        ))
    }

    /// The standings of a record store file (a node service with no vault: a test's lab of its
    /// own, `aterm_agent::fleet_log::LocalLog::founded_at`): every record it holds, under the
    /// genesis among them; `None` with no file, no genesis or a record the log refuses.
    #[must_use]
    pub fn read_records(file: &Path) -> Option<Self> {
        use aterm_secrets::membership_log::{Kind, decode_records};
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
        let mut records = decode_records(&bytes).ok()?;
        let pos = records.iter().position(|r| r.kind() == Kind::Genesis)?;
        let genesis = records.remove(pos);
        let mut log = Log::new(genesis).ok()?;
        for record in records {
            if record.kind() != Kind::Genesis {
                log.insert(record).ok()?;
            }
        }
        Some(Self::of_log(&log))
    }

    /// Whether `key` is an origin now.
    #[must_use]
    pub fn is_current(&self, key: &[u8; 32]) -> bool {
        self.current.contains(key) && !self.removed.contains(key) && !self.retired.contains(key)
    }

    /// The revoked set: every `Removed` or `Retired` key.
    #[must_use]
    pub fn revoked(&self) -> BTreeSet<[u8; 32]> {
        self.removed.union(&self.retired).copied().collect()
    }
}

fn principal_buf(key: &[u8; 32]) -> [u8; 18] {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = [0u8; 18];
    out[0] = b'n';
    out[1] = b'-';
    for (i, &b) in key[..8].iter().enumerate() {
        out[2 + i * 2] = HEX[usize::from(b >> 4)];
        out[3 + i * 2] = HEX[usize::from(b & 0x0f)];
    }
    out
}

fn principal_str(buf: &[u8; 18]) -> &str {
    std::str::from_utf8(buf).unwrap_or("")
}

/// The class a broker reads a key as: its producer id, `producer_id_of(identity_principal(key))`.
/// A broker keys its origins and revocations by the principal (the key's first 8 bytes) and
/// deduplicates relayed records by the producer id that principal hashes to, so two keys of one
/// class are one origin to it, each able to shadow or pre-empt the other.
fn class_of(key: &[u8; 32]) -> u64 {
    astream_broker::producer_id_of(principal_str(&principal_buf(key)))
}

/// What the brokers take of a read, with keys judged by class ([`class_of`]), current winning:
/// the origins, and what is revoked, by key and by principal.
///
/// - A current key that shares its class with no other current key (this Mac's own counted) is
///   an origin, admitted and a seat. Two or more current keys of one class are contested: none
///   is an origin, each is revoked by its key, and their principals are revoked, until one key
///   is left. On this Mac its own key always wins its class.
/// - A `Removed` or `Retired` key is revoked by its key, and by its principal unless an origin
///   holds that principal: a removed key that shares an origin's principal (a member can bind and
///   retire a key at will) never takes that origin out.
/// - This Mac's own key and principal are never revoked on its own brokers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Verdict {
    /// The origin keys every broker is given: this Mac's and each uncontested current seat's.
    pub(crate) origins: BTreeSet<[u8; 32]>,
    /// The keys revoked, each by its key alone.
    pub(crate) keys: BTreeSet<[u8; 32]>,
    /// The principals revoked.
    pub(crate) principals: BTreeSet<String>,
}

impl Verdict {
    /// The verdict on `read` for the Mac whose key is `me`, keys classed by `class`: every key the
    /// read names, this Mac's first, interned to dense classes and principals and judged by
    /// `aterm_mesh_core::relay::Verdict::of`, whose kernels and folds `relay_island.rs` proves.
    pub(crate) fn of(read: &Standings, me: &[u8; 32], class: impl Fn(&[u8; 32]) -> u64) -> Self {
        let others: BTreeSet<[u8; 32]> = read
            .current
            .iter()
            .chain(&read.removed)
            .chain(&read.retired)
            .filter(|key| *key != me)
            .copied()
            .collect();
        let all: Vec<[u8; 32]> = std::iter::once(*me).chain(others).collect();
        let mut classes: BTreeMap<u64, usize> = BTreeMap::new();
        let mut names: Vec<String> = Vec::new();
        let mut principal_of: BTreeMap<String, usize> = BTreeMap::new();
        let keys: Vec<aterm_mesh_core::relay::Key> = all
            .iter()
            .map(|key| {
                let next = classes.len();
                let class = *classes.entry(class(key)).or_insert(next);
                let buf = principal_buf(key);
                let name = principal_str(&buf);
                let principal = principal_of.get(name).copied().unwrap_or_else(|| {
                    let idx = names.len();
                    let owned = name.to_owned();
                    principal_of.insert(owned.clone(), idx);
                    names.push(owned);
                    idx
                });
                let gone = read.removed.contains(key) || read.retired.contains(key);
                aterm_mesh_core::relay::Key {
                    class,
                    principal,
                    current: !gone && read.current.contains(key),
                    gone,
                }
            })
            .collect();
        let verdict = aterm_mesh_core::relay::Verdict::of(&keys, 0, names.len());
        let pick = |judged: &dyn Fn(usize) -> bool| -> BTreeSet<[u8; 32]> {
            (0..all.len())
                .filter(|&k| judged(k))
                .map(|k| all[k])
                .collect()
        };
        Self {
            origins: pick(&|k| verdict.is_origin(k)),
            keys: pick(&|k| verdict.is_revoked(k)),
            principals: names
                .into_iter()
                .enumerate()
                .filter_map(|(p, name)| verdict.is_principal_revoked(p).then_some(name))
                .collect(),
        }
    }
}

/// How the node service opens the mesh.
#[derive(Clone)]
pub struct MeshConfig {
    /// The node service's state dir: `fleet-node-key` is read there and the logs live in its
    /// `mesh/` folder.
    pub state_dir: PathBuf,
    /// How each `rest.key` is marked for backups: `Backups::SYSTEM` in the node service,
    /// `Backups::INERT` in a test.
    pub backups: Backups,
    /// The clock the overlay's and the relays' timers read: the links' clock.
    pub clock: Arc<dyn LinkClock>,
    /// The wall clock.
    pub wall: Wall,
    /// Reads the membership log's standings.
    pub standings: ReadStandings,
    /// The watcher's probe; with `None` the standings are read only at start and on
    /// [`Mesh::follow`].
    pub changed: Option<Changed>,
    /// How often the watcher calls the probe.
    pub watch_every: Duration,
    /// Where the relays report what they did, beside the node service's journal.
    pub observe: Option<Observe>,
    /// The stores whose records the mesh moves (§11 B7b): with them, the egress, the ingest
    /// consumer and the `Inventory` exchanges run ([`crate::flow`]); with `None`, the logs relay
    /// only what is published on them.
    pub stores: Option<Arc<dyn crate::stores::Stores>>,
}

/// This Mac as the mesh knows it: the node key's seed (held scrubbed on drop), its public key,
/// its principal, the producer id its records carry, and aterm's state folder the key is in,
/// where the person key it vouches for lives too (§11 C3c).
pub(crate) struct Me {
    pub(crate) seed: Secret,
    pub(crate) key: [u8; 32],
    pub(crate) principal: String,
    pub(crate) pid: u64,
    pub(crate) dir: PathBuf,
}

impl Me {
    /// The seed in `fleet-node-key` (64 lowercase hex digits and a newline, as aterm-net writes
    /// it), checked against the node identity aterm-net reads from the same folder: one key signs
    /// the link handshakes, the records, and names the principal.
    fn read(state_dir: &Path) -> io::Result<Self> {
        let file = aterm_net::fleet_member::NODE_KEY_FILE;
        let mut bytes = aterm_uds::private_file::read_private(state_dir, file, 65)
            .map_err(|e| io::Error::new(io::ErrorKind::NotFound, format!("{file}: {e}")))?;
        let malformed = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{file} holds no node key"),
            )
        };
        let mut seed = [0u8; 32];
        let decoded = std::str::from_utf8(&bytes).ok().is_some_and(|text| {
            let digits = text.strip_suffix('\n').unwrap_or(text);
            aterm_codec::hex::is_lower(digits, 64)
                && aterm_codec::hex::decode_into(digits.as_bytes(), &mut seed)
        });
        aterm_uds::secret::scrub(&mut bytes);
        if !decoded {
            aterm_uds::secret::scrub(&mut seed);
            return Err(malformed());
        }
        let key = astream_broker::origin::public_key(&seed);
        let held = Secret::new(&seed);
        aterm_uds::secret::scrub(&mut seed);
        let proved = aterm_net::fleet_member::read_node_identity(state_dir)?
            .map(|identity| *identity.key().bytes());
        if proved != Some(key) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{file} gives a key aterm-net does not prove"),
            ));
        }
        let principal = astream_broker::identity_principal(&key);
        let pid = astream_broker::producer_id_of(&principal);
        Ok(Self {
            seed: held,
            key,
            principal,
            pid,
            dir: state_dir.to_path_buf(),
        })
    }

    /// The seed, for the one signature that needs it.
    pub(crate) fn seed(&self) -> &[u8; 32] {
        self.seed
            .expose()
            .try_into()
            .unwrap_or_else(|_| unreachable!("a seed is held as 32 bytes"))
    }
}

/// One log the mesh hosts: its broker, the socket it serves, its guard, the lease its sequence
/// numbers come from, its local publishing connection and when it last relayed to each
/// neighbour.
pub(crate) struct Hosted {
    pub(crate) log: MeshLog,
    pub(crate) broker: Arc<Broker>,
    served: Mutex<Option<BrokerHandle>>,
    guard: Secret,
    pub(crate) origin: Mutex<Origin>,
    local: Mutex<Option<AnyClient>>,
    pub(crate) relayed: Mutex<Relayed>,
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// A test's stand-in for a reboot: the brokers this thread opens name this boot, not the
    /// machine's. astream moves a weak tier's generation, with lineage, when it opens a log its
    /// last run left open in another boot.
    pub static BOOT: std::cell::Cell<Option<[u8; 16]>> = const { std::cell::Cell::new(None) };
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// A test's stand-in for a retirement that fails (the log refused, a write failed): each
    /// retirement this thread runs fails.
    pub static RETIRE_REFUSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The boot the brokers name: the machine's, or on a test's thread [`BOOT`].
fn boot() -> Boot {
    #[cfg(any(test, debug_assertions))]
    if let Some(boot) = BOOT.with(std::cell::Cell::get) {
        return Boot::Known(boot);
    }
    aterm_link::durability::this_boot().map_or(Boot::Unknown, Boot::Known)
}

fn refused(refusal: &at_rest::Refused) -> io::Error {
    io::Error::other(format!("{}: {}", refusal.why, refusal.remedy))
}

/// The log's guard secret: read, or minted once where none is. A guard is never replaced: the
/// grants a neighbour holds were minted under it.
fn guard_secret(dir: &Path) -> io::Result<Secret> {
    use std::os::unix::fs::MetadataExt as _;
    if let Ok(bytes) = aterm_uds::private_file::read_private(dir, GUARD_SECRET, GUARD_LEN + 1) {
        let held = Secret::from_vec(bytes);
        if held.len() != GUARD_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{GUARD_SECRET} is not {GUARD_LEN} bytes"),
            ));
        }
        return Ok(held);
    }
    let path = dir.join(GUARD_SECRET);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) => {
            if meta.nlink() > 1 {
                // A crash inside the write leaves the temporary as the name's second link.
                aterm_uds::atomic_file::sweep_temps_beside(&path, Duration::ZERO)?;
            }
        }
        Err(_) => {
            let mut fresh = [0u8; GUARD_LEN];
            if let Err(e) = aterm_uds::rand::fill(&mut fresh) {
                aterm_uds::secret::scrub(&mut fresh);
                return Err(e);
            }
            let wrote =
                aterm_uds::private_file::write_private_new(dir, GUARD_SECRET, &fresh, |_| Ok(()));
            if wrote.is_ok() {
                let held = Secret::new(&fresh);
                aterm_uds::secret::scrub(&mut fresh);
                return Ok(held);
            }
            aterm_uds::secret::scrub(&mut fresh);
            wrote?;
            aterm_uds::atomic_file::sweep_temps_beside(&path, Duration::ZERO)?;
        }
    }
    let bytes = aterm_uds::private_file::read_private(dir, GUARD_SECRET, GUARD_LEN + 1)
        .map_err(|e| io::Error::other(format!("{GUARD_SECRET}: {e}")))?;
    let held = Secret::from_vec(bytes);
    if held.len() != GUARD_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{GUARD_SECRET} is not {GUARD_LEN} bytes"),
        ));
    }
    Ok(held)
}

impl Hosted {
    /// Open `log` in its folder under `mesh`: the key, the seal, the guard, the broker, its rules,
    /// this Mac's own origin key, the socket, and the lease.
    fn open(mesh: &Path, log: MeshLog, cfg: &MeshConfig, me: &Me) -> io::Result<Self> {
        let dir = mesh.join(log.word());
        aterm_uds::private_file::ensure_private_dir(&dir)?;
        let file = dir.join(log.file());
        at_rest::ensure_key(&file, cfg.backups, false).map_err(|r| refused(&r))?;
        let prepared = at_rest::prepare(&file, cfg.backups).map_err(|r| refused(&r))?;
        for note in &prepared.notes {
            aterm_log::info!("mesh: {}: {note}", log.file());
        }
        let guard = guard_secret(&dir)?;
        let mut opts = log.durability().options();
        opts.escalation = log.escalation();
        opts.boot = boot();
        opts.guard = Some(guard.expose().to_vec());
        opts.at_rest_key = prepared.key.as_ref().map(at_rest::Key::bytes);
        opts.require_bound_at_rest = opts.at_rest_key.is_some();
        drop(prepared);
        let broker = Broker::open_opts(&file, opts, || Ok(astream_broker::Prepared::Nothing))?;
        if log == MeshLog::Metrics {
            broker.set_retention(aterm_metrics::rules::rules())?;
            aterm_log::info!(
                "mesh: metrics.log retains {}",
                aterm_metrics::rules::summary()
            );
        }
        broker.set_origin_keys([me.key]);
        let sock = dir.join(SOCKET);
        let served = if sock.as_os_str().len() < SUN_PATH_MAX {
            Some(broker.serve(&sock)?)
        } else {
            aterm_log::warn!(
                "mesh: {} is not served on a socket: {} is too long a path",
                log.file(),
                sock.display()
            );
            None
        };
        let origin = Origin::open(&dir.join("lease"), &cfg.wall, log)?;
        let relayed = Relayed::read(&dir);
        Ok(Self {
            log,
            broker: Arc::new(broker),
            served: Mutex::new(served),
            guard,
            origin: Mutex::new(origin),
            local: Mutex::new(None),
            relayed: Mutex::new(relayed),
        })
    }

    /// A grant minted under this log's guard.
    pub(crate) fn mint(&self, grant: &str) -> io::Result<Capability> {
        astream_broker::mint(self.guard.expose(), grant)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
    }

    /// A stream adopted by this log's broker under `key`: the far end, for a client.
    pub(crate) fn adopt(&self, key: [u8; 32]) -> io::Result<Arc<dyn StreamLink>> {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
        drop(self.broker.serve_stream(Arc::new(theirs), key)?);
        Ok(Arc::new(ours))
    }

    /// A client of this log's broker under this Mac's own key, holding `grant`, each read and
    /// write bounded by `within` (`None`: unbounded).
    pub(crate) fn client(
        &self,
        me: &Me,
        grant: &str,
        within: Option<Duration>,
    ) -> io::Result<AnyClient> {
        let cap = self.mint(grant)?;
        let link = self.adopt(me.key)?;
        link.set_read_timeout(within)?;
        link.set_write_timeout(within)?;
        let (mut client, _closer) = astream_broker::connect_stream(link);
        client.attach_all([(cap.filter.as_str(), &cap.tag[..])])?;
        Ok(client)
    }

    /// The grants a neighbour (`peer`, its principal) is handed at the head of its `Bus` stream:
    /// relaying every subject of the log that travels, and reading its own subjects, so a
    /// rebuilt Mac finds the high water this Mac holds of it. Both are bound to the peer, so only
    /// its own stream can use them.
    pub(crate) fn grants_for(&self, peer: &str) -> io::Result<Vec<Capability>> {
        let own = match self.log {
            MeshLog::Tables => format!("ro,p={peer}:/t/*/{peer}"),
            MeshLog::Metrics => format!("ro,p={peer}:/m/{peer}/shared/>"),
        };
        Ok(vec![
            self.mint(&format!("rw,relay,p={peer}:{}", self.log.filter()))?,
            self.mint(&own)?,
        ])
    }

    /// The grants this log's relay to a neighbour holds on its own broker: reading what travels,
    /// and committing the neighbour's group (`/relay/n-<peer>`).
    pub(crate) fn source_grants(&self, me: &Me) -> io::Result<Vec<Capability>> {
        Ok(vec![
            self.mint(&format!("ro:{}", self.log.filter()))?,
            self.mint(&format!("rw,p={}:{}/>", me.principal, relay::GROUP_PREFIX))?,
        ])
    }

    /// Publish one sealed record under this Mac's producer id on the local connection, dialing
    /// it again after an error.
    pub(crate) fn publish_local(
        &self,
        me: &Me,
        seq: u64,
        subject: &str,
        body: &[u8],
    ) -> io::Result<(u64, bool)> {
        let mut local = self.local.lock_or_recover();
        if local.is_none() {
            let grant = match self.log {
                MeshLog::Tables => format!("rw,p={0}:/t/*/{0}", me.principal),
                MeshLog::Metrics => format!("rw,p={0}:/m/{0}/>", me.principal),
            };
            *local = Some(self.client(me, &grant, Some(relay::IO_WITHIN))?);
        }
        let client = local
            .as_mut()
            .unwrap_or_else(|| unreachable!("the connection was dialed above"));
        let sent = client.publish(me.pid, seq, subject, body);
        if sent
            .as_ref()
            .is_err_and(|e| astream_broker::broker_refusal(e).is_none())
        {
            *local = None;
        }
        sent
    }

    /// Stop serving: the socket's handle goes first, which (as the last listener) ends every
    /// connection. Explicit shutdown also flushes and closes a stream-only broker where the
    /// socket path was too long to serve, even while the mesh still holds its `Arc`.
    fn shut(&self) {
        drop(self.local.lock_or_recover().take());
        drop(self.served.lock_or_recover().take());
        self.broker.shutdown();
    }
}

/// What the revocation pass applied last.
#[derive(Default)]
struct Applied {
    /// The standings, `None` before the first read.
    read: Option<Standings>,
    /// What every broker was given of them: the origins, and the keys and principals revoked.
    verdict: Verdict,
    /// The other current seats' keys (`verdict.origins` less this Mac's), keyed by principal.
    seats_by_principal: BTreeMap<String, [u8; 32]>,
    /// The `Removed` keys whose rows were retired.
    retired: BTreeSet<[u8; 32]>,
    /// The `Removed` keys whose rows could not be retired yet, each said once.
    unretired: BTreeSet<[u8; 32]>,
    /// The job's `bus.log`, while one is hosted and joined ([`Mesh::join_bus`]).
    bus: Option<Arc<BusHost>>,
    /// The `Removed` keys whose halts were retired on that `bus.log`.
    bus_retired: BTreeSet<[u8; 32]>,
    /// The `Removed` keys whose halts could not be retired there yet, each said once.
    bus_unretired: BTreeSet<[u8; 32]>,
}

/// The mesh of one node service: both logs, this Mac's node key, and the membership log's verdict.
pub struct Mesh {
    pub(crate) me: Me,
    state_dir: PathBuf,
    pub(crate) tables: Hosted,
    pub(crate) metrics: Hosted,
    applied: Mutex<Applied>,
    /// Bumped by each pass that changed what is applied: the overlay kicks its relays then.
    pub(crate) generation: AtomicU64,
    pub(crate) clock: Arc<dyn LinkClock>,
    pub(crate) wall: Wall,
    pub(crate) observe: Option<Observe>,
    standings: ReadStandings,
    /// The overlay's bell while one is attached ([`crate::overlay`]).
    pub(crate) wake: Mutex<Option<Weak<Bell>>>,
    /// The peers whose `Bus` streams arrived since the overlay last looked.
    pub(crate) arrived: Mutex<BTreeSet<[u8; 32]>>,
    stopped: AtomicBool,
    watcher: Mutex<Option<(Arc<Bell>, std::thread::JoinHandle<()>)>>,
    /// The egress, the ingest consumer and the `Inventory` exchanges, where the mesh moves tables.
    pub(crate) flow: Mutex<Option<crate::flow::Flow>>,
    /// Bumped, and rung, each time a tables relay's neighbour acks a burst.
    pub(crate) acks: (Mutex<u64>, Condvar),
    /// The neighbours a tables relay follows now, by principal: each one's node key and how many
    /// such relays run.
    pub(crate) heard: Mutex<BTreeMap<String, ([u8; 32], usize)>>,
    /// The job's bus slot this mesh joined, which it leaves as it closes.
    joined: Mutex<Option<Arc<BusSlot>>>,
    /// Held through each close, so a second caller (the orderly quit, racing the job's own close)
    /// returns only once the logs are shut, not as soon as the first caller marked them stopping.
    closing: Mutex<()>,
    /// Presence's board while presence runs on this mesh: the rows [`Mesh::holders`] reads.
    pub(crate) presence: Mutex<Weak<crate::presence::Shared>>,
}

/// The live generation, held weakly so orderly quit can close its logs without owning it.
static QUIT_MESH: Mutex<Weak<Mesh>> = Mutex::new(Weak::new());

/// Flush and mark both mesh logs closed before a quit signal ends the process. Where the job's
/// own close is in progress, it returns once that close is done ([`Mesh::close`]).
pub fn close_mesh_at_quit() {
    let mesh = QUIT_MESH.lock_or_recover().upgrade();
    if let Some(mesh) = mesh {
        mesh.close();
    }
}

impl std::fmt::Debug for Mesh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mesh")
            .field("me", &self.me.principal)
            .finish_non_exhaustive()
    }
}

impl Mesh {
    /// Open both logs in `<state_dir>/mesh/` and apply the membership log's standings once. With
    /// a probe ([`MeshConfig::changed`]), a watcher thread applies them again after each change.
    /// # Errors
    /// Where `fleet-node-key` cannot be read or proves another key, a folder cannot be made
    /// private, a log's key or seal is refused (`aterm_link::at_rest`), a log cannot be opened (a
    /// second node service holds it: `AlreadyExists`) or served, or a lease cannot be taken.
    pub fn open(cfg: MeshConfig) -> io::Result<Arc<Self>> {
        let me = Me::read(&cfg.state_dir)?;
        let mesh_dir = cfg.state_dir.join(MESH_DIR);
        aterm_uds::private_file::ensure_private_dir(&mesh_dir)?;
        let tables = Hosted::open(&mesh_dir, MeshLog::Tables, &cfg, &me)?;
        let metrics = Hosted::open(&mesh_dir, MeshLog::Metrics, &cfg, &me)?;
        let stores = cfg.stores.clone();
        let mesh = Arc::new(Self {
            me,
            state_dir: cfg.state_dir.clone(),
            tables,
            metrics,
            applied: Mutex::new(Applied::default()),
            generation: AtomicU64::new(0),
            clock: cfg.clock,
            wall: cfg.wall,
            observe: cfg.observe,
            standings: cfg.standings,
            wake: Mutex::new(None),
            arrived: Mutex::new(BTreeSet::new()),
            stopped: AtomicBool::new(false),
            watcher: Mutex::new(None),
            flow: Mutex::new(None),
            acks: (Mutex::new(0), Condvar::new()),
            heard: Mutex::new(BTreeMap::new()),
            joined: Mutex::new(None),
            closing: Mutex::new(()),
            presence: Mutex::new(Weak::new()),
        });
        // Registered as soon as it holds both logs, so a quit while it reads the standings or
        // starts its watcher closes them too.
        *QUIT_MESH.lock_or_recover() = Arc::downgrade(&mesh);
        let read = mesh.follow().is_some();
        if let Some(stores) = stores {
            let flow = crate::flow::Flow::start(&mesh, stores);
            match flow {
                Ok(flow) => *mesh.flow.lock_or_recover() = Some(flow),
                Err(e) => {
                    mesh.close();
                    return Err(e);
                }
            }
        }
        if let Some(changed) = cfg.changed {
            let bell = Arc::new(Bell::default());
            let weak = Arc::downgrade(&mesh);
            let rung = Arc::clone(&bell);
            let every = cfg.watch_every;
            let thread = std::thread::Builder::new()
                .name("aterm-mesh-members".to_owned())
                .spawn(move || {
                    // A change the probe saw stays due until a read of the standings succeeds:
                    // the probe answers each change once.
                    let mut due = !read;
                    loop {
                        rung.wait(Some(every));
                        let Some(mesh) = weak.upgrade() else { break };
                        if mesh.is_stopped() {
                            break;
                        }
                        due |= changed();
                        if due {
                            due = mesh.follow().is_none();
                        }
                        mesh.retire_due();
                    }
                })?;
            *mesh.watcher.lock_or_recover() = Some((bell, thread));
        }
        aterm_log::info!(
            "mesh: {} hosts {} and {} in {}",
            mesh.me.principal,
            MeshLog::Tables.file(),
            MeshLog::Metrics.file(),
            mesh_dir.display()
        );
        Ok(mesh)
    }

    /// The node service's state dir.
    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// The folder `log` lives in.
    #[must_use]
    pub fn log_dir(&self, log: MeshLog) -> PathBuf {
        self.state_dir.join(MESH_DIR).join(log.word())
    }

    /// The file of `log`.
    #[must_use]
    pub fn log_file(&self, log: MeshLog) -> PathBuf {
        self.log_dir(log).join(log.file())
    }

    /// This Mac's principal (`n-` and the first eight bytes of its node key in hex).
    #[must_use]
    pub fn principal(&self) -> &str {
        &self.me.principal
    }

    /// This Mac's node key.
    #[must_use]
    pub const fn key(&self) -> [u8; 32] {
        self.me.key
    }

    pub(crate) const fn hosted(&self, log: MeshLog) -> &Hosted {
        match log {
            MeshLog::Tables => &self.tables,
            MeshLog::Metrics => &self.metrics,
        }
    }

    /// The broker of `log`.
    #[must_use]
    pub fn broker(&self, log: MeshLog) -> &Arc<Broker> {
        &self.hosted(log).broker
    }

    /// A client of `log` that reads `filter`, under this Mac's own key, each read waiting at most
    /// `within` (`None`: unbounded, for a tail): the readers the node service runs over its own
    /// logs.
    /// # Errors
    /// Where the connection cannot be made or the grant is refused.
    pub fn read_client(
        &self,
        log: MeshLog,
        filter: &str,
        within: Option<Duration>,
    ) -> io::Result<AnyClient> {
        self.hosted(log)
            .client(&self.me, &format!("ro:{filter}"), within)
    }

    /// Publish `payload` on `subject` of `log` as this Mac: signed by its node key and numbered
    /// under the log's incarnation lease, so the relay carries it to every Mac.
    /// # Errors
    /// [`io::ErrorKind::InvalidInput`] for a subject this Mac does not own;
    /// [`io::ErrorKind::WouldBlock`] while a rebuilt Mac's lease waits for its first neighbour's
    /// high water ([`relay::Origin`]); what the broker refuses.
    pub fn publish(&self, log: MeshLog, subject: &str, payload: &[u8]) -> io::Result<Published> {
        relay::publish(self, log, subject, payload)
    }

    /// Read the membership log's standings and apply them: the revocation pass. A read that
    /// fails keeps what was applied. Answers whether anything changed, or `None` where the read
    /// failed.
    pub fn follow(&self) -> Option<bool> {
        (self.standings)().map(|read| self.apply(read))
    }

    /// The other Macs whose newest presence row names a held topic, by principal (§4.11): the
    /// Macs this Mac's topic broadcasts go to. None while presence does not run here.
    #[must_use]
    pub fn holders(&self) -> BTreeSet<String> {
        let presence = self.presence.lock_or_recover().upgrade();
        presence
            .map(|presence| presence.holders())
            .unwrap_or_default()
    }

    /// The standings applied, `None` before the first read.
    #[must_use]
    pub fn standings(&self) -> Option<Standings> {
        self.applied.lock_or_recover().read.clone()
    }

    /// Apply `read` to every broker as its [`Verdict`] says: revoke each key and each principal that
    /// entered the revoked sets, lift each that left them, install the origins (the uncontested
    /// current seats' keys and this Mac's own), and retire the rows of each key newly `Removed`
    /// whose principal no origin holds (one whose retire fails is tried again at each watch,
    /// [`Self::retire_due`]). This Mac's own key and principal are never revoked. Broker state is a
    /// function of the read: a key's principal is revoked or lifted with the class it stands in,
    /// never with one key's change. Answers whether anything changed.
    pub fn apply(&self, read: Standings) -> bool {
        let mut applied = self.applied.lock_or_recover();
        if applied.read.as_ref() == Some(&read) {
            return false;
        }
        let verdict = Verdict::of(&read, &self.me.key, class_of);
        let was = &applied.verdict;
        let keys_in: Vec<[u8; 32]> = verdict.keys.difference(&was.keys).copied().collect();
        let keys_out: Vec<[u8; 32]> = was.keys.difference(&verdict.keys).copied().collect();
        let principals_in: Vec<String> = verdict
            .principals
            .difference(&was.principals)
            .cloned()
            .collect();
        let principals_out: Vec<String> = was
            .principals
            .difference(&verdict.principals)
            .cloned()
            .collect();
        let bus = applied.bus.as_ref().map(|host| Arc::clone(&host.broker));
        for broker in [&self.tables.broker, &self.metrics.broker]
            .into_iter()
            .chain(bus.as_ref())
        {
            for key in &keys_in {
                broker.revoke_key(*key);
            }
            for principal in &principals_in {
                broker.revoke_principal(principal);
            }
            for key in &keys_out {
                broker.unrevoke_key(key);
            }
            for principal in &principals_out {
                broker.unrevoke_principal(principal);
            }
            broker.set_origin_keys(verdict.origins.iter().copied());
        }
        // Before the new verdict admits a rotated seat's new key, its pushes' resends of what its
        // old one landed here are known for what they are.
        if let Some(bus) = &bus {
            bus.set_producer_lineage(lineage(&read, &self.me.key));
        }
        // A key that left `Removed` is retired again should it come back.
        applied.retired.retain(|key| read.removed.contains(key));
        applied.unretired.retain(|key| read.removed.contains(key));
        applied.bus_retired.retain(|key| read.removed.contains(key));
        applied
            .bus_unretired
            .retain(|key| read.removed.contains(key));
        for key in &keys_in {
            let principal = astream_broker::identity_principal(key);
            if read.current.contains(key) {
                aterm_log::info!(
                    "mesh: {principal} is revoked: another current key shares its principal"
                );
            } else {
                aterm_log::info!("mesh: {principal} is revoked: the membership log took it out");
            }
        }
        for key in &keys_out {
            aterm_log::info!(
                "mesh: {} is un-revoked: the membership log seats it again",
                astream_broker::identity_principal(key)
            );
        }
        let alone = verdict.origins.len() == 1;
        applied.seats_by_principal = verdict
            .origins
            .iter()
            .filter(|&&key| key != self.me.key)
            .map(|&key| (astream_broker::identity_principal(&key), key))
            .collect();
        applied.verdict = verdict;
        applied.read = Some(read);
        self.generation.fetch_add(1, Ordering::AcqRel);
        drop(applied);
        self.retire_due();
        if alone {
            // No other seat can hold a record of this Mac: its lease needs no neighbour's word.
            for hosted in [&self.tables, &self.metrics] {
                hosted.origin.lock_or_recover().settle_alone();
            }
        }
        self.poke();
        // A writer seated now may be one a waiting table record names.
        if let Some(ingest) = self.ingest() {
            ingest.kick();
        }
        true
    }

    /// Retire the rows of every `Removed` key not retired yet whose principal stands revoked: rows
    /// go by principal, so never those of a principal an origin holds (this Mac's own among them).
    /// It runs outside the lock every stream's admission reads: the keys are revoked already.
    pub(crate) fn retire_due(&self) {
        let (due, bus_due) = {
            let applied = self.applied.lock_or_recover();
            let Some(read) = applied.read.as_ref() else {
                return;
            };
            let has_bus = applied.bus.is_some();
            let mut due = Vec::new();
            let mut bus_due = Vec::new();
            for key in &read.removed {
                let need_metrics = !applied.retired.contains(key);
                let need_bus = has_bus && !applied.bus_retired.contains(key);
                if (!need_metrics && !need_bus)
                    || !applied
                        .verdict
                        .principals
                        .contains(principal_str(&principal_buf(key)))
                {
                    continue;
                }
                if need_metrics {
                    due.push(*key);
                }
                if need_bus {
                    bus_due.push(*key);
                }
            }
            (due, bus_due)
        };
        for key in bus_due {
            self.retire_halts(&key);
        }
        for key in due {
            let retired = self.retire_rows(&key);
            let mut applied = self.applied.lock_or_recover();
            match retired {
                Ok(()) => {
                    applied.retired.insert(key);
                    applied.unretired.remove(&key);
                    drop(applied);
                    self.report(&Event::Retired {
                        principal: astream_broker::identity_principal(&key),
                    });
                }
                Err(e) => {
                    if applied.unretired.insert(key) {
                        aterm_log::warn!(
                            "mesh: the rows of {} could not be retired yet: {e}",
                            astream_broker::identity_principal(&key)
                        );
                    }
                }
            }
        }
    }

    /// Retire every row of the removed Mac `key` on `metrics.log`, so its rows leave every view.
    fn retire_rows(&self, key: &[u8; 32]) -> io::Result<()> {
        #[cfg(any(test, debug_assertions))]
        if RETIRE_REFUSED.with(std::cell::Cell::get) {
            return Err(io::Error::other("a test refused the retirement"));
        }
        let principal = astream_broker::identity_principal(key);
        let pid = astream_broker::producer_id_of(&principal);
        let filter = MeshLog::retired_filter(&principal);
        let hosted = &self.metrics;
        let mut client =
            hosted.client(&self.me, &format!("rw:{filter}"), Some(relay::IO_WITHIN))?;
        let (rows, _) = client.last_all(&filter)?;
        for (_, subject, _) in rows {
            client.retire_subject(pid, &subject)?;
        }
        Ok(())
    }

    /// Retire the halts the removed Mac `key` originated on the joined `bus.log`, so no halt
    /// outlives its origin (§4.11): every `/f/<fleet>/halt/n-<id>` row it holds. A failure is
    /// said once and tried again at the next watch, as a metrics retirement is.
    fn retire_halts(&self, key: &[u8; 32]) {
        let Some(host) = self.applied.lock_or_recover().bus.clone() else {
            return;
        };
        let principal = astream_broker::identity_principal(key);
        let retired = host.retire_halts_of(self.me.key, &principal);
        let mut applied = self.applied.lock_or_recover();
        // The host may have been replaced meanwhile: only its own retirement counts.
        if !applied
            .bus
            .as_ref()
            .is_some_and(|now| Arc::ptr_eq(now, &host))
        {
            return;
        }
        match retired {
            Ok(()) => {
                applied.bus_retired.insert(*key);
                applied.bus_unretired.remove(key);
            }
            Err(e) => {
                if applied.bus_unretired.insert(*key) {
                    aterm_log::warn!(
                        "mesh: the halts of {principal} could not be retired on bus.log yet: {e}"
                    );
                }
            }
        }
    }

    /// Join the job's `bus.log` to this mesh's revocation pass and its `Bus` service: the host
    /// `slot` holds now, and each host it holds later ([`BusSlot::set`]), takes every revoked
    /// key, the origin keys and the retirement of each removed Mac's halts, and a neighbour's
    /// `Bus` stream for `bus.log` is served with it. A mesh joins one slot; joining another
    /// leaves the first.
    pub fn join_bus(self: &Arc<Self>, slot: &Arc<BusSlot>) {
        let was = self.joined.lock_or_recover().replace(Arc::clone(slot));
        if let Some(was) = was.filter(|was| !Arc::ptr_eq(was, slot)) {
            let mut was_inner = was.inner.lock_or_recover();
            if was_inner
                .mesh
                .upgrade()
                .is_some_and(|mesh| Arc::ptr_eq(&mesh, self))
            {
                was_inner.mesh = Weak::new();
            }
        }
        let mut inner = slot.inner.lock_or_recover();
        inner.mesh = Arc::downgrade(self);
        self.bus_now(inner.host.clone());
        drop(inner);
        self.retire_due();
    }

    /// Make `host` the `bus.log` this mesh judges: a new broker takes the whole of what is
    /// applied (astream keeps no revocation across a reopen), and its halts are retired anew.
    /// Called with the slot's lock held, so a host's arrival and a join never cross.
    fn bus_now(&self, host: Option<Arc<BusHost>>) {
        let mut applied = self.applied.lock_or_recover();
        let same = match (applied.bus.as_ref(), host.as_ref()) {
            (Some(was), Some(is)) => Arc::ptr_eq(was, is),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        if let Some(host) = host.as_ref()
            && let Some(read) = applied.read.as_ref()
        {
            host.broker
                .set_producer_lineage(lineage(read, &self.me.key));
            for key in &applied.verdict.keys {
                host.broker.revoke_key(*key);
            }
            for principal in &applied.verdict.principals {
                host.broker.revoke_principal(principal);
            }
            host.broker
                .set_origin_keys(applied.verdict.origins.iter().copied());
        }
        applied.bus = host;
        applied.bus_retired.clear();
        applied.bus_unretired.clear();
    }

    /// The `bus.log` joined now.
    #[must_use]
    pub fn bus(&self) -> Option<Arc<BusHost>> {
        self.applied.lock_or_recover().bus.clone()
    }

    /// Whether `key` may stream to this Mac now, an origin other than this Mac ([`Verdict`]):
    /// `None` before the standings were read.
    pub(crate) fn admits(&self, key: &[u8; 32]) -> Option<bool> {
        let applied = self.applied.lock_or_recover();
        applied.read.as_ref()?;
        Some(*key != self.me.key && applied.verdict.origins.contains(key))
    }

    /// The other current seats' keys, the origins but this Mac's ([`Verdict`]): `None` before the
    /// standings were read.
    #[cfg(test)]
    pub(crate) fn seats(&self) -> Option<BTreeSet<[u8; 32]>> {
        let applied = self.applied.lock_or_recover();
        applied.read.as_ref()?;
        let mut seats = applied.verdict.origins.clone();
        seats.remove(&self.me.key);
        Some(seats)
    }

    /// The other current seats' keys (`Verdict::origins` less this Mac's), keyed by principal:
    /// `None` before the standings were read.
    pub(crate) fn seats_by_principal(&self) -> Option<BTreeMap<String, [u8; 32]>> {
        let applied = self.applied.lock_or_recover();
        applied.read.as_ref()?;
        Some(applied.seats_by_principal.clone())
    }

    /// The current seat key for `principal` (excluding this Mac): `None` before the standings
    /// were read or where `principal` is not an uncontested current seat.
    pub(crate) fn seat_key(&self, principal: &str) -> Option<[u8; 32]> {
        let applied = self.applied.lock_or_recover();
        applied.read.as_ref()?;
        applied.seats_by_principal.get(principal).copied()
    }

    /// The origin keys every broker holds now ([`Verdict`]): this Mac's alone before the first read.
    #[cfg(test)]
    pub(crate) fn origins(&self) -> BTreeSet<[u8; 32]> {
        let applied = self.applied.lock_or_recover();
        if applied.read.is_none() {
            return BTreeSet::from([self.me.key]);
        }
        applied.verdict.origins.clone()
    }

    /// Whether a relayed record of `owner`'s is still owed to a neighbour that did not take it
    /// (`FederationBridge::hold_while`): this Mac seats `owner` as an origin, or has read no
    /// standings yet, when it cannot tell and holds rather than passes.
    pub(crate) fn still_seats(&self, owner: &str) -> bool {
        let applied = self.applied.lock_or_recover();
        applied.read.is_none()
            || owner == self.me.principal
            || applied.seats_by_principal.contains_key(owner)
    }

    /// Serve a `Bus` stream a neighbour opened on its link: refused `unserved` for a log the mesh
    /// does not host, `unread` before the standings were read, `out` for a key that is not an
    /// origin ([`Verdict`]: removed, retired, never seated, or contested); otherwise accepted, handed its grants, and adopted by the log's broker under
    /// the peer's key, which refuses a revoked key and ends its stream when the key is revoked.
    pub fn serve_bus(&self, incoming: Incoming) {
        if incoming.log() == Some(BusLog::Bus) {
            return self.serve_bus_log(incoming);
        }
        let Some(log) = incoming.log().and_then(MeshLog::of_bus) else {
            return incoming.refuse("unserved");
        };
        if self.is_stopped() {
            return incoming.refuse("unserved");
        }
        let key = *incoming.peer().key.bytes();
        match self.admits(&key) {
            None => return incoming.refuse("unread"),
            Some(false) => return incoming.refuse("out"),
            Some(true) => {}
        }
        let peer = astream_broker::identity_principal(&key);
        let hosted = self.hosted(log);
        let grants = match hosted.grants_for(&peer) {
            Ok(grants) => grants,
            Err(e) => {
                aterm_log::warn!("mesh: no grants for {peer}: {e}");
                return incoming.refuse("unserved");
            }
        };
        let stream = incoming.accept();
        let served = relay::hand_grants(&stream, &grants).and_then(|()| {
            hosted
                .broker
                .serve_stream(Arc::new(relay::BusLink::new(stream, Vec::new())), key)
        });
        match served {
            Ok(handle) => drop(handle),
            Err(e) => aterm_log::info!("mesh: {peer}'s {} stream ended: {e}", log.word()),
        }
        self.arrived.lock_or_recover().insert(key);
        self.poke();
    }

    /// Serve a `Bus` stream for `bus.log` (§4.11): refused `unserved` with no `bus.log` joined,
    /// `unread` before the standings were read and `out` for a key that is not an origin
    /// ([`Verdict`]) (the node service checked the drive admission before it handed the stream here);
    /// otherwise handed the ring minted for the peer under the root's `mint.secret`, and adopted
    /// by the log's broker under the peer's key, which binds every grant of the ring to that peer,
    /// refuses a revoked key and ends its stream when the key is revoked.
    fn serve_bus_log(&self, incoming: Incoming) {
        if self.is_stopped() {
            return incoming.refuse("unserved");
        }
        let Some(host) = self.bus() else {
            return incoming.refuse("unserved");
        };
        let key = *incoming.peer().key.bytes();
        match self.admits(&key) {
            None => return incoming.refuse("unread"),
            Some(false) => return incoming.refuse("out"),
            Some(true) => {}
        }
        let peer = astream_broker::identity_principal(&key);
        let olds = self
            .standings()
            .map(|read| retired_of(&read, &key))
            .unwrap_or_default();
        let grants = match host.ring_for(&self.me.principal, &peer, &olds) {
            Ok(grants) => grants,
            Err(e) => {
                aterm_log::warn!("mesh: no bus.log ring for {peer}: {e}");
                return incoming.refuse("unserved");
            }
        };
        let stream = incoming.accept();
        let served = relay::hand_grants(&stream, &grants).and_then(|()| {
            host.broker
                .serve_stream(Arc::new(relay::BusLink::new(stream, Vec::new())), key)
        });
        match served {
            Ok(handle) => drop(handle),
            Err(e) => aterm_log::info!("mesh: {peer}'s bus.log stream ended: {e}"),
        }
    }

    /// Wake the membership watcher now, as its period passing would.
    #[cfg(test)]
    pub(crate) fn watch_now(&self) {
        if let Some((bell, _)) = self.watcher.lock_or_recover().as_ref() {
            bell.ring();
        }
    }

    /// Wake the overlay, if one is attached.
    pub fn poke(&self) {
        if let Some(bell) = self.wake.lock_or_recover().as_ref().and_then(Weak::upgrade) {
            bell.ring();
        }
    }

    /// Report `event` to the observer.
    pub(crate) fn report(&self, event: &Event) {
        if let Event::Settled { log, .. } = event
            && *log == MeshLog::Tables
            && let Some(egress) = self.egress()
        {
            // The lease numbers records now: what the egress held back goes.
            egress.ring();
        }
        if let Some(observe) = &self.observe {
            observe(event);
        }
    }

    /// Whether [`Self::close`] ran.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    /// Stop the mesh: the watcher ends, and each log stops serving and closes cleanly once the
    /// last relay holding it ends. Detach the overlay ([`crate::Attached`]) first. A second
    /// caller waits for the close in progress and returns once it is done.
    pub fn close(&self) {
        let _closing = self.closing.lock_or_recover();
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        // A push waiting for a relay ack sees the stop now, not at its deadline.
        drop(self.acks.0.lock_or_recover());
        self.acks.1.notify_all();
        if let Some((bell, thread)) = self.watcher.lock_or_recover().take() {
            bell.ring();
            if thread.thread().id() != std::thread::current().id() {
                let _ = thread.join();
            }
        }
        self.poke();
        let flow = self.flow.lock_or_recover().take();
        if let Some(mut flow) = flow {
            flow.stop();
        }
        let joined = self.joined.lock_or_recover().take();
        if let Some(slot) = joined {
            let mut inner = slot.inner.lock_or_recover();
            if inner
                .mesh
                .upgrade()
                .is_none_or(|mesh| std::ptr::eq(Arc::as_ptr(&mesh), self))
            {
                inner.mesh = Weak::new();
            }
        }
        self.applied.lock_or_recover().bus = None;
        for hosted in [&self.tables, &self.metrics] {
            hosted.shut();
        }
    }
}

impl Drop for Mesh {
    fn drop(&mut self) {
        self.close();
    }
}

/// `bus.log`'s file name in the fabric root, where the broker job kept it.
pub const BUS_LOG: &str = "bus.log";

/// `bus.log`'s socket in the fabric root: the one every local client has always dialed.
pub const BUS_SOCKET: &str = "bus.sock";

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The negative control of the bus ring (`unbound-ring`): a host opened on a thread where it
    /// is set mints each neighbour's mail grant unbound over every lane (`rw:/f/<fleet>/in/>`), so
    /// the neighbour can post as any sender, another Mac or a person. A lab instance turns it on
    /// for its whole process with `unbound-ring` in `ATERM_TEST_NEGATIVE_CONTROL`.
    pub static UNBOUND_RING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether [`UNBOUND_RING`] is on: set on this thread, or named for this process.
#[cfg(any(test, debug_assertions))]
fn unbound_ring() -> bool {
    UNBOUND_RING.with(std::cell::Cell::get) || aterm_types::negative_control::on("unbound-ring")
}

/// The grants a neighbour `peer` (its principal) is served this Mac's (`me`) `bus.log` with, for
/// `fleet` (§4.11): [`aterm_link::federate::peer_ring`]'s, mail to this Mac sent as the peer
/// itself, or under a node its seat held before a key rotation (`olds`), the peer's topics, the
/// halt rows and the relay of halts.
#[must_use]
pub fn ring(fleet: &str, me: &str, peer: &str, olds: &BTreeSet<String>) -> Vec<String> {
    aterm_link::federate::peer_ring(fleet, me, peer, olds)
}

/// The retired nodes of the seat whose node key is `key` now, by `read`: each key a rotation
/// replaced with it (`Standings::rotated`), as a node, but one that names a current seat's node
/// too, as the hand-off leaves it out (`Handoff::retired`): a ring granting it would let the
/// seat write as that other Mac.
#[must_use]
pub fn retired_of(read: &Standings, key: &[u8; 32]) -> BTreeSet<String> {
    let seated: BTreeSet<String> = read
        .current
        .iter()
        .map(astream_broker::identity_principal)
        .collect();
    read.rotated
        .iter()
        .filter(|(_, now)| *now == key)
        .map(|(old, _)| astream_broker::identity_principal(old))
        .filter(|old| !seated.contains(old))
        .collect()
}

/// Which producers continue others' sequences on this Mac's (`me`) `bus.log`, by `read`: each
/// other seat's node now continues every node a rotation retired it from
/// ([`Broker::set_producer_lineage`](astream_broker::Broker::set_producer_lineage)). Another
/// seat writes here only by its push, a mirror numbered by its own `bus.log`'s offsets, which a
/// rotation keeps; so a burst its old node landed here and never committed at home, sent again
/// under its new node, lands once (§11 C3b). This Mac's own nodes are left out: its bridge
/// numbers its records by incarnation, not by a log both nodes share.
#[must_use]
pub fn lineage(read: &Standings, me: &[u8; 32]) -> Vec<(u64, Vec<u64>)> {
    let mut lineage: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for (old, now) in &read.rotated {
        if now == me || old == me {
            continue;
        }
        lineage
            .entry(astream_cap::producer_id_of(
                &astream_broker::identity_principal(now),
            ))
            .or_default()
            .push(astream_cap::producer_id_of(
                &astream_broker::identity_principal(old),
            ));
    }
    lineage.into_iter().collect()
}

/// Open a `Bus` stream for a neighbour's `bus.log` over `link` and read the ring it hands this
/// Mac: the stream as astream's link, and the grants to attach on it.
/// # Errors
/// What `Link::open` answers ([`io::ErrorKind::PermissionDenied`] with the neighbour's reason
/// where it refused: `unserved` with no `bus.log` hosted there, the drive admission's refusal,
/// `out` for a key that is no current seat), or a ring that cannot be read.
pub fn dial_bus(
    link: &aterm_net::fleet::link::Link,
) -> io::Result<(relay::BusLink, Vec<Capability>)> {
    let stream = link.open(
        aterm_net::fleet::FleetOp::Bus,
        Some(BusLog::Bus),
        relay::IO_WITHIN,
    )?;
    let (grants, ahead) = relay::read_grants(&stream)?;
    Ok((relay::BusLink::new(stream, ahead), grants))
}

/// The boot `bus.log` names for its legacy mark's verdict: the machine's, or on a test's thread
/// [`BOOT`].
fn boot_id() -> Option<[u8; 16]> {
    match boot() {
        Boot::Known(id) => Some(id),
        _ => None,
    }
}

/// Open `log` as the node service hosts `bus.log`, under `opts` (the tier's options and the
/// guard its caller set), exactly as `aterm link broker` opened it: sealed at rest by aterm-link's
/// rules unless `opts` names a key already (`at_rest::prepare`: a key beside the log, or one
/// minted where `mint.secret` stands beside it), its legacy mark folded into the open
/// (`durability::open_with`), retained by aterm's default rules and limited by its default rate
/// limit (each journaled and skipped where it cannot be set, as the broker did). The fabric
/// world suite opens its broker through this too, so it runs against what the node service
/// hosts.
/// # Errors
/// A seal aterm-link refuses (an unfinished retire, a damaged or missing key), a log another
/// broker holds (`AlreadyExists`), and astream's own refusal to open it.
pub fn open_bus(
    log: &Path,
    mut opts: astream_broker::OpenOptions,
    backups: Backups,
) -> io::Result<Broker> {
    let named = log.display();
    if opts.at_rest_key.is_none() {
        let prepared = at_rest::prepare(log, backups).map_err(|r| refused(&r))?;
        for note in &prepared.notes {
            aterm_log::info!("mesh: {named}: {note}");
        }
        opts.at_rest_key = prepared.key.as_ref().map(at_rest::Key::bytes);
    }
    opts.require_bound_at_rest = opts.at_rest_key.is_some();
    let opened = aterm_link::durability::open_with(log, opts, boot_id())?;
    if let Some(why) = &opened.renewed {
        aterm_log::info!(
            "mesh: {named}: {}: its generation moves, and every bridge and notifier resumes where \
             it continues",
            why.describe()
        );
    }
    if let Some(kept) = &opened.mark_kept {
        aterm_log::warn!(
            "mesh: could not remove the legacy relaxed mark {kept}; the log's generation moves \
             once more after the next reboot"
        );
    }
    let broker = opened.broker;
    if let Err(e) = aterm_link::broker_rules::retain(&broker) {
        aterm_log::warn!(
            "mesh: {named}: its retention rules could not be set ({e}); it serves unretained"
        );
    }
    if let Err(e) = aterm_link::broker_rules::limit(&broker) {
        aterm_log::warn!(
            "mesh: {named}: its rate limit could not be set ({e}); it serves with no limit"
        );
    }
    Ok(broker)
}

/// Where and how the node service hosts `bus.log`.
#[derive(Clone, Debug)]
pub struct BusSpec {
    /// The fabric root: `bus.log`, `bus.sock` and `mint.secret` are in it.
    pub root: PathBuf,
    /// The fleet the local bridge serves: the ring's subjects name it.
    pub fleet: String,
    /// The tier `bus.log` acks under.
    pub tier: Durability,
    /// How `rest.key` is marked for backups.
    pub backups: Backups,
}

impl BusSpec {
    /// Whether `other` hosts the same log the same way (the backup mark aside).
    fn same(&self, other: &Self) -> bool {
        self.root == other.root && self.fleet == other.fleet && self.tier == other.tier
    }
}

/// `bus.log` as the node service hosts it: its broker, served on `bus.sock`, and the root's mint
/// secret, which guards it and mints each neighbour's ring.
pub struct BusHost {
    broker: Arc<Broker>,
    served: Mutex<Option<BrokerHandle>>,
    spec: BusSpec,
    secret: Secret,
    /// [`UNBOUND_RING`] as the opening thread read it.
    #[cfg(any(test, debug_assertions))]
    unbound: bool,
}

impl std::fmt::Debug for BusHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BusHost")
            .field("spec", &self.spec)
            .finish_non_exhaustive()
    }
}

impl BusHost {
    /// Open `bus.log` in `spec.root` under its `mint.secret` ([`open_bus`]), serve it on
    /// `bus.sock`, and keep its health file, as the broker job did.
    ///
    /// # Errors
    /// A mint secret aterm-link's rule refuses (`transport::read_mint_secret`), a socket path
    /// past `sun_path` (`InvalidInput`), what [`open_bus`] refuses, and a socket that cannot be
    /// served.
    pub fn open(spec: BusSpec) -> io::Result<Self> {
        let secret_path = spec.root.join(aterm_link::enable::MINT_SECRET);
        let shown = secret_path.to_string_lossy().into_owned();
        let read = aterm_link::transport::read_mint_secret(&shown)
            .map_err(|e| io::Error::new(e.kind(), aterm_link::transport::describe(&shown, &e)))?;
        let secret = Secret::from_vec(read);
        let sock = spec.root.join(BUS_SOCKET);
        if sock.as_os_str().len() >= SUN_PATH_MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is too long a socket path", sock.display()),
            ));
        }
        let log = spec.root.join(BUS_LOG);
        let created = std::fs::symlink_metadata(&log).is_err();
        let mut opts = spec.tier.options();
        opts.guard = Some(secret.expose().to_vec());
        let broker = open_bus(&log, opts, spec.backups)?;
        let served = match broker.serve(&sock) {
            Ok(served) => served,
            Err(e) => {
                drop(broker);
                if created
                    && std::fs::symlink_metadata(&log).is_ok_and(|m| m.is_file() && m.len() == 0)
                {
                    let _ = std::fs::remove_file(&log);
                }
                return Err(io::Error::new(
                    e.kind(),
                    format!("serve {}: {e}", sock.display()),
                ));
            }
        };
        if let Err(e) = broker.keep_health(aterm_link::broker_health::HEALTH_EVERY) {
            aterm_log::warn!(
                "mesh: {}: its health could not be written ({e}); astream tries again",
                astream_broker::health_path(&log).display()
            );
        }
        aterm_log::info!(
            "mesh: hosts {} on {} for fleet {}, durability {}, at-rest={}, log {}",
            log.display(),
            sock.display(),
            spec.fleet,
            spec.tier.name(),
            at_rest::word(&log),
            broker.log_identity()
        );
        Ok(Self {
            broker: Arc::new(broker),
            served: Mutex::new(Some(served)),
            spec,
            secret,
            #[cfg(any(test, debug_assertions))]
            unbound: unbound_ring(),
        })
    }

    /// Its broker.
    #[must_use]
    pub const fn broker(&self) -> &Arc<Broker> {
        &self.broker
    }

    /// How it is hosted.
    #[must_use]
    pub const fn spec(&self) -> &BusSpec {
        &self.spec
    }

    /// The socket it is served on.
    #[must_use]
    pub fn sock(&self) -> PathBuf {
        self.spec.root.join(BUS_SOCKET)
    }

    /// Why its log is poisoned, if it is: the node service reopens it.
    #[must_use]
    pub fn poisoned(&self) -> Option<String> {
        self.broker.poisoned()
    }

    /// Whether `bus.sock` names its listener still (`None`: unknown, or no longer served).
    #[must_use]
    pub fn names_listener(&self) -> Option<bool> {
        self.served
            .lock_or_recover()
            .as_ref()
            .and_then(BrokerHandle::names_listener)
    }

    /// A grant minted under the root's mint secret.
    /// # Errors
    /// A grant astream does not parse.
    pub fn mint(&self, grant: &str) -> io::Result<Capability> {
        astream_broker::mint(self.secret.expose(), grant)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
    }

    /// The ring neighbour `peer`, whose seat held the nodes `olds` before, is served with by this
    /// Mac (`me`) ([`ring`]), minted.
    /// # Errors
    /// A peer principal astream does not take in a grant.
    pub fn ring_for(
        &self,
        me: &str,
        peer: &str,
        olds: &BTreeSet<String>,
    ) -> io::Result<Vec<Capability>> {
        let grants = ring(&self.spec.fleet, me, peer, olds);
        #[cfg(any(test, debug_assertions))]
        let grants = if self.unbound {
            let mut unbound = grants;
            unbound[0] = format!("rw:/f/{}/in/>", self.spec.fleet);
            unbound
        } else {
            grants
        };
        grants.iter().map(|grant| self.mint(grant)).collect()
    }

    /// A client of this broker under `key` (this Mac's node key, which is never revoked here),
    /// holding `grant` minted under the mint secret, each read and write bounded by
    /// [`relay::IO_WITHIN`].
    fn client(&self, key: [u8; 32], grant: &str) -> io::Result<AnyClient> {
        let cap = self.mint(grant)?;
        let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
        drop(self.broker.serve_stream(Arc::new(theirs), key)?);
        let link: Arc<dyn StreamLink> = Arc::new(ours);
        link.set_read_timeout(Some(relay::IO_WITHIN))?;
        link.set_write_timeout(Some(relay::IO_WITHIN))?;
        let (mut client, _closer) = astream_broker::connect_stream(link);
        client.attach_all([(cap.filter.as_str(), &cap.tag[..])])?;
        Ok(client)
    }

    /// Retire every halt row `principal` (a removed Mac) originated, on every fleet's halt
    /// subject, under its producer id, as this Mac (`key`).
    fn retire_halts_of(&self, key: [u8; 32], principal: &str) -> io::Result<()> {
        #[cfg(any(test, debug_assertions))]
        if RETIRE_REFUSED.with(std::cell::Cell::get) {
            return Err(io::Error::other("a test refused the retirement"));
        }
        let filter = format!("/f/*/halt/{principal}");
        let mut client = self.client(key, &format!("rw:{filter}"))?;
        let pid = astream_broker::producer_id_of(principal);
        let (rows, _) = client.last_all(&filter)?;
        for (_, subject, _) in rows {
            client.retire_subject(pid, &subject)?;
        }
        Ok(())
    }

    /// Stop serving: the socket's handle goes, which (as its last listener) ends every
    /// connection and closes the log cleanly. Idempotent.
    pub fn shut(&self) {
        drop(self.served.lock_or_recover().take());
    }
}

impl Drop for BusHost {
    fn drop(&mut self) {
        self.shut();
    }
}

/// The job's `bus.log` slot: the host its keeper holds now, and the mesh that judges it. The
/// keeper and the mesh live apart (the mesh opens and closes with the node key and the
/// generations; `bus.log` serves local clients whatever they do), and meet here.
#[derive(Default)]
pub struct BusSlot {
    inner: Mutex<SlotInner>,
}

#[derive(Default)]
struct SlotInner {
    host: Option<Arc<BusHost>>,
    mesh: Weak<Mesh>,
}

impl std::fmt::Debug for BusSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BusSlot")
            .field("host", &self.inner.lock_or_recover().host)
            .finish_non_exhaustive()
    }
}

impl BusSlot {
    /// The host now.
    #[must_use]
    pub fn host(&self) -> Option<Arc<BusHost>> {
        self.inner.lock_or_recover().host.clone()
    }

    /// Hold `host` (or none), and hand it to the mesh that joined, which applies everything it
    /// applied to the new broker before any stream of it is served.
    pub fn set(&self, host: Option<Arc<BusHost>>) {
        let mut inner = self.inner.lock_or_recover();
        inner.host.clone_from(&host);
        let mesh = inner.mesh.upgrade();
        if let Some(mesh) = &mesh {
            mesh.bus_now(host);
        }
        drop(inner);
        if let Some(mesh) = mesh {
            mesh.retire_due();
        }
    }
}

/// How the keeper reads whether, and how, `bus.log` is hosted now
/// (`aterm_link::enable::bus_hosting` in the node service).
pub type Hosting = Arc<dyn Fn() -> BusHosting + Send + Sync>;

/// How the keeper retires the separate broker job before it opens `bus.log`
/// (`aterm_link::enable::retire_broker_job` in the node service).
pub type Retire = Arc<dyn Fn() -> Result<RetiredJob, String> + Send + Sync>;

/// How the node service keeps `bus.log`.
#[derive(Clone)]
pub struct BusKeeperConfig {
    /// The fabric root.
    pub root: PathBuf,
    /// How `rest.key` is marked for backups.
    pub backups: Backups,
    /// Whether, and how, it is hosted now: read at each pass.
    pub hosting: Hosting,
    /// Retires the broker job before each open; `None` where there is none to retire (a test).
    pub retire: Option<Retire>,
    /// How often the keeper looks, beside each [`BusKeeper::poke`].
    pub every: Duration,
}

impl BusKeeperConfig {
    /// The node service's: the fabric root of this login as aterm-link resolves it
    /// (`enable::Paths::resolve`), whether the fabric is on there (`enable::bus_hosting`), the
    /// broker job it retires (`enable::retire_broker_job`), `rest.key` marked for system backups,
    /// looked at `every`.
    /// # Errors
    /// Where the root does not resolve (no `$HOME`, no aterm binary).
    pub fn live(every: Duration) -> Result<Self, String> {
        let paths = Arc::new(aterm_link::enable::Paths::resolve(None)?);
        let root = paths.root.clone();
        let (hosting, retire) = (Arc::clone(&paths), paths);
        Ok(Self {
            root,
            backups: Backups::SYSTEM,
            hosting: Arc::new(move || aterm_link::enable::bus_hosting(&hosting)),
            retire: Some(Arc::new(move || {
                aterm_link::enable::retire_broker_job(&retire)
            })),
            every,
        })
    }
}

/// The node service's keeper of `bus.log` (§11 C2): a thread that, at each pass, opens it where
/// the fabric is on (retiring the broker job first, so the job's process lets go of its lock),
/// reopens it in process where its log is poisoned, its socket path no longer names it, or its
/// fleet or tier changed, and closes it where the fabric is off, unprovisioned, or handed to a
/// person's broker. The job ends with the keeper, closing the log cleanly.
pub struct BusKeeper {
    slot: Arc<BusSlot>,
    bell: Arc<Bell>,
    stopped: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for BusKeeper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BusKeeper")
            .field("slot", &self.slot)
            .finish_non_exhaustive()
    }
}

/// The slot of this process's keeper, for the orderly quit ([`close_bus_at_quit`]).
static QUIT_SLOT: Mutex<Weak<BusSlot>> = Mutex::new(Weak::new());

/// Close the keeper's `bus.log` cleanly now: the node service's orderly quit, which ends the
/// process without running the keeper's drop, calls it first, so a weak tier's log is flushed in
/// full and marked closed.
pub fn close_bus_at_quit() {
    let slot = QUIT_SLOT.lock_or_recover().upgrade();
    if let Some(host) = slot.as_ref().and_then(|slot| slot.host()) {
        host.shut();
    }
}

impl BusKeeper {
    /// Start the keeper; its first pass runs at once on its own thread.
    /// # Errors
    /// Where the thread cannot be spawned.
    pub fn start(cfg: BusKeeperConfig) -> io::Result<Self> {
        let slot = Arc::new(BusSlot::default());
        let bell = Arc::new(Bell::default());
        let stopped = Arc::new(AtomicBool::new(false));
        let thread = {
            let (slot, bell, stopped) =
                (Arc::clone(&slot), Arc::clone(&bell), Arc::clone(&stopped));
            std::thread::Builder::new()
                .name("aterm-mesh-bus".to_owned())
                .spawn(move || {
                    let mut keep = Keep { cfg, said: None };
                    while !stopped.load(Ordering::Acquire) {
                        keep.pass(&slot);
                        bell.wait(Some(keep.cfg.every));
                    }
                    keep.close(&slot, "the node service ends");
                })?
        };
        *QUIT_SLOT.lock_or_recover() = Arc::downgrade(&slot);
        Ok(Self {
            slot,
            bell,
            stopped,
            thread: Some(thread),
        })
    }

    /// The slot it keeps `bus.log` in: what a mesh joins ([`Mesh::join_bus`]).
    #[must_use]
    pub const fn slot(&self) -> &Arc<BusSlot> {
        &self.slot
    }

    /// Look again now, as the period passing would.
    pub fn poke(&self) {
        self.bell.ring();
    }
}

impl Drop for BusKeeper {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.bell.ring();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The keeper's own state: its configuration and the last failure it journaled.
struct Keep {
    cfg: BusKeeperConfig,
    said: Option<String>,
}

impl Keep {
    /// Journal `why` once, until something else happens.
    fn say(&mut self, why: String) {
        if self.said.as_deref() != Some(why.as_str()) {
            aterm_log::warn!("{why}");
            self.said = Some(why);
        }
    }

    /// Close the host `slot` holds, saying `why`.
    fn close(&self, slot: &BusSlot, why: &str) {
        if let Some(host) = slot.host() {
            aterm_log::info!(
                "mesh: {} closes: {why}",
                host.spec.root.join(BUS_LOG).display()
            );
            slot.set(None);
            host.shut();
        }
    }

    /// What `bus.log` is hosted as now: its spec where the fabric is on, else `None`, with the
    /// host `slot` holds closed.
    fn hosted(&self, slot: &BusSlot) -> Option<BusSpec> {
        let (fleet, tier) = match (self.cfg.hosting)() {
            BusHosting::On { fleet, tier } => (fleet, tier),
            BusHosting::Unprovisioned => {
                self.close(slot, "the fabric root holds no mint secret");
                return None;
            }
            BusHosting::Off => {
                self.close(slot, "the fabric was turned off");
                return None;
            }
            BusHosting::Hand => {
                self.close(
                    slot,
                    "`aterm fabric on --service none` hands the broker to its person",
                );
                return None;
            }
        };
        Some(BusSpec {
            root: self.cfg.root.clone(),
            fleet,
            tier,
            backups: self.cfg.backups,
        })
    }

    /// One look: hosted as the fabric now says, the host reopened where it must be. The broker
    /// job is retired only before an open (a look at launchd or systemd each time), and a
    /// retirement records the tier the job ran at, so the fabric is read again after one.
    fn pass(&mut self, slot: &BusSlot) {
        let Some(mut spec) = self.hosted(slot) else {
            return;
        };
        if let Some(host) = slot.host() {
            let why = if !host.spec.same(&spec) {
                Some("its fleet or tier changed".to_owned())
            } else if let Some(poisoned) = host.poisoned() {
                Some(format!("its log is poisoned ({poisoned})"))
            } else if host.names_listener() == Some(false) {
                Some(format!("{} no longer names it", host.sock().display()))
            } else {
                None
            };
            let Some(why) = why else { return };
            self.close(slot, &format!("{why}; it reopens"));
        }
        if let Some(retire) = &self.cfg.retire {
            match retire() {
                Ok(RetiredJob::Absent) => {}
                Ok(RetiredJob::Retired { label, tier }) => {
                    aterm_log::info!(
                        "mesh: retired the broker job {label}{}: the node service hosts bus.log",
                        tier.map_or_else(String::new, |tier| format!(
                            " (durability {})",
                            tier.name()
                        ))
                    );
                    let Some(now) = self.hosted(slot) else {
                        return;
                    };
                    spec = now;
                }
                Err(e) => self.say(format!("mesh: the broker job could not be retired: {e}")),
            }
        }
        match BusHost::open(spec) {
            Ok(host) => {
                self.said = None;
                slot.set(Some(Arc::new(host)));
            }
            Err(e) => self.say(format!("mesh: bus.log did not open: {e}")),
        }
    }
}
