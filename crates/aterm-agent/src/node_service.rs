// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `systems.alab.aterm-node` background job (`aterm node serve | status | on | off`), kept
//! separate from `systems.alab.aterm-secrets` so a fault in fleet code never stops credentials
//! (`docs/DESIGN-alab-swarm-2026-10-27.md` §3.3, §7, §12.3 C20–C23).

#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

pub use crate::fleet_member::prove_discovery_address;

/// The launchd label and systemd user unit stem for the fleet/swarm node job.
pub const NODE_LABEL: &str = "systems.alab.aterm-node";

/// Systemd unit `Description` for [`NODE_LABEL`].
pub const NODE_DESCRIPTION: &str = "aterm fleet and swarm node job (aterm node serve)";

/// Exclusive advisory lock held by a running `aterm node serve` in `<state_dir>`.
pub const NODE_JOB_LOCK: &str = "node-job.lock";

/// Whether this Mac left its fleet and holds no fleet's key since: `aterm fleet leave` wrote
/// `fleet-off` and removed the key. A `fleet-off` beside a key (a fleet formed after a leave, which
/// keeps the record) is a member's.
#[must_use]
pub fn left_the_fleet(state_dir: &Path) -> bool {
    std::fs::symlink_metadata(state_dir.join(aterm_net::fleet_member::OFF_FILE)).is_ok()
        && aterm_net::fleet::read_key_file(state_dir).is_err()
}

/// Exclusive advisory lock a running `aterm node serve` holds in `<state_dir>` only while it serves
/// the fleet port or is about to bind it ([`node_port_holds_lock`]).
pub const NODE_PORT_LOCK: &str = "node-port.lock";

/// Marker written by `aterm node off` in `<state_dir>` so `sync.node` leaves the job off.
pub const NODE_OFF_FILE: &str = "node-off";

/// Serializes automatic node repair with explicit on/off changes across processes.
#[cfg(unix)]
const NODE_ADMIN_LOCK: &str = "node-admin.lock";

#[cfg(all(test, unix))]
thread_local! {
    static NODE_ADMIN_WAITER: std::cell::RefCell<Option<std::sync::mpsc::Sender<()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(unix)]
fn node_admin_lock(state_dir: &Path) -> Result<aterm_uds::private_file::PrivateLock, String> {
    aterm_uds::private_file::ensure_private_dir(state_dir).map_err(|error| error.to_string())?;
    aterm_uds::private_file::PrivateLock::take_with(
        state_dir,
        NODE_ADMIN_LOCK,
        Duration::from_secs(120),
        "another node service change is in progress",
        || {
            #[cfg(test)]
            NODE_ADMIN_WAITER.with(|waiter| {
                if let Some(waiter) = waiter.borrow().as_ref() {
                    let _ = waiter.send(());
                }
            });
        },
    )
    .map_err(|error| error.to_string())
}

/// Bonjour service type advertised by `aterm node serve` on every network (§3.3, C23; F17).
pub const ALAB_TCP_SERVICE: &str = "_alab._tcp";

/// Discovery policy captured once when this process starts its services.
pub struct DiscoveryPolicy {
    /// Whether the node job owns discovery for the whole Mac.
    pub mesh: bool,
}

impl DiscoveryPolicy {
    /// This process's release policy. Tests can construct either policy directly.
    #[must_use]
    pub fn process_discovery() -> &'static Self {
        static POLICY: std::sync::OnceLock<DiscoveryPolicy> = std::sync::OnceLock::new();
        POLICY.get_or_init(|| Self {
            mesh: aterm_types::mesh_switch::MESH,
        })
    }
}

/// The node job's single browse, with an address-only cache keyed by proven node id.
/// The job lock is held before this service starts; windows and CLI processes never start it.
pub struct MeshDiscovery {
    /// Captured release policy, independently configurable in tests.
    pub mesh: bool,
    watch: Option<std::sync::Arc<aterm_secrets::discover::Watch>>,
    bell: std::sync::Arc<aterm_secrets::wake::Bell>,
    addresses: std::collections::BTreeMap<String, std::net::SocketAddr>,
    next: Option<Instant>,
    dirty: bool,
}

/// Address hints are refreshed once per period, with the clock supplied by the caller.
pub const DISCOVERY_PERIOD: Duration = Duration::from_secs(30);
const ADDRESS_CACHE: &str = "mesh-addresses";

#[cfg(test)]
#[path = "node_service_mesh_discovery_tests.rs"]
mod mesh_discovery_tests;

impl MeshDiscovery {
    /// Make an unstarted service. Only the node job calls [`Self::start`].
    #[must_use]
    pub fn new(mesh: bool) -> Self {
        Self {
            mesh,
            watch: None,
            bell: std::sync::Arc::new(aterm_secrets::wake::Bell::default()),
            addresses: std::collections::BTreeMap::new(),
            next: None,
            dirty: false,
        }
    }

    /// Keep last-known addresses across a node restart, including sleeping members' hints.
    /// These are authenticated again when dialled, like newly resolved mDNS hints.
    pub fn restore(&mut self, state: &Path) {
        if self.mesh {
            self.addresses.extend(mesh_addresses(state));
        }
    }

    /// Start at most one browse for this service.
    pub fn start(&mut self, own: &str) -> Option<std::sync::Arc<aterm_secrets::discover::Watch>> {
        if cfg!(test)
            || std::env::var_os(aterm_types::world::MARKER).is_some()
            || std::env::var_os("CI").is_some()
        {
            return None;
        }
        self.start_with(own, aterm_secrets::discover::Watch::start_for)
    }

    fn start_with(
        &mut self,
        own: &str,
        start: impl FnOnce(
            &'static str,
            &std::sync::Arc<aterm_secrets::wake::Bell>,
            &[&str],
        ) -> aterm_secrets::discover::Watch,
    ) -> Option<std::sync::Arc<aterm_secrets::discover::Watch>> {
        if !self.mesh {
            return None;
        }
        Some(std::sync::Arc::clone(self.watch.get_or_insert_with(|| {
            std::sync::Arc::new(start(ALAB_TCP_SERVICE, &self.bell, &[own]))
        })))
    }

    /// Resolve fresh hints and replace an address only when `prove` authenticates its node.
    /// `now` is injected so address-change laws need no wall-clock waits.
    pub fn refresh(
        &mut self,
        now: Instant,
        hints: impl FnOnce() -> Vec<aterm_secrets::discover::Advert>,
        mut prove: impl FnMut(
            &aterm_secrets::discover::Advert,
        ) -> Option<(String, std::net::SocketAddr)>,
    ) -> bool {
        if !self.mesh || self.next.is_some_and(|next| now < next) {
            return false;
        }
        self.next = Some(now + DISCOVERY_PERIOD);
        let mut changed = false;
        for hint in hints() {
            if let Some((node, addr)) = prove(&hint) {
                changed |= self.addresses.insert(node, addr) != Some(addr);
            }
        }
        self.dirty |= changed;
        changed
    }

    /// Save only addresses proven by the node job, for CLI processes that run no browse.
    /// # Errors
    /// The private address file could not be written.
    pub fn save(&mut self, state: &Path) -> std::io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let mut text = String::new();
        for (node, addr) in &self.addresses {
            text.push_str(&format!("{node} {addr}\n"));
        }
        aterm_uds::private_file::write_private(state, ADDRESS_CACHE, text.as_bytes())?;
        self.dirty = false;
        Ok(())
    }
}

/// Read the node job's saved addresses. They remain dial hints: every use authenticates again.
#[must_use]
pub fn mesh_addresses(state: &Path) -> Vec<(String, std::net::SocketAddr)> {
    let Ok(bytes) = aterm_uds::private_file::read_private(state, ADDRESS_CACHE, 64 * 1024) else {
        return Vec::new();
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let (node, addr) = line.split_once(' ')?;
            aterm_net::fleet::is_node_id(node)
                .then(|| Some((node.to_owned(), addr.parse().ok()?)))?
        })
        .collect()
}

/// Maximum age (24 h) of this Mac's last ALab vault answer before `_alab._tcp` stops advertising.
pub const ALAB_MEMBER_TTL_SECS: u64 = 24 * 60 * 60;

/// Path to `dns-sd` on macOS, for a lab guest's member advert (`aterm-lab`'s `guest::member`):
/// aterm's own adverts are LAN broker registrations, and it runs no `dns-sd`.
pub const DNS_SD: &str = aterm_uds::os_path!("dns-sd");

/// Path to `launchctl` on macOS.
pub const LAUNCHCTL: &str = aterm_uds::os_path!("launchctl");

/// Candidate `systemctl` paths on Linux (never a `PATH` lookup).
pub const SYSTEMCTL_PATHS: [&str; 3] = [
    "/usr/bin/systemctl",
    "/bin/systemctl",
    "/run/current-system/sw/bin/systemctl",
];

/// LaunchAgent plist path: `<home>/Library/LaunchAgents/systems.alab.aterm-node.plist`.
#[must_use]
pub fn plist_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{NODE_LABEL}.plist"))
}

/// Systemd user unit path: `<home>/.config/systemd/user/systems.alab.aterm-node.service`.
#[must_use]
pub fn unit_path(home: &Path) -> PathBuf {
    home.join(".config")
        .join("systemd")
        .join("user")
        .join(format!("{NODE_LABEL}.service"))
}

/// Log path for launchd stdout/stderr: `<state_dir>/systems.alab.aterm-node.log`.
#[must_use]
pub fn log_path(state_dir: &Path) -> PathBuf {
    state_dir.join(format!("{NODE_LABEL}.log"))
}

/// Off-marker path in `<state_dir>`: `<state_dir>/node-off`.
#[must_use]
pub fn off_path(state_dir: &Path) -> PathBuf {
    state_dir.join(NODE_OFF_FILE)
}

/// Whether the person turned `systems.alab.aterm-node` off (`aterm node off`).
#[must_use]
pub fn node_turned_off(state_dir: &Path) -> bool {
    std::fs::symlink_metadata(off_path(state_dir)).is_ok()
}

/// Exclusive advisory lock held in `<state_dir>/node-job.lock` for the lifetime of
/// `aterm node serve` (one job at a time), and the same kind of lock in `node-port.lock` while it
/// serves the fleet port: interactive windows test [`node_port_holds_lock`] and refrain from
/// binding port 7100 while the node job serves it (C20).
#[cfg(unix)]
pub struct NodeJobLock {
    file: File,
}

#[cfg(unix)]
impl NodeJobLock {
    /// Try to acquire `<state_dir>/node-job.lock` without blocking. Returns `Ok(None)` when
    /// another `aterm node serve` process already holds the lock.
    ///
    /// # Errors
    /// Returns an error if `state_dir` cannot be made private or the lock file cannot be opened.
    pub fn try_acquire(state_dir: &Path) -> std::io::Result<Option<Self>> {
        Self::try_acquire_at(state_dir, NODE_JOB_LOCK)
    }

    /// Try to acquire `<state_dir>/node-port.lock` ([`NODE_PORT_LOCK`]) without blocking: the
    /// node job holds it only while it serves the fleet port or is about to bind it, so a window
    /// yields the port to a job that serves it and never to one that idles. `Ok(None)` while
    /// another process holds it.
    ///
    /// # Errors
    /// As [`Self::try_acquire`].
    pub fn try_acquire_port(state_dir: &Path) -> std::io::Result<Option<Self>> {
        Self::try_acquire_at(state_dir, NODE_PORT_LOCK)
    }

