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

use crate::relay::{self, Event, Observe, Origin, Published, Relayed};
use astream_broker::{
    AnyClient, Boot, Broker, BrokerHandle, Capability, Escalate, Escalation, StreamLink,
};
use aterm_link::at_rest;
use aterm_link::durability::Durability;
use aterm_net::fleet::link::{Bell, BusLog, Incoming, LinkClock};
use aterm_secrets::membership_log::{Log, NodeStanding};
use aterm_types::MutexExt;
use aterm_uds::backup::Backups;
use aterm_uds::secret::Secret;
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime};

/// The folder under the state dir that holds both logs.
pub const MESH_DIR: &str = "mesh";

/// Each log's guard secret, beside it: 32 random bytes, 0600, never replaced.
pub const GUARD_SECRET: &str = "guard.secret";

/// Each log's local socket, beside it.
pub const SOCKET: &str = "sock";

/// The bytes a socket path may take (`sun_path`, `aterm_link::enable::SUN_PATH_MAX`): a longer
/// one is not served, and the log is reached through the node service alone.
const SUN_PATH_MAX: usize = 104;

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
}

impl Standings {
    /// The standings `log` evaluates: a `Member` key whose seat is current (its node key and
    /// writer key not removed, its recipient not removed) is an origin; a `Removed` or `Retired`
    /// key is revoked; a contested key, and a member's key whose seat is not current, is neither.
    #[must_use]
    pub fn of_log(log: &Log) -> Self {
        let membership = log.membership();
        let mut standings = Self::default();
        for (key, standing) in membership.nodes() {
            match standing {
                NodeStanding::Member(id) => {
                    let current = membership.get(id).is_some_and(|seat| {
                        !membership.member_node_removed(seat)
                            && !log.recipient_is_removed(&seat.binding.recipient())
                    });
                    if current {
                        standings.current.insert(*key);
                    }
                }
                NodeStanding::Removed(_) => {
                    standings.removed.insert(*key);
                }
                NodeStanding::Retired(_) => {
                    standings.retired.insert(*key);
                }
                NodeStanding::Contested | NodeStanding::Unknown => {}
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
        const MAX_STORE_BYTES: u64 = 16 * 1024 * 1024;
        let meta = std::fs::symlink_metadata(file).ok()?;
        if !meta.is_file() || meta.len() > MAX_STORE_BYTES {
            return None;
        }
        let records = decode_records(&std::fs::read(file).ok()?).ok()?;
        let genesis = records.iter().find(|r| r.kind() == Kind::Genesis)?.clone();
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
}

/// This Mac as the mesh knows it: the node key's seed (held scrubbed on drop), its public key,
/// its principal and the producer id its records carry.
pub(crate) struct Me {
    pub(crate) seed: Secret,
    pub(crate) key: [u8; 32],
    pub(crate) principal: String,
    pub(crate) pid: u64,
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
    let path = dir.join(GUARD_SECRET);
    if std::fs::symlink_metadata(&path).is_err() {
        let mut fresh = [0u8; GUARD_LEN];
        aterm_uds::rand::fill(&mut fresh)?;
        let wrote =
            aterm_uds::private_file::write_private_new(dir, GUARD_SECRET, &fresh, |_| Ok(()));
        aterm_uds::secret::scrub(&mut fresh);
        wrote?;
    }
    // A crash inside the write leaves the temporary as the name's second link.
    aterm_uds::atomic_file::sweep_temps_beside(&path, Duration::ZERO)?;
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

    /// A client of this log's broker under this Mac's own key, holding `grants`, each read and
    /// write bounded by `within` (`None`: unbounded).
    pub(crate) fn client(
        &self,
        me: &Me,
        grants: &[String],
        within: Option<Duration>,
    ) -> io::Result<AnyClient> {
        let caps = grants
            .iter()
            .map(|grant| self.mint(grant))
            .collect::<io::Result<Vec<_>>>()?;
        let link = self.adopt(me.key)?;
        link.set_read_timeout(within)?;
        link.set_write_timeout(within)?;
        let (mut client, _closer) = astream_broker::connect_stream(link);
        client.attach_all(caps.iter().map(|cap| (cap.filter.as_str(), &cap.tag[..])))?;
        Ok(client)
    }

    /// The grants a neighbour (`peer`, its principal) is handed at the head of its `Bus` stream:
    /// relaying every subject of the log that travels, and reading its own subjects, so a
    /// rebuilt Mac finds the high water this Mac holds of it. Both are bound to the peer, so only
    /// its own stream can use them.
    pub(crate) fn grants_for(&self, peer: &str) -> io::Result<Vec<Capability>> {
        Ok(vec![
            self.mint(&format!("rw,relay,p={peer}:{}", self.log.filter()))?,
            self.mint(&format!("ro,p={peer}:{}", self.log.own_filter(peer)))?,
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
            *local = Some(self.client(
                me,
                &[format!(
                    "rw,p={}:{}",
                    me.principal,
                    self.log.publish_filter(&me.principal)
                )],
                Some(relay::IO_WITHIN),
            )?);
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
    /// connection and closes the log cleanly; a broker never served closes when it drops.
    fn shut(&self) {
        drop(self.local.lock_or_recover().take());
        drop(self.served.lock_or_recover().take());
    }
}

/// What the revocation pass applied last.
#[derive(Default)]
struct Applied {
    /// The standings, `None` before the first read.
    read: Option<Standings>,
    /// The keys revoked on both brokers.
    revoked: BTreeSet<[u8; 32]>,
    /// The `Removed` keys whose rows were retired.
    retired: BTreeSet<[u8; 32]>,
    /// The `Removed` keys whose rows could not be retired yet, each said once.
    unretired: BTreeSet<[u8; 32]>,
}

/// The mesh of one node service: both logs, this Mac's node key, and the membership log's verdict.
pub struct Mesh {
    pub(crate) me: Me,
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
        let mesh = Arc::new(Self {
            me,
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
        });
        let read = mesh.follow().is_some();
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
            .client(&self.me, &[format!("ro:{filter}")], within)
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

    /// The standings applied, `None` before the first read.
    #[must_use]
    pub fn standings(&self) -> Option<Standings> {
        self.applied.lock_or_recover().read.clone()
    }

    /// Apply `read` to both brokers: revoke each key that entered the revoked set, un-revoke each
    /// that left it, install the current seats' keys (and this Mac's own) as the origins, and
    /// retire the rows of each key newly `Removed` (one whose retire fails is tried again at each
    /// watch, [`Self::retire_due`]). This Mac's own key is never revoked. Answers whether anything
    /// changed.
    pub fn apply(&self, read: Standings) -> bool {
        let mut applied = self.applied.lock_or_recover();
        if applied.read.as_ref() == Some(&read) {
            return false;
        }
        let mut revoked = read.revoked();
        revoked.remove(&self.me.key);
        let entered: Vec<[u8; 32]> = revoked.difference(&applied.revoked).copied().collect();
        let left: Vec<[u8; 32]> = applied.revoked.difference(&revoked).copied().collect();
        let mut origins: BTreeSet<[u8; 32]> = read.current.difference(&revoked).copied().collect();
        origins.insert(self.me.key);
        for hosted in [&self.tables, &self.metrics] {
            for key in &entered {
                hosted.broker.revoke_identity(*key);
            }
            for key in &left {
                // Lifts the key's derived principal too, as `revoke_identity` revoked it.
                hosted.broker.unrevoke_identity(key);
            }
            hosted.broker.set_origin_keys(origins.iter().copied());
        }
        // A key that left `Removed` is retired again should it come back.
        applied.retired.retain(|key| read.removed.contains(key));
        applied.unretired.retain(|key| read.removed.contains(key));
        for key in &entered {
            aterm_log::info!(
                "mesh: {} is revoked: the membership log took it out",
                astream_broker::identity_principal(key)
            );
        }
        for key in &left {
            aterm_log::info!(
                "mesh: {} is un-revoked: the membership log seats it again",
                astream_broker::identity_principal(key)
            );
        }
        let alone = origins.len() == 1;
        applied.revoked = revoked;
        applied.read = Some(read);
        drop(applied);
        self.retire_due();
        if alone {
            // No other seat can hold a record of this Mac: its lease needs no neighbour's word.
            for hosted in [&self.tables, &self.metrics] {
                hosted.origin.lock_or_recover().settle_alone();
            }
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.poke();
        true
    }

    /// Retire the rows of every `Removed` key (never this Mac's own) not retired yet. It runs
    /// outside the lock every stream's admission reads: the keys are revoked already.
    pub(crate) fn retire_due(&self) {
        let due: Vec<[u8; 32]> = {
            let applied = self.applied.lock_or_recover();
            let Some(read) = applied.read.as_ref() else {
                return;
            };
            read.removed
                .iter()
                .filter(|key| **key != self.me.key && !applied.retired.contains(*key))
                .copied()
                .collect()
        };
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
            hosted.client(&self.me, &[format!("rw:{filter}")], Some(relay::IO_WITHIN))?;
        let (rows, _) = client.last_all(&filter)?;
        for (_, subject, _) in rows {
            client.retire_subject(pid, &subject)?;
        }
        Ok(())
    }

    /// Whether `key` may stream to this Mac now: `None` before the standings were read.
    pub(crate) fn admits(&self, key: &[u8; 32]) -> Option<bool> {
        let applied = self.applied.lock_or_recover();
        let read = applied.read.as_ref()?;
        Some(*key != self.me.key && read.is_current(key) && !applied.revoked.contains(key))
    }

    /// The other current seats' keys, `None` before the standings were read.
    pub(crate) fn seats(&self) -> Option<BTreeSet<[u8; 32]>> {
        let applied = self.applied.lock_or_recover();
        let read = applied.read.as_ref()?;
        Some(
            read.current
                .iter()
                .filter(|key| **key != self.me.key && !applied.revoked.contains(*key))
                .copied()
                .collect(),
        )
    }

    /// Serve a `Bus` stream a neighbour opened on its link: refused `unserved` for a log the mesh
    /// does not host, `unread` before the standings were read, `out` for a key that is not a
    /// current seat; otherwise accepted, handed its grants, and adopted by the log's broker under
    /// the peer's key, which refuses a revoked key and ends its stream when the key is revoked.
    pub fn serve_bus(&self, incoming: Incoming) {
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
    /// last relay holding it ends. Detach the overlay ([`crate::Attached`]) first.
    pub fn close(&self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some((bell, thread)) = self.watcher.lock_or_recover().take() {
            bell.ring();
            if thread.thread().id() != std::thread::current().id() {
                let _ = thread.join();
            }
        }
        self.poke();
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