    fn try_acquire_at(state_dir: &Path, name: &str) -> std::io::Result<Option<Self>> {
        aterm_uds::private_file::ensure_private_dir(state_dir)?;
        let file =
            aterm_uds::kept_out(|| aterm_uds::private_file::open_private_rw(state_dir, name))
                .map_err(|e| std::io::Error::other(e.to_string()))?;
        for attempt in 0..32 {
            #[expect(
                clippy::disallowed_methods,
                reason = "node-job.lock and node-port.lock are CLOEXEC/CLOFORK and NodeJobLock::drop releases by LOCK_UN"
            )]
            match file.try_lock() {
                Ok(()) => return Ok(Some(Self { file })),
                Err(std::fs::TryLockError::WouldBlock) => {
                    // Distinguish another exclusive `NodeJobLock` (`LOCK_EX`) from a transient
                    // shared `node_job_holds_lock` probe (`LOCK_SH`): if `try_lock_shared` also
                    // returns `WouldBlock`, an exclusive lock is held so we return `Ok(None)`
                    // immediately; if `try_lock_shared` succeeds, only a shared probe was in
                    // flight so we unlock and retry.
                    #[expect(
                        clippy::disallowed_methods,
                        reason = "probe lock is CLOEXEC/CLOFORK and released immediately by LOCK_UN"
                    )]
                    match file.try_lock_shared() {
                        Ok(()) => {
                            let _ = file.unlock();
                            if attempt < 4 {
                                #[expect(
                                    clippy::disallowed_methods,
                                    reason = "a counted retry past another process's momentary shared probe, which no wake crosses: no deadline waits on the node job's take, and a longer pause only gives the probe longer to let go"
                                )]
                                std::thread::yield_now();
                            } else {
                                std::thread::sleep(Duration::from_millis(1));
                            }
                        }
                        Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
                        Err(std::fs::TryLockError::Error(err)) => return Err(err),
                    }
                }
                Err(std::fs::TryLockError::Error(err)) => return Err(err),
            }
        }
        Ok(None)
    }
}

#[cfg(unix)]
impl Drop for NodeJobLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(unix)]
struct NodeJobProbe(File);

#[cfg(unix)]
impl Drop for NodeJobProbe {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Whether a local `aterm node serve` process currently holds `<state_dir>/node-job.lock`.
#[cfg(unix)]
#[must_use]
pub fn node_job_holds_lock(state_dir: &Path) -> bool {
    holds_lock_at(state_dir, NODE_JOB_LOCK)
}

/// Whether a local `aterm node serve` serves the fleet port now, or is about to bind it: it holds
/// `<state_dir>/node-port.lock` ([`NodeJobLock::try_acquire_port`]). A window yields the port only
/// while this holds.
#[cfg(unix)]
#[must_use]
pub fn node_port_holds_lock(state_dir: &Path) -> bool {
    holds_lock_at(state_dir, NODE_PORT_LOCK)
}

#[cfg(unix)]
fn holds_lock_at(state_dir: &Path, name: &str) -> bool {
    let Ok((file, _)) =
        aterm_uds::kept_out(|| aterm_uds::private_file::open_private(state_dir, name))
    else {
        return false;
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "shared probe lock is CLOEXEC/CLOFORK and NodeJobProbe::drop releases immediately by LOCK_UN"
    )]
    match file.try_lock_shared() {
        Ok(()) => {
            drop(NodeJobProbe(file));
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(_)) => false,
    }
}

/// Role advertised in `_alab._tcp` TXT (`role=member|steward`, §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    /// Ordinary swarm member.
    Member,
    /// Steward holding the vault's admin locker.
    Steward,
}

impl NodeRole {
    /// Wire token for `role=`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Steward => "steward",
        }
    }
}

/// Exact `_alab._tcp` TXT payload (`v=1`, `role=member|steward`, `fleet=<port>`, `window=0|1`;
/// nothing else — no label, node id or version, per §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlabTcpTxt {
    /// Whether this node holds the steward admin locker.
    pub role: NodeRole,
    /// Fleet TCP port (`7100` by default).
    pub fleet_port: u16,
    /// Whether at least one interactive `aterm` window is open for this login.
    pub window: bool,
}

impl AlabTcpTxt {
    /// Four TXT key-value pairs in canonical order (`§3.3`).
    #[must_use]
    pub fn txt_pairs(self) -> [String; 4] {
        [
            "v=1".to_owned(),
            format!("role={}", self.role.word()),
            format!("fleet={}", self.fleet_port),
            format!("window={}", u8::from(self.window)),
        ]
    }
}

/// The `_alab._tcp` advert on `local.` with [`AlabTcpTxt`], which the node holds through the LAN
/// broker ([`AlabTcpAdvertiser`]). `instance` must be an ephemeral opaque token (never a label,
/// hostname, node id or version).
#[must_use]
pub fn alab_tcp_advert(instance: &str, txt: AlabTcpTxt) -> aterm_uds::lan::Advert {
    advert(instance, txt.fleet_port, txt.txt_pairs().to_vec())
}

/// The mesh advert carries a protocol revision and untrusted identity/address hints only.
#[must_use]
pub fn mesh_advert(instance: &str, node: &str, port: u16) -> aterm_uds::lan::Advert {
    advert(
        instance,
        port,
        vec!["v=2".into(), format!("id={node}"), format!("port={port}")],
    )
}

/// `instance` of `_alab._tcp` on `local.` at `port`, its TXT record `txt`.
fn advert(instance: &str, port: u16, txt: Vec<String>) -> aterm_uds::lan::Advert {
    aterm_uds::lan::Advert {
        name: instance.to_owned(),
        service: ALAB_TCP_SERVICE.to_owned(),
        domain: "local.".to_owned(),
        port,
        txt: txt.into_iter().map(String::into_bytes).collect(),
        ..aterm_uds::lan::Advert::default()
    }
}

/// The `dns-sd -R` arguments registering [`alab_tcp_advert`]'s `_alab._tcp` on `local`: the
/// form a lab guest's own `dns-sd` advertises a member by; the node itself holds its advert
/// through the LAN broker.
#[must_use]
pub fn alab_tcp_dns_sd_args(instance: &str, txt: AlabTcpTxt) -> Vec<String> {
    let mut args = vec![
        "-R".to_owned(),
        instance.to_owned(),
        ALAB_TCP_SERVICE.to_owned(),
        "local".to_owned(),
        txt.fleet_port.to_string(),
    ];
    args.extend(txt.txt_pairs());
    args
}

/// Generate a fresh random opaque instance token (`alab-<8 hex>`) carrying no machine identity.
#[must_use]
pub fn random_alab_instance_name() -> String {
    let mut bytes = [0u8; 4];
    if aterm_uds::rand::fill(&mut bytes).is_err() {
        let fallback = u32::try_from(aterm_time::unix_millis().unwrap_or(0) & 0xffff_ffff)
            .unwrap_or(0)
            ^ std::process::id();
        bytes = fallback.to_le_bytes();
    }
    format!("alab-{}", aterm_codec::hex::encode_lower(&bytes))
}

/// Current route-source tokens (`route_sources()`), empty when offline.
#[must_use]
pub fn current_route_tokens() -> Vec<String> {
    aterm_secrets::server::route_sources()
        .into_iter()
        .flatten()
        .map(|ip| ip.to_string())
        .collect()
}

/// How long a route this Mac was read to have stands for the node's advert and its LAN responder
/// while the kernel names no change of network ([`RouteStanding`]).
const ROUTE_STANDS: Duration = Duration::from_secs(10);

/// How long after a change of network was heard no read of the routes is kept
/// ([`RouteStanding`]): the kernel names a change of address or link, and nothing shows the
/// routes have followed it by then, so a route read that soon can be the one the change takes
/// away. For this long the node reads at every ask, as it did before a route stood at all.
const ROUTE_SETTLES: Duration = Duration::from_secs(1);

/// Whether this Mac has a route, as the node's advert and its LAN responder ask many times a
/// minute ([`AlabTcpAdvertiser::tick`], [`lan_responder_allowed`]). A read of the routes
/// ([`current_route_tokens`]) asks the kernel's routing table, so the two share one answer
/// rather than each read at every ask: a route it read stands until the kernel names a change of
/// network (`changes`, asked without waiting before every answer) or [`ROUTE_STANDS`] passes,
/// whichever is first, since nothing shows that every change of route alone is named there. That
/// it has no route is never kept, so a Mac that comes online is seen at the next ask. Nor is a
/// read made at the ask that hears a change or less than [`ROUTE_SETTLES`] after it, which can
/// come before the routes have followed the change: every ask in that time reads. With no
/// change socket nothing is kept at all.
struct RouteStanding {
    /// The kernel's network-change events from before the read `seen` dates. `None` where it
    /// could not be opened or has failed since.
    #[cfg(unix)]
    changes: Option<aterm_uds::network::RouteChanges>,
    /// The latest ask that heard a change of network, by its own clock.
    changed: Option<Instant>,
    /// When a read made [`ROUTE_SETTLES`] or more after `changed` last found a route, with no
    /// change heard since.
    seen: Option<Instant>,
}

impl RouteStanding {
    /// A standing that hears the kernel's network changes from now on, and has read nothing.
    fn open() -> Self {
        Self {
            #[cfg(unix)]
            changes: aterm_uds::network::RouteChanges::open().ok(),
            changed: None,
            seen: None,
        }
    }

    /// Whether a change of network came since the last ask; `None` with no socket to tell, which
    /// a socket that fails here is from now on.
    fn change_heard(&mut self) -> Option<bool> {
        #[cfg(unix)]
        {
            let heard = self.changes.as_ref()?.changed_now();
            if heard.is_none() {
                self.changes = None;
            }
            heard
        }
        #[cfg(not(unix))]
        None
    }

    /// Whether this Mac has a route at `now`, by `read` only where no read of the last
    /// [`ROUTE_STANDS`] found one with no change of network heard since. A read made at the ask
    /// that heard a change, or less than [`ROUTE_SETTLES`] after it, answers its own ask alone:
    /// it can come before the routes have followed the change, so it is not kept.
    fn has_route(&mut self, now: Instant, read: impl FnOnce() -> bool) -> bool {
        match self.change_heard() {
            Some(false) => {}
            Some(true) => {
                self.seen = None;
                self.changed = self.changed.max(Some(now));
            }
            None => {
                self.seen = None;
                return read();
            }
        }
        if self
            .seen
            .and_then(|seen| now.checked_duration_since(seen))
            .is_some_and(|age| age < ROUTE_STANDS)
        {
            return true;
        }
        let routed = read();
        let settled = self.changed.is_none_or(|changed| {
            now.checked_duration_since(changed)
                .is_some_and(|since| since >= ROUTE_SETTLES)
        });
        self.seen = (routed && settled).then_some(now);
        routed
    }
}

/// One [`RouteStanding`] for an advertiser and the responder it starts, opened at its first ask:
/// a Mac that is no member asks nothing of its routes and opens no socket.
type SharedRouteStanding = Arc<LazyLock<Mutex<RouteStanding>>>;

/// Whether this Mac has a route now, by `standing` ([`RouteStanding::has_route`] over
/// [`current_route_tokens`]).
fn has_route_now(standing: &SharedRouteStanding) -> bool {
    standing
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .has_route(Instant::now(), || !current_route_tokens().is_empty())
}

/// Whether `_alab._tcp` is advertised (§3.3, C23; on every network since F17,
/// `docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1): `[swarm] enabled` is not `false` (`enabled`),
/// this Mac has a route (`routed`), and it is an ALab vault member whose vault answered within
/// [`ALAB_MEMBER_TTL_SECS`] before `now_secs` (`heard`, unix seconds). A clock that could not be
/// read (`now_secs` `None`) or an answer dated after it (the clock stepped back since) gives no
/// age, so nothing is advertised until the vault answers again. No network is special: a café's
/// routes advertise as an office's do, since the advert carries no identity and a dial of it is
/// judged by its handshake's proof.
#[must_use]
pub fn advertises(enabled: bool, routed: bool, heard: Option<u64>, now_secs: Option<u64>) -> bool {
    enabled
        && routed
        && heard
            .zip(now_secs)
            .and_then(|(heard, now)| now.checked_sub(heard))
            .is_some_and(|age| age <= ALAB_MEMBER_TTL_SECS)
}

/// [`advertises`] as `aterm_config`'s `[swarm]` and the vault at `vault_paths` stand at
/// `now_secs`, on a Mac that has a route when `routed` says so: the vault's word that this Mac
/// is an ALab member no evidence denies,
/// and when it last heard it, is read again only when one of the vault's files it was read from
/// moved ([`aterm_secrets::client::swarm_member_heard_now`]), and judged against `now_secs` at
/// every call. A read that judged nothing (a vault writer mid-apply, a file it cannot read) is no
/// member's. `routed` is asked last, and only where every other term holds, so a Mac with the
/// swarm off, or no member's, asks nothing of its routes.
#[must_use]
pub fn should_advertise_alab_tcp_with(
    vault_paths: &aterm_secrets::client::Paths,
    aterm_config: Option<&Path>,
    routed: impl FnOnce() -> bool,
    now_secs: Option<u64>,
) -> bool {
    let enabled = aterm_config
        .is_none_or(|cfg_path| aterm_secrets::swarm::enabled_at(cfg_path).unwrap_or(false));
    let heard = aterm_secrets::client::swarm_member_heard_now(vault_paths)
        .ok()
        .flatten();
    advertises(enabled, true, heard, now_secs) && routed()
}

/// Whether this Mac's LAN responder answers now ([`crate::lan_members::Responder`]), which asks at
/// each look at its sources and before each answer it sends: the node `state` holds is still the one
/// keyed `key`, neither turned off ([`node_turned_off`]) nor gone from its fleet
/// ([`left_the_fleet`]), each read afresh, and this Mac advertises at `now_secs` where `routed`
/// says it has a route
/// ([`should_advertise_alab_tcp_with`] of the vault and `aterm.toml` in `vault`), so a removal, a
/// turn-off or a day unheard ends its answers at the next ask. `routed` is asked last, and only
/// where every other term holds: a Mac that left its fleet asks nothing of its routes.
#[cfg(unix)]
fn lan_responder_allowed(
    state: Option<&Path>,
    key: aterm_net::fleet::NodeKey,
    (vault_paths, aterm_config): (&aterm_secrets::client::Paths, Option<&Path>),
    routed: impl FnOnce() -> bool,
    now_secs: Option<u64>,
) -> bool {
    state.is_some_and(|state| {
        !left_the_fleet(state)
            && !node_turned_off(state)
            && aterm_net::fleet_member::read_node_identity(state)
                .ok()
                .flatten()
                .is_some_and(|node| node.key() == key)
    }) && should_advertise_alab_tcp_with(vault_paths, aterm_config, routed, now_secs)
}

/// The role `_alab._tcp`'s TXT names ([`AlabTcpTxt`]): steward on a vault admin, which opens the
/// vault's admin custody to tell. Only the advert that carries it asks.
fn alab_tcp_role(vault_paths: &aterm_secrets::client::Paths) -> NodeRole {
    if aterm_secrets::client::admin_cached(vault_paths).is_ok() {
        NodeRole::Steward
    } else {
        NodeRole::Member
    }
}

/// The listener's direct LAN responder and one `_alab._tcp` advert, a
/// [`aterm_uds::lan::Registration`] the root LAN
/// broker holds for as long as this does (so macOS asks nobody whether aterm may use the local
/// network), withdrawn on drop, whenever this Mac stops advertising ([`advertises`]), and by the
/// broker itself if this process dies.
pub struct AlabTcpAdvertiser {
    instance: String,
    mesh: bool,
    state: Option<PathBuf>,
    /// Whether this Mac has a route, for [`Self::tick`] and the responder it starts.
    routes: SharedRouteStanding,
    inner: Mutex<AdvertiserInner>,
}

/// A listener generation's permission to announce a newly usable automatic desk.
/// Closing it makes retained callbacks inert before that generation releases its listener.
pub struct AdmissionAnnouncer {
    active: Arc<AtomicBool>,
    notify: Arc<dyn Fn() + Send + Sync>,
}

impl AdmissionAnnouncer {
    /// The admission worker calls this only after publishing usable current policy.
    #[must_use]
    pub fn notifier(&self) -> Arc<dyn Fn() + Send + Sync> {
        Arc::clone(&self.notify)
    }

    /// End this generation before stopping its responder or joining its admission worker.
    pub fn close(&self) {
        self.active.store(false, Ordering::Release);
    }
}

impl Drop for AdmissionAnnouncer {
    fn drop(&mut self) {
        self.close();
    }
}

struct AdvertiserInner {
    held: Option<(aterm_uds::lan::Registration, aterm_uds::lan::Advert)>,
    #[cfg(unix)]
    lan: Option<crate::lan_members::Responder>,
}

impl Default for AlabTcpAdvertiser {
    fn default() -> Self {
        Self::new(random_alab_instance_name())
    }
}

impl AlabTcpAdvertiser {
    /// Keep the fixture's real loopback responder under the ordinary advertiser owner.
    #[cfg(test)]
    pub(crate) fn holding_responder_for_test(
        mut self,
        responder: crate::lan_members::Responder,
    ) -> Self {
        self.inner
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .lan = Some(responder);
        self
    }

    /// Own a capability wake for exactly this listener generation. Neither the callback nor
    /// its queued hint keeps an advertiser or a closed generation alive.
    #[must_use]
    pub fn admission_announcer(self: &Arc<Self>, port: u16) -> AdmissionAnnouncer {
        let active = Arc::new(AtomicBool::new(true));
        let live = Arc::downgrade(&active);
        let advertiser = Arc::downgrade(self);
        let notify = Arc::new(move || {
            let Some(active) = live
                .upgrade()
                .filter(|active| active.load(Ordering::Acquire))
            else {
                return;
            };
            let Some(advertiser) = advertiser.upgrade() else {
                return;
            };
            let guard = advertiser
                .inner
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            #[cfg(unix)]
            if active.load(Ordering::Acquire)
                && let Some(lan) = guard.lan.as_ref().filter(|lan| lan.port == port)
            {
                lan.announce_ready(Arc::downgrade(&active));
            }
            #[cfg(not(unix))]
            let _ = (active, guard, port);
        });
        AdmissionAnnouncer { active, notify }
    }

    /// Create a tender using `instance` as the opaque Bonjour service instance name.
    #[must_use]
    pub fn new(instance: String) -> Self {
        Self {
            instance,
            mesh: DiscoveryPolicy::process_discovery().mesh,
            state: aterm_types::dirs::state_dir(),
            routes: Arc::new(LazyLock::new(|| Mutex::new(RouteStanding::open()))),
            inner: Mutex::new(AdvertiserInner {
                held: None,
                #[cfg(unix)]
                lan: None,
            }),
        }
    }

    /// Bind the advert's identity to the node service's configured state directory.
    #[must_use]
    pub fn at_state(instance: String, state: &Path) -> Self {
        let mut advertiser = Self::new(instance);
        advertiser.state = Some(state.to_owned());
        advertiser
    }

    /// The opaque Bonjour instance name it registers, which this process's own browse holds as
    /// its own.
    #[must_use]
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// Reconcile the held advert with current ALab-network eligibility and [`AlabTcpTxt`]. A
    /// registration the broker could not make (none installed, say) is tried again next tick.
    pub fn tick(
        &self,
        vault_paths: &aterm_secrets::client::Paths,
        aterm_config: Option<&Path>,
        fleet_port: u16,
        window_open: bool,
    ) {
        let now_secs = aterm_time::unix_secs();
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let eligible = should_advertise_alab_tcp_with(
            vault_paths,
            aterm_config,
            || has_route_now(&self.routes),
            now_secs,
        );
        let identity = (cfg!(unix) || cfg!(target_os = "macos"))
            .then_some(eligible)
            .filter(|&eligible| eligible)
            .and(self.state.as_ref())
            .and_then(|state| {
                aterm_net::fleet_member::read_node_identity(state)
                    .ok()
                    .flatten()
            });
        #[cfg(unix)]
        {
            let wanted = identity.as_ref().map(|node| (fleet_port, node.key()));
            if guard.lan.as_ref().map(|lan| (lan.port, lan.key)) != wanted
                || guard
                    .lan
                    .as_ref()
                    .is_some_and(crate::lan_members::Responder::finished)
            {
                guard.lan = None;
                if let Some((port, key)) = wanted
                    && !cfg!(test)
                    && std::env::var_os(aterm_types::world::MARKER).is_none()
                    && std::env::var_os("CI").is_none()
                    && std::env::var_os(aterm_secrets::discover::LAB_MEMBER_SERVICE_ENV).is_none()
                {
                    let paths = vault_paths.clone();
                    let config = aterm_config.map(Path::to_owned);
                    let state = self.state.clone();
                    let routes = Arc::clone(&self.routes);
                    guard.lan = crate::lan_members::Responder::start(port, key, move || {
                        lan_responder_allowed(
                            state.as_deref(),
                            key,
                            (&paths, config.as_deref()),
                            || has_route_now(&routes),
                            aterm_time::unix_secs(),
                        )
                    })
                    .ok();
                }
            }
        }
        if !cfg!(target_os = "macos") {
            return;
        }
        let wanted = eligible
            .then(|| {
                if self.mesh {
                    let node = identity.as_ref()?;
                    Some(mesh_advert(&self.instance, node.node(), fleet_port))
                } else {
                    Some(alab_tcp_advert(
                        &self.instance,
                        AlabTcpTxt {
                            role: alab_tcp_role(vault_paths),
                            fleet_port,
                            window: window_open,
                        },
                    ))
                }
            })
            .flatten();
        reconcile_advert(&mut guard.held, wanted, &aterm_uds::lan::register);
    }

    /// Stop the LAN responder and withdraw the held advert.
    pub fn stop(&self) {
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        #[cfg(unix)]
        {
            guard.lan = None;
        }
        guard.held.take();
    }
}

impl Drop for AlabTcpAdvertiser {
    fn drop(&mut self) {
        self.stop();
    }
}

/// An advert held while it lasts ([`reconcile_advert`]).
trait HeldAdvert {
    /// Whether the broker ended it (its connection closed, or it reported an error).
    fn ended(&mut self) -> bool;
}

/// How long [`HeldAdvert::ended`] waits for the rest of a frame `poll(2)` already saw begin: a
/// hang detector on a broker that stalls mid-frame, never a wait for news.
const ADVERT_FRAME_HANG: Duration = Duration::from_secs(1);

impl HeldAdvert for aterm_uds::lan::Registration {
    /// Asks the broker's connection once without waiting: a connection with nothing to read is
    /// live; whatever it holds is read (an event, such as a rename, keeps the advert), and its
    /// end of stream or an error means the broker ended the advert. `Events::next_within` alone
    /// cannot tell: an already passed deadline returns before it polls.
    fn ended(&mut self) -> bool {
        let events = self.events();
        loop {
            if !events.buffered() {
                let watch = aterm_uds::ready::Watch::new(
                    std::os::fd::AsFd::as_fd(&*events),
                    aterm_uds::ready::Interest::Read,
                );
                match aterm_uds::ready::any_interests_now(&[watch]) {
                    Ok(ready) if ready.is_empty() => return false,
                    Ok(_) => {}
                    Err(_) => return true,
                }
            }
            match events.next_within(ADVERT_FRAME_HANG) {
                Ok(Some(_)) => {}
                Ok(None) => return false,
                Err(_) => return true,
            }
        }
    }
}

/// Hold `wanted` in `held`: an advert already held as wanted and not ended stays; any other is
/// dropped (withdrawn), and `wanted` is registered in its place. A registration that fails
/// leaves nothing held.
fn reconcile_advert<T: HeldAdvert>(
    held: &mut Option<(T, aterm_uds::lan::Advert)>,
    wanted: Option<aterm_uds::lan::Advert>,
    register: &dyn Fn(&aterm_uds::lan::Advert) -> Result<T, aterm_uds::lan::LanError>,
) {
    if let (Some((registration, had)), Some(advert)) = (held.as_mut(), wanted.as_ref())
        && had == advert
        && !registration.ended()
    {
        return;
    }
    *held = None;
    let Some(advert) = wanted else {
        return;
    };
    if let Ok(registration) = register(&advert) {
        *held = Some((registration, advert));
    }
}

/// Render the macOS `LaunchAgent` plist for `systems.alab.aterm-node`, putting `atpkg`'s managed
/// `pkg/bin` first on `PATH` (C8, C20). Its `ProcessType` is `Interactive`, which clamps none of
/// the job's threads: `Background` held every thread and child at priority 4, which a loaded Mac
/// starves for minutes (`docs/measured/fleet-lan-m100-2026-11-02.md`), and `Standard` clamps them
/// all at utility, where no lift reaches (the fleet book's lock holder kept the lock at utility).
/// The job holds itself at utility thread by thread instead (`aterm node serve`,
/// `aterm_gui::hold_process_as_background_work`), so its routine work runs where `Standard` ran
/// it. A plist an earlier build wrote is rewritten by the next `sync.node` pass
/// ([`ensure_for_sync_with`]).
#[must_use]
pub fn node_plist_text(
    program: &Path,
    home: &Path,
    state_dir: &Path,
    bundle_id: Option<&str>,
) -> String {
    let xml = |s: &str| format!("<string>{}</string>", aterm_codec::xml::escape(s));
    let log = log_path(state_dir);
    let pkg_bin = home.join("Library/Application Support/aterm/pkg/bin");
    let path_env = format!(
        "{}:/usr/bin:/bin:/usr/sbin:/sbin",
        pkg_bin.to_string_lossy()
    );
    let associated = bundle_id.map_or_else(String::new, |id| {
        format!(
            "\t<key>AssociatedBundleIdentifiers</key>\n\t<array>\n\t\t{}\n\t</array>\n",
            xml(id)
        )
    });
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t{}\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         \t\t{}\n\
         \t\t<string>node</string>\n\
         \t\t<string>serve</string>\n\
         \t</array>\n\
         \t<key>EnvironmentVariables</key>\n\
         \t<dict>\n\
         \t\t<key>HOME</key>\n\
         \t\t{}\n\
         \t\t<key>PATH</key>\n\
         \t\t{}\n\
         \t</dict>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         \t<key>KeepAlive</key>\n\
         \t<true/>\n\
         \t<key>ProcessType</key>\n\
         \t<string>Interactive</string>\n\
         {associated}\
         \t<key>StandardOutPath</key>\n\
         \t{}\n\
         \t<key>StandardErrorPath</key>\n\
         \t{}\n\
         </dict>\n\
         </plist>\n",
        xml(NODE_LABEL),
        xml(&program.to_string_lossy()),
        xml(&home.to_string_lossy()),
        xml(&path_env),
        xml(&log.to_string_lossy()),
        xml(&log.to_string_lossy()),
    )
}

/// Writes `val` into a quoted systemd word: `\\` and `\"` for the quoting, `%%` for a
/// specifier and, in an `ExecStart=` word (`exec`), `$$` for a variable. A line break, a tab and
/// every other control character is written as its C escape (`\n`, `\t`, `\r`, `\xNN` below
/// U+0080, `\uNNNN` above it), which systemd decodes inside the word. Written raw, a line break
/// would end the directive (systemd ends a line at a line feed and at a carriage return), and
/// what follows it in a program path or a home would be read as a line of its own: another
/// `ExecStart=` or `Environment=`.
fn systemd_escape_into(out: &mut String, val: &str, exec: bool) {
    for ch in val.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' if exec => out.push_str("$$"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() && c.is_ascii() => {
                out.push_str(&format!("\\x{:02x}", u32::from(c)));
            }
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
}

fn systemd_escape_exec(word: &str) -> String {
    let mut out = String::with_capacity(word.len() + 2);
    out.push('"');
    systemd_escape_into(&mut out, word, true);
    out.push('"');
    out
}

fn systemd_escape_env(key: &str, val: &str) -> String {
    let mut out = String::with_capacity(key.len() + val.len() + 3);
    out.push('"');
    out.push_str(key);
    out.push('=');
    systemd_escape_into(&mut out, val, false);
    out.push('"');
    out
}

/// Render the Linux `systemd --user` unit for `systems.alab.aterm-node`.
#[must_use]
pub fn node_systemd_unit_text(program: &Path, home: &Path) -> String {
    let pkg_bin = home.join(".local/share/aterm/pkg/bin");
    let path_env = format!("{}:/usr/local/bin:/usr/bin:/bin", pkg_bin.to_string_lossy());
    format!(
        "[Unit]\n\
         Description={NODE_DESCRIPTION}\n\n\
         [Service]\n\
         Type=simple\n\
         ExecStart={} \"node\" \"serve\"\n\
         Environment={}\n\
         Environment={}\n\
         Restart=always\n\
         RestartSec=5\n\n\
         [Install]\n\
         WantedBy=default.target\n",
        systemd_escape_exec(&program.to_string_lossy()),
        systemd_escape_env("HOME", &home.to_string_lossy()),
        systemd_escape_env("PATH", &path_env),
    )
}

fn systemd_unescape_exec(rest: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = rest.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => return (!out.is_empty()).then_some(out),
            '\\' => match chars.next()? {
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'x' => out.push(hex_char(&mut chars, 2).filter(char::is_ascii)?),
                'u' => out.push(hex_char(&mut chars, 4)?),
                other => {
                    out.push('\\');
                    out.push(other);
                }
            },
            '%' if chars.peek() == Some(&'%') => {
                chars.next();
                out.push('%');
            }
            '$' if chars.peek() == Some(&'$') => {
                chars.next();
                out.push('$');
            }
            c => out.push(c),
        }
    }
    None
}

/// The character the next `digits` hex digits of `chars` name (a `\x` or `\u` escape's), if
/// they name one other than NUL, which systemd refuses in an escape.
fn hex_char(chars: &mut impl Iterator<Item = char>, digits: usize) -> Option<char> {
    let mut code = 0u32;
    for _ in 0..digits {
        code = (code << 4) | chars.next()?.to_digit(16)?;
    }
    char::from_u32(code).filter(|&c| c != '\0')
}

/// Extract the `ProgramArguments[0]` or `ExecStart` binary path from a rendered unit file.
#[must_use]
pub fn kept_program_in_unit(text: &str) -> Option<PathBuf> {
    if let Some(after) = text.split("<key>ProgramArguments</key>").nth(1)
        && let Some(array) = after.split("</array>").next()
        && let Some(first) = array.split("<string>").nth(1)
        && let Some(raw) = first.split("</string>").next()
    {
        let unescaped = raw
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&");
        if !unescaped.is_empty() {
            return Some(PathBuf::from(unescaped));
        }
    }
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("ExecStart=\"")
            && let Some(unescaped) = systemd_unescape_exec(rest)
        {
            return Some(PathBuf::from(unescaped));
        }
    }
    None
}

/// Observed state of `systems.alab.aterm-node`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeJobState {
    /// Unit file is not installed.
    Absent,
    /// Unit file is installed; `pid` is `Some` when launchd/systemd reports it running.
    Installed {
        /// Running process ID, if active.
        pid: Option<u32>,
        /// Path to the plist or systemd service file.
        unit: PathBuf,
        /// Binary path named in the unit file, if readable.
        program: Option<PathBuf>,
    },
}

/// Platform supervisor seam so unit tests exercise `on`, `off`, `sync.node`, and `fleet leave`
/// deterministically without touching the host's real launchd/systemd.
pub trait ServiceSupervisor {
    /// Unit file path under `home` (`~/Library/LaunchAgents/...` or `~/.config/systemd/user/...`).
    fn unit_file(&self, home: &Path) -> PathBuf;
    /// Render unit file contents for `program`.
    fn render_unit(&self, program: &Path, home: &Path, state_dir: &Path) -> String;
    /// Query running PID (`Ok(Some(pid))` if running, `Ok(None)` if not running).
    ///
    /// # Errors
    /// Returns an error if the supervisor query itself failed fatally.
    fn running_pid(&self, home: &Path) -> Result<Option<u32>, String>;
    /// The release actually run by `program`, measured under a bounded deadline.
    ///
    /// # Errors
    /// The program did not identify as an aterm release, or this supervisor does not measure it.
    fn program_release(&self, _program: &Path) -> Result<(u64, u64, u64), String> {
        Err("the program's release was not measured".to_owned())
    }
    /// The oldest release a fresh service may run: the one this process is, as its installed
    /// identity says (`aterm_stamp`).
    ///
    /// # Errors
    /// This process installed no identity, or its version is not `MAJOR.MINOR.PATCH`: nothing
    /// says which release includes this node implementation, so no program is chosen.
    fn required_release(&self) -> Result<(u64, u64, u64), String> {
        aterm_stamp::installed()
            .and_then(aterm_stamp::Stamp::release)
            .ok_or_else(|| "this process carries no release identity".to_owned())
    }
    /// Load/restart the written unit file at `unit_path`.
    ///
    /// # Errors
    /// Returns an error if `launchctl` or `systemctl` rejected the unit.
    fn load_and_start(&self, unit_path: &Path) -> Result<(), String>;
    /// Stop and unload `systems.alab.aterm-node` (ignoring already-unloaded units).
    ///
    /// # Errors
    /// Returns an error if unloading a loaded unit failed.
    fn stop_and_unload(&self, unit_path: &Path) -> Result<(), String>;
    /// Why the running job stands where it should not, `None` where it stands right: on macOS, a
    /// job loaded in the background domain (`user/<uid>`, where an SSH-only pass put it) while a
    /// login holds the screen, whose `open -g -a` a spawn needs in `gui/<uid>`.
    fn misplaced(&self) -> Option<String> {
        None
    }
    /// Whether the job's process `pid` runs a build an update replaced: its executable image is
    /// no longer the file `program` names.
    fn runs_replaced(&self, pid: u32, program: &Path) -> bool {
        let _ = (pid, program);
        false
    }
    /// Positive evidence that `pid` has loaded `program`, or that it loaded another image.
    /// `None` is unreadable, never evidence that a replacement finished.
    fn running_image_matches(&self, pid: u32, program: &Path) -> Option<bool> {
        let _ = (pid, program);
        None
    }
    /// Restart the loaded job on the program its unit names now (`launchctl kickstart -k`),
    /// never a bootout followed at once by a bootstrap.
    ///
    /// # Errors
    /// The supervisor refused the restart.
    fn renew(&self) -> Result<(), String> {
        Ok(())
    }
    /// Why the unit file under `home` is not this supervisor's to write or remove, `None` where
    /// it is: an install refuses it, and a removal keeps it and says so.
    fn leaves_unit_alone(&self, home: &Path) -> Option<String> {
        let _ = home;
        None
    }
    /// Whether a person switched the job off in the supervisor (`launchctl disable`, or its
    /// switch in Login Items turned off): a sync pass leaves that job as their choice.
    ///
    /// # Errors
    /// The supervisor could not be asked, so whether it is switched off is not known.
    fn switched_off(&self) -> Result<bool, String> {
        Ok(false)
    }
}

/// One `launchctl` call, so the job's upkeep runs inert in a test.
pub trait Launchctl {
    /// Run `launchctl args…`: whether it exited 0, and what it printed (stdout, then stderr).
    ///
    /// # Errors
    /// `launchctl` could not be run, or did not end within [`LAUNCHCTL_WITHIN`].
    fn run(&self, args: &[&str]) -> Result<(bool, String), String>;
    /// Observe a service: its printed state, or `None` only when its service or domain is absent.
    ///
    /// # Errors
    /// The query could not run or was refused for a reason other than explicit absence.
    fn print_service(&self, target: &str) -> Result<Option<String>, String>;
}

/// How long one `launchctl` call may take.
pub const LAUNCHCTL_WITHIN: Duration = Duration::from_secs(60);

/// How long a reload waits for launchd to let the old job go before it loads the plist again.
pub const BOOTOUT_SETTLE: Duration = Duration::from_secs(60);

fn await_bootout(
    target: &str,
    started: Instant,
    within: Duration,
    mut still_held: impl FnMut() -> Result<bool, String>,
    mut pause: impl FnMut(Duration),
) -> Result<(), String> {
    while still_held()? {
        if started.elapsed() >= within {
            return Err(format!(
                "launchd still holds {target} {} s after its bootout; the next pass loads it",
                within.as_secs()
            ));
        }
        pause(Duration::from_millis(100));
    }
    Ok(())
}

fn deadline_after(d: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(d)
        .unwrap_or_else(|| now + Duration::from_secs(86_400))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn run_systemctl_bounded(systemctl: &str, args: &[&str]) -> Result<std::process::Output, String> {
    run_service_after(Path::new(systemctl), args, Duration::ZERO)
}

fn run_service_after(
    program: &Path,
    args: &[&str],
    elapsed: Duration,
) -> Result<std::process::Output, String> {
    let remaining = LAUNCHCTL_WITHIN
        .checked_sub(elapsed)
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| {
            format!(
                "{} {}: required service command timed out",
                program.display(),
                args.join(" ")
            )
        })?;
    let mut command = aterm_uds::spawnfd::command(program);
    command.args(args);
    aterm_uds::deadline::output_until(&mut command, deadline_after(remaining))
        .map_err(|e| format!("{} {}: {e}", program.display(), args.join(" ")))
}

/// `/bin/launchctl`, each call bounded by [`LAUNCHCTL_WITHIN`].
pub struct RealLaunchctl;

impl RealLaunchctl {
    fn output(&self, args: &[&str]) -> Result<(std::process::ExitStatus, String), String> {
        let out = run_service_after(Path::new(LAUNCHCTL), args, Duration::ZERO)?;
        let mut said = String::from_utf8_lossy(&out.stdout).into_owned();
        said.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok((out.status, said))
    }
}

fn printed_service(
    target: &str,
    code: Option<i32>,
    said: String,
) -> Result<Option<String>, String> {
    match code {
        Some(0) => Ok(Some(said)),
        // `launchctl error 112` and `113`: absent domain and absent service. Other
        // nonzero statuses, including a permission refusal, establish no absence.
        Some(112 | 113) => Ok(None),
        _ => Err(format!(
            "{LAUNCHCTL} print {target} (exit {code:?}): {}",
            said.trim()
        )),
    }
}

impl Launchctl for RealLaunchctl {
    fn run(&self, args: &[&str]) -> Result<(bool, String), String> {
        let (status, said) = self.output(args)?;
        Ok((status.success(), said))
    }

    fn print_service(&self, target: &str) -> Result<Option<String>, String> {
        let (status, said) = self.output(&["print", target])?;
        printed_service(target, status.code(), said)
    }
}

/// The job's launchd upkeep for user `uid`, every call through `launchctl`.
pub struct Launchd<'a> {
    /// The seam each call goes through.
    pub launchctl: &'a dyn Launchctl,
    /// The user whose domains hold the job.
    pub uid: u32,
    /// How long a reload waits for a bootout to settle ([`BOOTOUT_SETTLE`]).
    pub settle: Duration,
}

impl Launchd<'_> {
    fn gui(&self) -> String {
        format!("gui/{}", self.uid)
    }

    fn user(&self) -> String {
        format!("user/{}", self.uid)
    }

    /// The domain the job belongs in now: the login at the screen's where there is one, else the
    /// user's background domain.
    fn wanted(&self) -> Result<String, String> {
        Ok(if self.launchctl.run(&["print", &self.gui()])?.0 {
            self.gui()
        } else {
            self.user()
        })
    }

    /// Each domain that holds the job, with what `launchctl print` says of it there.
    fn printed(&self) -> Result<Vec<(String, String)>, String> {
        let mut held = Vec::new();
        for domain in [self.gui(), self.user()] {
            let printed = self
                .launchctl
                .print_service(&format!("{domain}/{NODE_LABEL}"))?;
            if let Some(said) = printed {
                held.push((domain, said));
            }
        }
        Ok(held)
    }

    /// Each domain that holds the job, with its pid while it runs.
    fn loaded(&self) -> Result<Vec<(String, Option<u32>)>, String> {
        Ok(self
            .printed()?
            .into_iter()
            .map(|(domain, said)| {
                let pid = parse_launchctl_pid(&said);
                (domain, pid)
            })
            .collect())
    }

    /// Whether `launchctl print-disabled gui/<uid>` names the job disabled ([`disabled_in`]).
    ///
    /// # Errors
    /// `print-disabled` could not run or failed.
    pub fn switched_off(&self) -> Result<bool, String> {
        let domain = self.gui();
        let (ok, said) = self.launchctl.run(&["print-disabled", &domain])?;
        if !ok {
            return Err(format!(
                "{LAUNCHCTL} print-disabled {domain}: {}",
                said.trim()
            ));
        }
        Ok(disabled_in(&said, NODE_LABEL))
    }

    /// The running job's pid, in whichever domain holds it.
    ///
    /// # Errors
    /// `launchctl` could not be asked.
    pub fn running_pid(&self) -> Result<Option<u32>, String> {
        Ok(self.loaded()?.into_iter().find_map(|(_, pid)| pid))
    }

    /// Load the plist at `unit_path` in the domain the job belongs in: every domain that holds the
    /// job lets it go first, and the plist is loaded only once launchd no longer holds it, since a
    /// bootstrap during the teardown fails and strands the job unloaded.
    ///
    /// # Errors
    /// A bootout did not settle within [`Self::settle`], or launchd refused the bootstrap.
    pub fn load(&self, unit_path: &Path) -> Result<(), String> {
        let wanted = self.wanted()?;
        for (domain, _) in self.loaded()? {
            let target = format!("{domain}/{NODE_LABEL}");
            let _ = self.launchctl.run(&["bootout", &target])?;
            await_bootout(
                &target,
                Instant::now(),
                self.settle,
                || self.launchctl.run(&["print", &target]).map(|out| out.0),
                std::thread::sleep,
            )?;
        }
        let unit = unit_path.to_string_lossy();
        let (ok, said) = self.launchctl.run(&["bootstrap", &wanted, &unit])?;
        if ok {
            return Ok(());
        }
        let refused = format!("{LAUNCHCTL} bootstrap {wanted} {unit}: {}", said.trim());
        // A move the login's session refuses must not strand the job unloaded: it goes back to
        // the background session it ran in, and the pass still reports the refusal.
        let user = self.user();
        if wanted != user && self.launchctl.run(&["bootstrap", &user, &unit])?.0 {
            return Err(format!("{refused}; kept it in {user}"));
        }
        Err(refused)
    }

    /// Unload the job `unit` loaded from every domain that holds it. A job launchd names another
    /// plist for is another home's (the person's own, beside an isolated home's leave), so it
    /// stays; one whose print names no plist is taken for this one's.
    ///
    /// # Errors
    /// `launchctl` could not be asked, or refused to unload a job it reported loaded.
    pub fn unload(&self, unit: &Path) -> Result<(), String> {
        for (domain, said) in self.printed()? {
            if launchctl_path(&said).is_some_and(|path| path != unit) {
                continue;
            }
            let target = format!("{domain}/{NODE_LABEL}");
            let (ok, said) = self.launchctl.run(&["bootout", &target])?;
            if !ok {
                return Err(format!("{LAUNCHCTL} bootout {target}: {}", said.trim()));
            }
        }
        Ok(())
    }

    /// See [`ServiceSupervisor::misplaced`].
    #[must_use]
    pub fn misplaced(&self) -> Option<String> {
        let wanted = self.wanted().ok()?;
        let held = self.loaded().ok()?;
        (!held.is_empty() && held.iter().any(|(domain, _)| *domain != wanted)).then(|| {
            format!(
                "{NODE_LABEL} is loaded in {}, and it belongs in {wanted}, where the login at the \
                 screen is",
                held.iter()
                    .map(|(domain, _)| domain.as_str())
                    .collect::<Vec<_>>()
                    .join(" and ")
            )
        })
    }

    /// See [`ServiceSupervisor::renew`].
    ///
    /// # Errors
    /// No domain holds the job, or launchd refused the restart.
    pub fn renew(&self) -> Result<(), String> {
        let Some((domain, _)) = self.loaded()?.into_iter().next() else {
            return Err(format!("launchd holds no {NODE_LABEL} to renew"));
        };
        let target = format!("{domain}/{NODE_LABEL}");
        let (ok, said) = self.launchctl.run(&["kickstart", "-k", &target])?;
        if ok {
            Ok(())
        } else {
            Err(format!(
                "{LAUNCHCTL} kickstart -k {target}: {}",
                said.trim()
            ))
        }
    }
}

/// Whether process `pid`'s executable image is another file than the one `program` names now
/// (an update replaced it at the same path).
#[must_use]
pub fn image_replaced(pid: u32, program: &Path) -> bool {
    i32::try_from(pid)
        .ok()
        .and_then(|pid| aterm_uds::process::image_matches_file(pid, program))
        == Some(false)
}

/// Live OS supervisor (`launchd` on macOS, `systemd --user` on Linux).
pub struct LiveSupervisor;

/// A rejected disable is harmless only when the supervisor explicitly names no loaded unit
/// and no running process. Missing, mixed or unreadable status stays a refusal.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn systemd_absent(state: &str) -> bool {
    let field = |key: &str| {
        let mut values = state.lines().filter_map(|line| {
            let (name, value) = line.split_once('=')?;
            (name == key).then_some(value)
        });
        let value = values.next()?;
        values.next().is_none().then_some(value)
    };
    field("LoadState") == Some("not-found")
        && field("ActiveState") == Some("inactive")
        && field("MainPID") == Some("0")
}

#[cfg(any(test, all(unix, not(target_os = "macos"))))]
type SystemctlResult = Result<(bool, String), String>;

#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn unload_systemd(run: &dyn Fn(&[&str]) -> SystemctlResult) -> Result<(), String> {
    let service = format!("{NODE_LABEL}.service");
    let (ok, said) = run(&["--user", "disable", "--now", &service])?;
    if !ok {
        let (_, state) = run(&[
            "--user",
            "show",
            "--property=LoadState",
            "--property=ActiveState",
            "--property=MainPID",
            &service,
        ])?;
        if !systemd_absent(&state) {
            return Err(format!(
                "systemctl --user disable --now {service}: {}",
                said.trim()
            ));
        }
    }
    let (ok, said) = run(&["--user", "daemon-reload"])?;
    if !ok {
        return Err(format!("systemctl --user daemon-reload: {}", said.trim()));
    }
    Ok(())
}

impl LiveSupervisor {
    /// The bundle identifier of the app `program` (through any link to it) is the executable of,
    /// as its `Info.plist` names it, so the job is aterm.app's, as `secrets watch`'s is.
    fn bundle_id_for(program: &Path) -> Option<String> {
        #[cfg(unix)]
        {
            let exe = program
                .canonicalize()
                .unwrap_or_else(|_| program.to_path_buf());
            aterm_secrets::service::bundle_id_of(&exe)
        }
        #[cfg(not(unix))]
        {
            let _ = program;
            None
        }
    }
}

impl ServiceSupervisor for LiveSupervisor {
    fn switched_off(&self) -> Result<bool, String> {
        #[cfg(target_os = "macos")]
        {
            live_launchd(&RealLaunchctl).switched_off()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(false)
        }
    }

    fn program_release(&self, program: &Path) -> Result<(u64, u64, u64), String> {
        #[cfg(unix)]
        {
            aterm_secrets::service::program_release(program).map_err(|error| error.to_string())
        }
        #[cfg(not(unix))]
        {
            let _ = program;
            Err("node service is Unix only".to_owned())
        }
    }

    fn unit_file(&self, home: &Path) -> PathBuf {
        if cfg!(target_os = "macos") {
            plist_path(home)
        } else {
            unit_path(home)
        }
    }

    fn render_unit(&self, program: &Path, home: &Path, state_dir: &Path) -> String {
        if cfg!(target_os = "macos") {
            node_plist_text(
                program,
                home,
                state_dir,
                Self::bundle_id_for(program).as_deref(),
            )
        } else {
            node_systemd_unit_text(program, home)
        }
    }

    fn running_pid(&self, _home: &Path) -> Result<Option<u32>, String> {
        #[cfg(target_os = "macos")]
        {
            live_launchd(&RealLaunchctl).running_pid()
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let Some(systemctl) = SYSTEMCTL_PATHS.into_iter().find(|p| Path::new(p).is_file())
            else {
                return Ok(None);
            };
            let service = format!("{NODE_LABEL}.service");
            let out = run_systemctl_bounded(
                systemctl,
                &["--user", "show", "--property=MainPID", "--value", &service],
            )?;
            if !out.status.success() {
                return Ok(None);
            }
            let pid = crate::parse_uint::<u32>(String::from_utf8_lossy(&out.stdout).trim())
                .filter(|&p| p > 0);
            Ok(pid)
        }
        #[cfg(not(unix))]
        {
            Ok(None)
        }
    }

    fn load_and_start(&self, unit_path: &Path) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            live_launchd(&RealLaunchctl).load(unit_path)
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let _ = unit_path;
            let systemctl = SYSTEMCTL_PATHS
                .into_iter()
                .find(|p| Path::new(p).is_file())
                .ok_or_else(|| "systemctl not found".to_owned())?;
            let service = format!("{NODE_LABEL}.service");
            let _ = run_systemctl_bounded(systemctl, &["--user", "daemon-reload"]);
            let out = run_systemctl_bounded(systemctl, &["--user", "enable", "--now", &service])?;
            if !out.status.success() {
                return Err(format!(
                    "{systemctl} --user enable --now {service}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            let _ = run_systemctl_bounded(systemctl, &["--user", "restart", &service]);
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = unit_path;
            Err("node service is Unix only".to_owned())
        }
    }

    fn stop_and_unload(&self, unit_path: &Path) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            live_launchd(&RealLaunchctl).unload(unit_path)
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let _ = unit_path;
            let systemctl = SYSTEMCTL_PATHS
                .into_iter()
                .find(|p| Path::new(p).is_file())
                .ok_or_else(|| "systemctl not found; the node unit was kept".to_owned())?;
            unload_systemd(&|args| {
                let out = run_systemctl_bounded(systemctl, args)?;
                let mut said = String::from_utf8_lossy(&out.stdout).into_owned();
                said.push_str(&String::from_utf8_lossy(&out.stderr));
                Ok((out.status.success(), said))
            })
        }
        #[cfg(not(unix))]
        {
            let _ = unit_path;
            Ok(())
        }
    }

    fn misplaced(&self) -> Option<String> {
        #[cfg(target_os = "macos")]
        {
            live_launchd(&RealLaunchctl).misplaced()
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    fn runs_replaced(&self, pid: u32, program: &Path) -> bool {
        image_replaced(pid, program)
    }

    fn running_image_matches(&self, pid: u32, program: &Path) -> Option<bool> {
        aterm_uds::process::image_matches_file(i32::try_from(pid).ok()?, program)
    }

    fn renew(&self) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            live_launchd(&RealLaunchctl).renew()
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let systemctl = SYSTEMCTL_PATHS
                .into_iter()
                .find(|p| Path::new(p).is_file())
                .ok_or_else(|| "systemctl not found".to_owned())?;
            let service = format!("{NODE_LABEL}.service");
            let out = run_systemctl_bounded(systemctl, &["--user", "restart", &service])?;
            if out.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "{systemctl} --user restart {service}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        }
        #[cfg(not(unix))]
        {
            Ok(())
        }
    }
}

/// Whether `print-disabled`'s `text` names `label` disabled: its rows read
/// `"<label>" => disabled` (`=> true` before macOS 13).
fn disabled_in(text: &str, label: &str) -> bool {
    let quoted = format!("\"{label}\"");
    text.lines().any(|line| {
        line.trim()
            .strip_prefix(&quoted)
            .and_then(|rest| rest.trim().strip_prefix("=>"))
            .is_some_and(|state| matches!(state.trim(), "disabled" | "true"))
    })
}

/// This user's launchd, as the live supervisor reaches it.
#[cfg(target_os = "macos")]
fn live_launchd(launchctl: &dyn Launchctl) -> Launchd<'_> {
    Launchd {
        launchctl,
        uid: aterm_uds::peer::our_uid(),
        settle: BOOTOUT_SETTLE,
    }
}

/// Inert supervisor for isolated environments (a private state root or `CI`): it writes and
/// removes unit files without calling the host's live `launchctl` or `systemctl`, and only under
/// a home that is not the account's own. The unit file under the account's own home is the one
/// the account's launchd or systemd loads: a job it started runs on (it is kept alive) after a
/// removal no stop came with, and a unit written there starts the account's job on this
/// process's program and state at its next load. A job a session outside `CI` loaded can stand
/// there whatever this process installed, so the file is left alone.
pub struct FileOnlySupervisor {
    /// The account's own home, `None` where it could not be read, which leaves every unit file
    /// alone.
    account_home: Option<PathBuf>,
}

impl FileOnlySupervisor {
    /// The file-only supervisor of the account this process runs as, whose home is the account
    /// database's ([`aterm_uds::account::home`]), whatever `$HOME` says.
    #[must_use]
    pub fn of_this_account() -> Self {
        Self::guarding(aterm_uds::account::home())
    }

    /// One that leaves alone the unit file under `account_home`, taken for the account's own.
    #[must_use]
    pub(crate) const fn guarding(account_home: Option<PathBuf>) -> Self {
        Self { account_home }
    }

    /// Whether the unit file under `home` is the one under the account's home `account`: `home`
    /// is that home ([`aterm_types::dirs::same_directory`]), or the unit's folder under it is the
    /// account's, reached through a link (a scratch home whose `Library` links to the account's).
    fn reaches_accounts_unit(&self, home: &Path, account: &Path) -> bool {
        let folder = |home: &Path| self.unit_file(home).parent().map(Path::to_path_buf);
        aterm_types::dirs::same_directory(home, account)
            || folder(home)
                .zip(folder(account))
                .is_some_and(|(home, account)| aterm_types::dirs::same_directory(&home, &account))
    }
}

impl ServiceSupervisor for FileOnlySupervisor {
    fn leaves_unit_alone(&self, home: &Path) -> Option<String> {
        match &self.account_home {
            Some(account) if !self.reaches_accounts_unit(home, account) => None,
            Some(_) => Some(
                "it is this account's own, and this process (under a private state root or CI) \
                 reaches none of the account's jobs, so it neither writes nor removes it"
                    .to_owned(),
            ),
            None => Some(
                "this account's home could not be read, so it may be the account's own, and \
                 this process (under a private state root or CI) reaches none of the account's \
                 jobs, so it neither writes nor removes it"
                    .to_owned(),
            ),
        }
    }

    fn unit_file(&self, home: &Path) -> PathBuf {
        LiveSupervisor.unit_file(home)
    }

    fn render_unit(&self, program: &Path, home: &Path, state_dir: &Path) -> String {
        LiveSupervisor.render_unit(program, home, state_dir)
    }

    fn running_pid(&self, _home: &Path) -> Result<Option<u32>, String> {
        Ok(None)
    }

    fn load_and_start(&self, _unit_path: &Path) -> Result<(), String> {
        Ok(())
    }

    fn stop_and_unload(&self, _unit_path: &Path) -> Result<(), String> {
        Ok(())
    }
}

/// The plist a job was loaded from, as `launchctl print` names it (its `path = …` line).
fn launchctl_path(text: &str) -> Option<&Path> {
    text.lines()
        .find_map(|line| line.strip_prefix("\tpath = "))
        .map(|path| Path::new(path.trim_end()))
}

/// Parse `pid = <u32>` from `launchctl print` output.
#[must_use]
pub fn parse_launchctl_pid(text: &str) -> Option<u32> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("pid = ") {
            let digits = rest.trim_end_matches(';').trim();
            if let Some(pid) = crate::parse_uint::<u32>(digits)
                && pid > 0
            {
                return Some(pid);
            }
        }
    }
    None
}

const UNIT_READ_MAX_BYTES: u64 = 256 * 1024;

/// Inspect `systems.alab.aterm-node` state via `supervisor`.
///
/// # Errors
/// Returns an error if reading the unit file or supervisor state fails fatally.
pub fn state_with(home: &Path, supervisor: &dyn ServiceSupervisor) -> Result<NodeJobState, String> {
    let unit = supervisor.unit_file(home);
    let program = match crate::harness::accounts::read_bounded(&unit, UNIT_READ_MAX_BYTES) {
        Ok(None) => return Ok(NodeJobState::Absent),
        Ok(Some(text)) => kept_program_in_unit(&text),
        Err(_) => {
            if !std::fs::metadata(&unit).is_ok_and(|meta| meta.is_file()) {
                return Ok(NodeJobState::Absent);
            }
            None
        }
    };
    let pid = supervisor.running_pid(home)?;
    Ok(NodeJobState::Installed { pid, unit, program })
}

/// Install or update `systems.alab.aterm-node`, clearing any `node-off` marker when `clear_off`
/// is true.
///
/// # Errors
/// Returns an error if the supervisor leaves the unit file under `home` alone
/// ([`ServiceSupervisor::leaves_unit_alone`]), changing nothing, or if writing the unit file or
/// loading the job fails.
pub fn install_with(
    program: &Path,
    home: &Path,
    state_dir: &Path,
    clear_off: bool,
    supervisor: &dyn ServiceSupervisor,
) -> Result<PathBuf, String> {
    #[cfg(unix)]
    let _guard = node_admin_lock(state_dir)?;
    install_locked(program, home, state_dir, clear_off, supervisor)
}

fn install_locked(
    program: &Path,
    home: &Path,
    state_dir: &Path,
    clear_off: bool,
    supervisor: &dyn ServiceSupervisor,
) -> Result<PathBuf, String> {
    let unit = supervisor.unit_file(home);
    if let Some(why) = supervisor.leaves_unit_alone(home) {
        return Err(format!(
            "{} was left alone, and nothing changed: {why}",
            unit.display()
        ));
    }
    aterm_uds::private_file::ensure_private_dir(state_dir).map_err(|e| e.to_string())?;
    if clear_off {
        let off = off_path(state_dir);
        match std::fs::remove_file(&off) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", off.display())),
        }
    }
    if let Some(parent) = unit.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let rendered = supervisor.render_unit(program, home, state_dir);
    aterm_secrets::fsx::write_atomic(&unit, rendered.as_bytes(), 0o644)
        .map_err(|e| format!("{}: {e}", unit.display()))?;
    supervisor.load_and_start(&unit)?;
    Ok(unit)
}

/// What a removal of `systems.alab.aterm-node` did with its unit file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    /// Nothing was removed, and there is nothing to say: no unit file stood under the home, or a
    /// join after the leave owns the job.
    Nothing,
    /// The job was stopped and this unit file removed.
    Removed(PathBuf),
    /// This unit file was left alone, and no job was stopped.
    Kept {
        /// The unit file.
        unit: PathBuf,
        /// Why it is not the supervisor's to remove
        /// ([`ServiceSupervisor::leaves_unit_alone`]).
        why: String,
    },
}

/// Stop and remove `systems.alab.aterm-node`. When `record_off` is true (`aterm node off`),
/// writes `<state_dir>/node-off` so `sync.node` leaves it off; when false (`aterm fleet leave`),
/// removes the unit cleanly without recording `node-off`. A unit file the supervisor leaves
/// alone is kept ([`Removal::Kept`]), `node-off` recorded all the same.
///
/// # Errors
/// Returns an error if writing `node-off`, stopping the loaded job or removing the unit file
/// fails. A failed stop preserves the unit so it can be retried.
pub fn remove_with(
    home: &Path,
    state_dir: &Path,
    record_off: bool,
    supervisor: &dyn ServiceSupervisor,
) -> Result<Removal, String> {
    #[cfg(unix)]
    let _guard = node_admin_lock(state_dir)?;
    remove_locked(home, state_dir, record_off, supervisor)
}

/// Remove the node job after a fleet leave has let the book's lock go, only while membership
/// is still absent. The check and service removal share the node administration lock with
/// `sync.node`, so a job a later join already started is kept. A join published after this
/// check can be served by an ensure once the removal releases that lock.
/// # Errors
/// As [`remove_with`], or membership cannot be read well enough to establish absence.
pub(crate) fn remove_after_fleet_leave_with(
    home: &Path,
    state_dir: &Path,
    supervisor: &dyn ServiceSupervisor,
) -> Result<Removal, String> {
    let _guard = node_admin_lock(state_dir)?;
    match aterm_net::fleet::read_key_file(state_dir) {
        Ok(_) => return Ok(Removal::Nothing),
        Err(aterm_net::fleet::KeyFileError::Missing) => {}
        Err(why) => {
            return Err(format!(
                "the node job was kept: fleet absence could not be established ({why})"
            ));
        }
    }
    remove_locked(home, state_dir, false, supervisor)
}

/// The stop and unlink while the caller holds the node administration lock.
fn remove_locked(
    home: &Path,
    state_dir: &Path,
    record_off: bool,
    supervisor: &dyn ServiceSupervisor,
) -> Result<Removal, String> {
    if record_off {
        aterm_uds::private_file::ensure_private_dir(state_dir).map_err(|e| e.to_string())?;
        aterm_uds::private_file::write_private(
            state_dir,
            NODE_OFF_FILE,
            b"the node job was turned off by choice; aterm node on turns it back on\n",
        )
        .map_err(|e| format!("{}: {e}", off_path(state_dir).display()))?;
    }
    let unit = supervisor.unit_file(home);
    let existed = unit.is_file();
    if let Some(why) = supervisor.leaves_unit_alone(home) {
        return Ok(if existed {
            Removal::Kept { unit, why }
        } else {
            Removal::Nothing
        });
    }
    supervisor.stop_and_unload(&unit)?;
    match std::fs::remove_file(&unit) {
        Ok(()) => Ok(Removal::Removed(unit)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(if existed {
            Removal::Removed(unit)
        } else {
            Removal::Nothing
        }),
        Err(e) => Err(format!("{}: {e}", unit.display())),
    }
}

/// The supervisor this process reaches `systems.alab.aterm-node` through: [`FileOnlySupervisor`]
/// where this process runs under a private state root (a build with dev seams told so by
/// `ATERM_STATE_HOME`: [`aterm_types::dirs::runs_under_private_state_root`]) or under `CI`, since
/// its launchd or systemd is still the host's and the job loaded there serves other state, else
/// [`LiveSupervisor`]. The file-only one writes and removes a unit file only under a home that
/// is not the account's own, so `aterm node`, `fleet leave` and `sync.node` there leave the
/// account's own unit alone. Outside `CI`, a release build, whose state is the person's whatever
/// `ATERM_STATE_HOME` says, reaches the person's job, that state's.
#[must_use]
pub fn supervisor_here() -> &'static dyn ServiceSupervisor {
    static FILE_ONLY: std::sync::OnceLock<FileOnlySupervisor> = std::sync::OnceLock::new();
    if aterm_types::dirs::runs_under_private_state_root() || std::env::var_os("CI").is_some() {
        FILE_ONLY.get_or_init(FileOnlySupervisor::of_this_account)
    } else {
        &LiveSupervisor
    }
}

/// Converge read state for `sync.node` (C20).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncNodeState {
    /// Not applicable on this Mac (turned off, not in a fleet, CI, or no installed binary).
    NotApplicable(String),
    /// Installed and running the expected binary.
    Current(String),
    /// Needs installation, binary update, or restart.
    Due {
        /// Current observed state.
        from: String,
        /// Target state after `sync.node` applies.
        to: String,
    },
}

/// Resolve the installed release `aterm` executable for `sync.node` (`current_exe` when it is an
/// installed release app, else `/Applications/aterm.app/Contents/MacOS/aterm` on macOS or
/// `<home>/.local/bin/aterm` on Linux). Never returns a scratch or cargo build binary.
#[must_use]
pub fn resolve_installed_program(
    current_exe: Option<&Path>,
    installed_release: bool,
    home: &Path,
) -> Option<PathBuf> {
    if installed_release
        && let Some(exe) = current_exe
        && exe.is_file()
    {
        return Some(exe.to_path_buf());
    }
    let mac_app = PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm");
    if cfg!(target_os = "macos") && mac_app.is_file() {
        return Some(mac_app);
    }
    let linux_bin = home.join(".local/bin/aterm");
    if cfg!(target_os = "linux") && linux_bin.is_file() {
        return Some(linux_bin);
    }
    None
}

/// A fresh service must run a release that includes this node implementation
/// ([`ServiceSupervisor::required_release`]).
fn compatible_program(
    proposed: &Path,
    release: (u64, u64, u64),
    supervisor: &dyn ServiceSupervisor,
) -> Result<PathBuf, String> {
    let required = supervisor.required_release()?;
    if release < required {
        let (major, minor, patch) = required;
        return Err(format!(
            "{} is older than this node implementation ({major}.{minor}.{patch}); the node job \
             was kept",
            proposed.display()
        ));
    }
    Ok(proposed.to_path_buf())
}

/// A retained helper may propose a stale second app. Compare the actual programs before
/// moving an existing job, preserving a verified newer program even when it needs renewal.
fn sync_program(
    proposed: &Path,
    state: &NodeJobState,
    supervisor: &dyn ServiceSupervisor,
) -> Result<PathBuf, String> {
    let NodeJobState::Installed {
        program: Some(kept),
        ..
    } = state
    else {
        if matches!(state, NodeJobState::Installed { pid: Some(_), .. }) {
            return Err("the running node program is unknown; the node job was kept".to_owned());
        }
        return compatible_program(proposed, supervisor.program_release(proposed)?, supervisor);
    };
    if kept == proposed {
        return Ok(proposed.to_path_buf());
    }
    match (
        supervisor.program_release(kept),
        supervisor.program_release(proposed),
    ) {
        (Ok(current), Ok(next)) if current > next => Ok(kept.clone()),
        (Ok(_), Err(_)) => Ok(kept.clone()),
        (Err(_), Ok(_))
            if matches!(state, NodeJobState::Installed { pid: Some(_), .. }) && kept.is_file() =>
        {
            Err(format!(
                "{} is running but its release is unknown; the node job was kept",
                kept.display()
            ))
        }
        (Err(_), Ok(next)) => compatible_program(proposed, next, supervisor),
        (Ok(_), Ok(_)) => Ok(proposed.to_path_buf()),
        (Err(_), Err(_)) => Err(format!(
            "neither {} nor {} identified as an aterm release; the node job was kept",
            kept.display(),
            proposed.display()
        )),
    }
}

fn sync_gate(
    state_dir: &Path,
    aterm_config: Option<&Path>,
    under_ci: bool,
) -> Result<Option<String>, String> {
    sync_gate_with_mesh(
        state_dir,
        aterm_config,
        under_ci,
        aterm_types::mesh_switch::MESH,
    )
}

fn sync_gate_with_mesh(
    state_dir: &Path,
    aterm_config: Option<&Path>,
    under_ci: bool,
    mesh: bool,
) -> Result<Option<String>, String> {
    if !mesh
        && let Some(cfg) = aterm_config
        && !aterm_secrets::swarm::enabled_at(cfg).map_err(|e| e.to_string())?
    {
        return Ok(Some(aterm_secrets::swarm::SWARM_OFF.to_owned()));
    }
    if under_ci {
        return Ok(Some("under CI no pass installs the node job".to_owned()));
    }
    if node_turned_off(state_dir) {
        return Ok(Some(
            "off on this Mac by your choice: `aterm node on` turns it on".to_owned(),
        ));
    }
    // With the mesh on this job also hosts the local fabric, independently of swarm and fleet
    // membership. CI and an explicit node off still stand aside. On the old path membership is
    // the fleet's key. A leave removes it (and writes `fleet-off`, which keeps the
    // default fleet's sync from joining again); a fleet a person forms or joins here after that is
    // served by the window all the same, so the job stands for it too.
    if !mesh && aterm_net::fleet::read_key_file(state_dir).is_err() {
        return Ok(Some(
            if left_the_fleet(state_dir) {
                "this Mac left its fleet (`aterm fleet leave`)"
            } else {
                "this Mac is in no fleet (sync.fleet)"
            }
            .to_owned(),
        ));
    }
    Ok(None)
}

/// Pure `sync.node` probe over injected paths and `supervisor`.
///
/// # Errors
/// Returns an error if reading `aterm.toml` or querying `supervisor` fails, or a running
/// program's release cannot be established before the job would be moved.
pub fn read_for_sync_with(
    home: &Path,
    state_dir: &Path,
    aterm_config: Option<&Path>,
    program: Option<&Path>,
    under_ci: bool,
    supervisor: &dyn ServiceSupervisor,
) -> Result<SyncNodeState, String> {
    if let Some(why) = sync_gate(state_dir, aterm_config, under_ci)? {
        return Ok(SyncNodeState::NotApplicable(why));
    }
    let Some(program) = program else {
        return Ok(SyncNodeState::NotApplicable(
            "no installed release aterm binary on this Mac".to_owned(),
        ));
    };
    let state = state_with(home, supervisor)?;
    // A job that does not run because a person switched it off stays off: putting it back is
    // theirs to do. Where launchd cannot say, the pass goes on as before.
    if !matches!(state, NodeJobState::Installed { pid: Some(_), .. })
        && supervisor.switched_off().unwrap_or(false)
    {
        return Ok(SyncNodeState::NotApplicable(format!(
            "{NODE_LABEL} is switched off in launchd (Login Items or `launchctl disable`), \
             which a sync pass leaves as the person's choice"
        )));
    }
    let program = sync_program(program, &state, supervisor)?;
    let target = format!("{NODE_LABEL} running {}", program.display());
    match state {
        NodeJobState::Absent => Ok(SyncNodeState::Due {
            from: format!("{NODE_LABEL} is not installed"),
            to: target,
        }),
        NodeJobState::Installed {
            pid: Some(pid),
            program: Some(kept),
            ..
        } if kept == program => Ok(match renewal(supervisor, home, state_dir, pid, &kept) {
            Some(Renewal::Reload(why)) => SyncNodeState::Due {
                from: why,
                to: target,
            },
            Some(Renewal::Restart) => SyncNodeState::Due {
                from: format!(
                    "{NODE_LABEL} (pid {pid}) runs a build an update replaced ({} is another \
                     file now)",
                    kept.display()
                ),
                to: format!("{target}, renewed with kickstart -k"),
            },
            None => {
                SyncNodeState::Current(format!("{NODE_LABEL} runs (pid {pid}, {})", kept.display()))
            }
        }),
        NodeJobState::Installed {
            pid, program: kept, ..
        } => {
            let from = match (pid, kept) {
                (Some(pid), Some(kept)) => {
                    format!("{NODE_LABEL} runs {} (pid {pid})", kept.display())
                }
                (Some(pid), None) => format!("{NODE_LABEL} runs (pid {pid}, program unread)"),
                (None, Some(kept)) => {
                    format!(
                        "{NODE_LABEL} is installed ({}) and not running",
                        kept.display()
                    )
                }
                (None, None) => format!("{NODE_LABEL} is installed and not running"),
            };
            Ok(SyncNodeState::Due { from, to: target })
        }
    }
}

/// What a job that runs its unit's program still needs ([`renewal`]).
enum Renewal {
    /// Its unit must be written and loaded again (it sits in the wrong domain, or its plist is
    /// not the one this build writes): why.
    Reload(String),
    /// It runs a build an update replaced: a restart in place.
    Restart,
}

/// What the job `pid`, running `program` as its unit says, still needs, `None` where nothing.
/// A plist an earlier build wrote is rewritten, so a fix to the unit reaches jobs already
/// installed and not only new ones.
fn renewal(
    supervisor: &dyn ServiceSupervisor,
    home: &Path,
    state_dir: &Path,
    pid: u32,
    program: &Path,
) -> Option<Renewal> {
    if let Some(why) = supervisor.misplaced() {
        return Some(Renewal::Reload(why));
    }
    let unit = supervisor.unit_file(home);
    if crate::harness::accounts::read_bounded(&unit, UNIT_READ_MAX_BYTES)
        .ok()
        .flatten()
        != Some(supervisor.render_unit(program, home, state_dir))
    {
        return Some(Renewal::Reload(format!(
            "{} is not the unit this build writes",
            unit.display()
        )));
    }
    supervisor
        .runs_replaced(pid, program)
        .then_some(Renewal::Restart)
}

/// Apply `sync.node` via `supervisor`.
///
/// # Errors
/// Returns an error if installing or starting `systems.alab.aterm-node` fails.
pub fn ensure_for_sync_with(
    home: &Path,
    state_dir: &Path,
    aterm_config: Option<&Path>,
    program: Option<&Path>,
    under_ci: bool,
    supervisor: &dyn ServiceSupervisor,
) -> Result<String, String> {
    match read_for_sync_with(home, state_dir, aterm_config, program, under_ci, supervisor)? {
        SyncNodeState::NotApplicable(why) => Ok(format!("left as it is: {why}")),
        SyncNodeState::Current(now) => Ok(format!("nothing changed: {now}")),
        SyncNodeState::Due { .. } => {
            #[cfg(unix)]
            let _guard = node_admin_lock(state_dir)?;
            // Another window or the subscriber may have repaired it while this pass waited.
            // A stale Due must not reload the now-current job.
            match read_for_sync_with(home, state_dir, aterm_config, program, under_ci, supervisor)?
            {
                SyncNodeState::NotApplicable(why) => return Ok(format!("left as it is: {why}")),
                SyncNodeState::Current(now) => return Ok(format!("nothing changed: {now}")),
                SyncNodeState::Due { .. } => {}
            }
            // Off or fleet leave may have finished while version probes were running.
            // The same lock keeps either from completing between this check and mutation.
            if let Some(why) = sync_gate(state_dir, aterm_config, under_ci)? {
                return Ok(format!("left as it is: {why}"));
            }
            let Some(program) = program else {
                return Ok("left as it is: no installed release aterm binary".to_owned());
            };
            let state = state_with(home, supervisor)?;
            let program = sync_program(program, &state, supervisor)?;
            // A job that runs as its unit says, on a build an update replaced, restarts in place;
            // anything else is (re)installed, which reloads it where it belongs.
            if let NodeJobState::Installed {
                pid: Some(pid),
                program: Some(kept),
                ..
            } = state
                && kept == program
                && matches!(
                    renewal(supervisor, home, state_dir, pid, &kept),
                    Some(Renewal::Restart)
                )
            {
                supervisor.renew()?;
                return Ok(format!("renewed {NODE_LABEL} on this build (kickstart -k)"));
            }
            let unit = install_locked(&program, home, state_dir, false, supervisor)?;
            Ok(format!(
                "installed and started {NODE_LABEL} ({})",
                unit.display()
            ))
        }
    }
}

/// A committed installed window's relationship to its already-installed node job.
/// This is image readiness for that launch, not a network-health or access verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowNode {
    /// Another program, an absent job, or an explicit off is left to ordinary upkeep.
    Unmanaged,
    /// The job has positively loaded this window's installed program.
    Ready,
    /// The job's loaded image is unreadable, or its replacement has not started yet.
    Waiting,
    /// The same installed path now names a different image than the job loaded.
    Replaced,
}

/// Read readiness without changing the job. Different installed paths are not compared by
/// release number: two private builds can have the same version and different protocols.
///
/// # Errors
/// The job, its policy, or its unit cannot be read.
pub fn window_node_with(
    home: &Path,
    state_dir: &Path,
    aterm_config: Option<&Path>,
    program: &Path,
    under_ci: bool,
    supervisor: &dyn ServiceSupervisor,
) -> Result<WindowNode, String> {
    if sync_gate(state_dir, aterm_config, under_ci)?.is_some() || supervisor.switched_off()? {
        return Ok(WindowNode::Unmanaged);
    }
    if let Some(why) = supervisor.leaves_unit_alone(home) {
        return Err(why);
    }
    let NodeJobState::Installed {
        pid,
        program: Some(kept),
        ..
    } = state_with(home, supervisor)?
    else {
        return Ok(WindowNode::Unmanaged);
    };
    if kept != program {
        return Ok(WindowNode::Unmanaged);
    }
    let Some(pid) = pid else {
        return Ok(WindowNode::Waiting);
    };
    Ok(match supervisor.running_image_matches(pid, program) {
        Some(true) => WindowNode::Ready,
        Some(false) => WindowNode::Replaced,
        None => WindowNode::Waiting,
    })
}

/// Renew only a positively replaced job at this installed window's own path, then wait for its
/// loaded image. One call makes at most one renewal; later upkeep owns retries. The existing
/// administration lock keeps concurrent windows from repeating a successful renewal.
/// `wait` sleeps off the GUI thread and returns false when the window is shutting down.
///
/// # Errors
/// A policy or job read fails, renewal is refused, or the image remains unconfirmed after a
/// thirty-second polling budget. Lock acquisition and supervisor calls retain their own bounds.
/// The caller keeps its pending presence and may continue read-only observation.
pub fn renew_for_window_with(
    home: &Path,
    state_dir: &Path,
    aterm_config: Option<&Path>,
    program: &Path,
    under_ci: bool,
    supervisor: &dyn ServiceSupervisor,
    mut wait: impl FnMut(Duration) -> bool,
) -> Result<WindowNode, String> {
    let read = || window_node_with(home, state_dir, aterm_config, program, under_ci, supervisor);
    if !wait(Duration::ZERO) {
        return Err("the window ended before its node renewal".to_owned());
    }
    let first = read()?;
    if matches!(first, WindowNode::Unmanaged | WindowNode::Ready) {
        return Ok(first);
    }
    #[cfg(unix)]
    let _guard = node_admin_lock(state_dir)?;
    if !wait(Duration::ZERO) {
        return Err("the window ended before its node renewal".to_owned());
    }
    // A prior window may have finished while this one waited for the administration lock.
    match read()? {
        WindowNode::Unmanaged => return Ok(WindowNode::Unmanaged),
        WindowNode::Ready => return Ok(WindowNode::Ready),
        WindowNode::Replaced => supervisor.renew()?,
        WindowNode::Waiting => {}
    }
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let state = read()?;
        if matches!(state, WindowNode::Unmanaged | WindowNode::Ready) {
            return Ok(state);
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() || !wait(left.min(Duration::from_secs(1))) {
            return Err(
                "the node's loaded image has not confirmed this installed window".to_owned(),
            );
        }
    }
}

/// CLI text for `aterm node --help`.
pub const NODE_USAGE: &str = "\
usage: aterm node <command>

commands:
  serve    run the fleet/swarm node job in the foreground (launchd/systemd entry)
  status   show systems.alab.aterm-node installation and process status
  on       install and start systems.alab.aterm-node for this login
  off      stop and remove systems.alab.aterm-node (keeps sync.node off)
";

/// Run `aterm node status | on | off` (everything except `serve`, which `aterm` composes with
/// `aterm-db` and `aterm-gui`).
///
/// # Errors
/// Returns `(message, exit_code)` on usage or runtime failure.
pub fn run_admin_subcommand(
    subcmd: &str,
    rest: &[String],
    home: &Path,
    state_dir: &Path,
    current_exe: &Path,
    supervisor: &dyn ServiceSupervisor,
) -> Result<String, (String, u8)> {
    if !rest.is_empty() {
        return Err((
            format!(
                "aterm node {subcmd}: unexpected argument {:?}\n{NODE_USAGE}",
                rest[0]
            ),
            2,
        ));
    }
    match subcmd {
        "status" => {
            let st =
                state_with(home, supervisor).map_err(|e| (format!("aterm node status: {e}"), 1))?;
            let lock_held = {
                #[cfg(unix)]
                {
                    node_job_holds_lock(state_dir)
                }
                #[cfg(not(unix))]
                {
                    let _ = state_dir;
                    false
                }
            };
            let off = node_turned_off(state_dir);
            let mut out = match st {
                NodeJobState::Absent => format!("{NODE_LABEL}: not installed\n"),
                NodeJobState::Installed {
                    pid: Some(pid),
                    unit,
                    program,
                } => format!(
                    "{NODE_LABEL}: running (pid {pid})\n  unit: {}\n  program: {}\n",
                    unit.display(),
                    program.map_or_else(|| "-".to_owned(), |p| p.display().to_string()),
                ),
                NodeJobState::Installed {
                    pid: None,
                    unit,
                    program,
                } => format!(
                    "{NODE_LABEL}: installed, not running\n  unit: {}\n  program: {}\n",
                    unit.display(),
                    program.map_or_else(|| "-".to_owned(), |p| p.display().to_string()),
                ),
            };
            let _ = std::fmt::Write::write_fmt(
                &mut out,
                format_args!(
                    "  lock: {}\n  sync: {}\n",
                    if lock_held { "held" } else { "free" },
                    if off { "off (node-off)" } else { "on" }
                ),
            );
            Ok(out)
        }
        "on" => {
            let unit = install_with(current_exe, home, state_dir, true, supervisor)
                .map_err(|e| (format!("aterm node on: {e}"), 1))?;
            Ok(format!(
                "{NODE_LABEL}: installed and started ({})\n",
                unit.display()
            ))
        }
        "off" => {
            let removed = remove_with(home, state_dir, true, supervisor)
                .map_err(|e| (format!("aterm node off: {e}"), 1))?;
            let off = off_path(state_dir);
            Ok(match removed {
                Removal::Removed(unit) => {
                    format!("{NODE_LABEL}: stopped and removed ({})\n", unit.display())
                }
                Removal::Nothing => format!("{NODE_LABEL}: off (recorded in {})\n", off.display()),
                Removal::Kept { unit, why } => format!(
                    "{NODE_LABEL}: off (recorded in {}); {} was left alone, and no job was \
                     stopped: {why}\n",
                    off.display(),
                    unit.display()
                ),
            })
        }
        other => Err((
            format!("aterm node: unknown command {other:?}\n{NODE_USAGE}"),
            2,
        )),
    }
}

#[cfg(test)]
#[path = "node_service_tests.rs"]
mod tests;
