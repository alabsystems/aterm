// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FLEET LISTENER (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5), and beside each
//! generation that listens the LAN toolset sharing it carries ([`share_units`]).

#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::net::{IpAddr, SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_net::fleet::{FleetEvent, FleetOp, FleetScope};
use aterm_types::MutexExt as _;

use crate::app_config::FleetConfig;

/// How often a bind refused `EADDRINUSE` or `EADDRNOTAVAIL`, or a key that cannot be used, is
/// tried again, and how often a served key is read again.
pub(crate) const RETRY: Duration = aterm_net::fleet::KEY_READ_EVERY;

const RETRY_SEAM: &str = "ATERM_DEBUG_FLEET_RETRY_MS";

const LIFETIME_SEAM: &str = "ATERM_DEBUG_FLEET_LIFETIME_MS";

const HEARTBEAT_SEAM: &str = "ATERM_DEBUG_FLEET_HEARTBEAT_MS";

/// How long one background heartbeat round takes at most: each member it dials, and then the
/// adverts, gets a share of it ([`aterm_net::fleet_member::heartbeat_members`]).
#[cfg(unix)]
const HEARTBEAT_ROUND: Duration = Duration::from_secs(10);

/// The dev seam that sets, in milliseconds, [`heartbeat_within`] for a test of the binary.
const HEARTBEAT_WITHIN_SEAM: &str = "ATERM_DEBUG_FLEET_HEARTBEAT_WITHIN_MS";

/// The round [`spawn_member_heartbeat_at`] gives its dials: [`HEARTBEAT_ROUND`], or what
/// [`HEARTBEAT_WITHIN_SEAM`] names. A test's members are its own (a unit test's in-process node
/// jobs, a test world's aterms on this machine, and the places either test holds), which answer
/// or hang up however loaded the machine is, and a round that misses one waits
/// [`heartbeat_every`] for the next, so each dial's share of a unit test's round waits the hang
/// detector, as a test world's does under the seam.
#[cfg(unix)]
fn heartbeat_within() -> Duration {
    #[cfg(not(test))]
    const ROUND: Duration = HEARTBEAT_ROUND;
    #[cfg(test)]
    const ROUND: Duration = aterm_spec::wait::MUST_HAPPEN
        .saturating_mul(aterm_net::fleet_member::HEARTBEAT_MEMBERS as u32 + 1);
    static WITHIN: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *WITHIN.get_or_init(|| seam_or(HEARTBEAT_WITHIN_SEAM, ROUND))
}

/// Timeout for a facts push to the issuer this Mac's book names. A unit test's issuer is its own
/// in-process server, which answers, so its push waits the hang detector.
#[cfg(not(test))]
const FACTS_PUSH_WITHIN: Duration = Duration::from_secs(5);
#[cfg(test)]
const FACTS_PUSH_WITHIN: Duration = aterm_spec::wait::MUST_HAPPEN;

/// The milliseconds the dev seam `seam` names, else `default`.
pub(crate) fn seam_or(seam: &str, default: Duration) -> Duration {
    aterm_types::dev_seam!(seam)
        .and_then(|v| {
            let s = v.to_str()?.trim();
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse::<u64>().ok())
                .flatten()
        })
        .filter(|&ms| ms > 0)
        .map_or(default, Duration::from_millis)
}

fn retry() -> Duration {
    static EVERY: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *EVERY.get_or_init(|| seam_or(RETRY_SEAM, RETRY))
}

/// How long a proved connection is served ([`aterm_net::fleet::SERVED_WITHIN`]). A unit test's
/// peers serve in process and finish their exchanges, so its connections are served to the
/// hang detector: a wait the test lifts (a node's window wake) is not cut at 30 s.
fn lifetime() -> Duration {
    #[cfg(not(test))]
    const SERVED: Duration = aterm_net::fleet::SERVED_WITHIN;
    #[cfg(test)]
    const SERVED: Duration = aterm_spec::wait::MUST_HAPPEN;
    static WITHIN: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *WITHIN.get_or_init(|| seam_or(LIFETIME_SEAM, SERVED))
}

/// How often a member's listener registers with the members that answer it (F17: three, then up
/// to three more of those marked missed).
#[cfg(unix)]
fn heartbeat_every() -> Duration {
    static EVERY: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *EVERY.get_or_init(|| seam_or(HEARTBEAT_SEAM, aterm_net::fleet_member::HEARTBEAT_EVERY))
}

/// When a member's heartbeat is due: every [`heartbeat_every`], and once when this process's
/// network moved (`crate::network_moved`, docs/DESIGN-fluid-swarm-2026-10-28.md F3c), as soon as
/// this Mac's spread after the move has passed, so a member back on the LAN registers again
/// without waiting out the interval, and ~35 Macs back at once reach the issuer apart.
#[cfg(unix)]
pub(crate) struct HeartbeatClock {
    /// When the last heartbeat was due; `None` before the first.
    last: Option<std::time::Instant>,
    every: Duration,
    moved: crate::network_moved::Watch,
}

#[cfg(unix)]
impl HeartbeatClock {
    /// A clock whose last heartbeat was `last`, due every `every` and on each move `moved` hears.
    pub(crate) const fn new(
        last: Option<std::time::Instant>,
        every: Duration,
        moved: crate::network_moved::Watch,
    ) -> Self {
        Self { last, every, moved }
    }

    /// [`Self::due`] for a caller whose last round may still be out (`busy`): while it is, this
    /// answers nothing and takes nothing, so a move told meanwhile (the settled place's, seconds
    /// after a route move began a round that finds the LAN still down) gets a round of its own
    /// once that one ends, rather than being taken and dropped by a round that cannot start.
    pub(crate) fn due_when_free(
        &mut self,
        now: std::time::Instant,
        busy: bool,
    ) -> Option<crate::network_moved::Woke> {
        if busy { None } else { self.due(now) }
    }

    /// Whether a heartbeat is due at `now`, and why: a move not yet answered and due at `now`
    /// (its source), else the interval run out ([`crate::network_moved::Woke::Timer`]). Each
    /// answers once.
    pub(crate) fn due(&mut self, now: std::time::Instant) -> Option<crate::network_moved::Woke> {
        if let Some(woke) = self.moved.take_at(now) {
            self.last = Some(now);
            return Some(woke);
        }
        let lapsed = self
            .last
            .is_none_or(|last| now.saturating_duration_since(last) >= self.every);
        lapsed.then(|| {
            self.last = Some(now);
            crate::network_moved::Woke::Timer
        })
    }
}

/// Say in aterm.log a heartbeat a move made due (`woke`); one the interval made is not said.
#[cfg(unix)]
pub(crate) fn say_heartbeat_woke(who: &str, woke: crate::network_moved::Woke) {
    if woke != crate::network_moved::Woke::Timer {
        aterm_log::info!(
            "{who}: one fleet heartbeat now: the network moved ({})",
            woke.words()
        );
    }
}

/// The most fleet connections in flight at once, handshaking or served.
pub(crate) fn fleet_slots() -> usize {
    slots_for(
        crate::control::soft_descriptor_limit(),
        crate::control::control_connections_cap(),
    )
}

fn slots_for(soft_limit: Option<u64>, control_cap: usize) -> usize {
    let by_descriptors = soft_limit.map_or(usize::MAX, |soft| {
        usize::try_from(soft / 32).unwrap_or(usize::MAX)
    });
    aterm_net::fleet::MAX_SLOTS
        .min(control_cap)
        .min(by_descriptors)
        .max(1)
}

fn launched_inside_aterm() -> bool {
    std::env::var_os(aterm_types::domain::ENV_PARENT_SESSION_ID).is_some()
        || std::env::var_os("ATERM_CHILD").is_some()
}

const SERVES: &[FleetOp] = &[
    FleetOp::Survey,
    FleetOp::Book,
    FleetOp::Join,
    FleetOp::Facts,
    FleetOp::MailCode,
    FleetOp::Census,
    FleetOp::VaultJoin,
];

const SERVES_DRIVE: &[FleetOp] = &[
    FleetOp::Survey,
    FleetOp::Book,
    FleetOp::Join,
    FleetOp::Drive,
    FleetOp::Facts,
    FleetOp::MailCode,
    FleetOp::Census,
    FleetOp::VaultJoin,
];

/// [`SERVES`] and the ops only the mesh carries that a node service serves as a connection's
/// own op: its neighbour links, toolset units and the desk (§4.4).
#[cfg(unix)]
const SERVES_MESH: &[FleetOp] = &[
    FleetOp::Survey,
    FleetOp::Book,
    FleetOp::Join,
    FleetOp::Facts,
    FleetOp::MailCode,
    FleetOp::Census,
    FleetOp::VaultJoin,
    FleetOp::Neighbour,
    FleetOp::Units,
    FleetOp::Desk,
];

/// [`SERVES_MESH`] with [`FleetOp::Drive`].
#[cfg(unix)]
const SERVES_DRIVE_MESH: &[FleetOp] = &[
    FleetOp::Survey,
    FleetOp::Book,
    FleetOp::Join,
    FleetOp::Drive,
    FleetOp::Facts,
    FleetOp::MailCode,
    FleetOp::Census,
    FleetOp::VaultJoin,
    FleetOp::Neighbour,
    FleetOp::Units,
    FleetOp::Desk,
];

/// C16 3-minute heartbeat interval for pushing `survey facts` to the fleet issuer.
#[cfg(unix)]
const FACTS_HEARTBEAT_EVERY: Duration = Duration::from_secs(180);

/// C24 5-minute refresh interval for recomputing this Mac's own `swarm-census.json` (F17).
#[cfg(unix)]
const CENSUS_REFRESH_EVERY: Duration =
    Duration::from_secs(aterm_agent::fleet_census::CENSUS_REFRESH_S);

/// Where the listener stands, as `status`'s `fleet=` says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Standing {
    /// No `[fleet] listen`.
    Off,
    /// Bound and serving.
    Listening,
    /// The port is held by another process, or the address is not this machine's yet.
    Retrying,
    /// Not listening, for the named reason (`status`'s help lists them).
    Refused(&'static str),
}

impl Standing {
    fn word(self) -> String {
        match self {
            Self::Off => "off".to_owned(),
            Self::Listening => "listening".to_owned(),
            Self::Retrying => "retrying".to_owned(),
            Self::Refused(why) => format!("refused:{why}"),
        }
    }
}

/// The `[fleet]` table as the listener reads it: absent, or a blank `listen`, is no listener.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Wanted {
    listen: String,
    allow: Option<String>,
    /// `[fleet] drive_from`'s grants.
    drive_from: Vec<aterm_net::fleet::DriveGrant>,
}

fn fleet_wanted(fleet: Option<&FleetConfig>) -> Option<Wanted> {
    let fleet = fleet?;
    let listen = fleet
        .listen
        .as_deref()
        .filter(|listen| !listen.is_empty())?;
    Some(Wanted {
        listen: listen.to_owned(),
        allow: fleet.allow.clone(),
        drive_from: fleet
            .drive_from
            .iter()
            .flatten()
            .filter_map(|word| aterm_net::fleet::DriveGrant::from_word(word))
            .collect(),
    })
}

fn level_of(allow: Option<&str>) -> Option<(FleetScope, bool)> {
    match allow.map(str::to_ascii_lowercase).as_deref() {
        None => Some((FleetScope::Facts, false)),
        Some(aterm_net::fleet::DRIVE_LEVEL) => Some((FleetScope::Content, true)),
        Some(word) => FleetScope::from_word(word).map(|scope| (scope, false)),
    }
}

/// The `[fleet]` table `text` names, `Some(None)` where it names no listener: `None` when `text`
/// is no TOML or its `[fleet]` does not read.
fn table_in(text: &str) -> Option<Option<Wanted>> {
    #[derive(serde::Deserialize)]
    struct Fleet {
        fleet: Option<FleetConfig>,
        swarm: Option<crate::app_config::SwarmConfig>,
    }
    let config = aterm_toml::from_str::<Fleet>(text).ok()?;
    let enabled = config.swarm.and_then(|s| s.enabled).unwrap_or(true);
    Some(if enabled {
        fleet_wanted(config.fleet.as_ref())
    } else {
        None
    })
}

/// The grants `wanted` serves: its `drive_from` at the drive level, none at any other.
fn grants_of(wanted: Option<&Wanted>) -> Vec<aterm_net::fleet::DriveGrant> {
    wanted
        .filter(|wanted| level_of(wanted.allow.as_deref()).is_some_and(|(_, drive)| drive))
        .map(|wanted| wanted.drive_from.clone())
        .unwrap_or_default()
}

/// What a listener serves each connection it accepts: the level it proves, and the grants that
/// admit a drive.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Serving {
    scope: FleetScope,
    grants: Vec<aterm_net::fleet::DriveGrant>,
}

impl Serving {
    /// The ops it serves: a drive only while at least one grant exists.
    fn ops(&self) -> &'static [FleetOp] {
        if self.grants.is_empty() {
            SERVES
        } else {
            SERVES_DRIVE
        }
    }

    /// The ops a node service serves, the mesh's own too where `mesh` is on ([`SERVES_MESH`]).
    #[cfg(unix)]
    fn ops_meshed(&self, mesh: bool) -> &'static [FleetOp] {
        match (mesh, self.grants.is_empty()) {
            (false, _) => self.ops(),
            (true, true) => SERVES_MESH,
            (true, false) => SERVES_DRIVE_MESH,
        }
    }

    /// The line that says a listener at `addr` serves it.
    fn line(&self, addr: SocketAddr) -> String {
        if self.grants.is_empty() {
            format!("fleet listening on {addr} (serving {})", self.scope.word())
        } else if self.grants.iter().any(|grant| grant.all()) {
            format!(
                "fleet listening on {addr} (serving {}; every fleet member may drive it)",
                self.scope.word()
            )
        } else {
            format!(
                "fleet listening on {addr} (serving {}; {} node(s) may drive it)",
                self.scope.word(),
                self.grants.len()
            )
        }
    }

    /// Whether it still serves a connection served at content (`content`), or a drive of the node
    /// and key `driver` names: one this Mac still holds under that key (`holds`, [`holds_in`]),
    /// which a grant still admits under it ([`still_drives`]), so `*` and a named grant cut it
    /// alike.
    fn serves(
        &self,
        content: bool,
        driver: Option<&aterm_net::fleet::DriveGrant>,
        holds: &dyn Fn(&str, aterm_net::fleet::NodeKey) -> bool,
        denied_nodes: &[String],
    ) -> bool {
        (!content || self.scope == FleetScope::Content)
            && driver.is_none_or(|driver| {
                still_drives(
                    &self.grants,
                    || holds(&driver.node, driver.key).then_some(driver.key),
                    &driver.node,
                    denied_nodes,
                )
            })
    }
}

/// What a listener running `running` serves as aterm.toml reads `now`: the level and grants its
/// `[fleet]` names on disk, so one written there applies to the next connection whether or not the
/// reload that reads it was taken, else the running table's while aterm.toml is unread or its
/// `[fleet]` does not read, as the running settings stand then. A `[fleet]` that names no listener,
/// or a level no listener takes, grants none and keeps the running level.
fn serving_now(running: Option<&Wanted>, now: Option<&str>) -> Serving {
    let scope_of = |wanted: Option<&Wanted>| {
        wanted
            .and_then(|wanted| level_of(wanted.allow.as_deref()))
            .map(|(scope, _)| scope)
    };
    let kept = scope_of(running).unwrap_or(FleetScope::Facts);
    match now.and_then(table_in) {
        None => Serving {
            scope: kept,
            grants: grants_of(running),
        },
        Some(named) => Serving {
            scope: scope_of(named.as_ref()).unwrap_or(kept),
            grants: grants_of(named.as_ref()),
        },
    }
}

/// What this Mac's listener serves now ([`serving_now`] of the running table and aterm.toml as it
/// is on disk), its grants those that admit now ([`window_grants`]).
#[cfg(unix)]
fn served_now() -> Serving {
    let running = FLEET_DESK.lock_or_recover().wanted.clone();
    let serving = serving_now(running.as_ref(), config_now().as_deref());
    Serving {
        grants: window_grants(serving.grants),
        ..serving
    }
}

/// The grants among `grants` that admit now in this window's fleet state: [`grants_under`] of
/// the state directory and of the vault this process reads ([`vault_here`]). Every reader of the
/// window's grants reads them so: each connection's serving and each lift of a hold. Where none
/// admits because the vault cannot be found, the journal says so once ([`say_no_grant_admits`]).
#[cfg(unix)]
fn window_grants(grants: Vec<aterm_net::fleet::DriveGrant>) -> Vec<aterm_net::fleet::DriveGrant> {
    let vault = vault_here();
    let (admitted, unfound) = grants_under(
        grants,
        aterm_types::dirs::state_dir().as_deref(),
        vault.as_ref().map(Option::as_ref),
    );
    if let Some(why) = unfound {
        say_no_grant_admits(&why);
    }
    admitted
}

/// Journal, once in this process, that no drive grant admits a Mac because its vault cannot be
/// found (`why`, [`vault_under`]'s error): a record, not an alert. The deny list it cannot read
/// says the rest, that every Mac that dials this one is turned away ([`denied_nodes_now`]).
#[cfg(unix)]
fn say_no_grant_admits(why: &str) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        note_fleet(
            &format!(
                "fleet: no drive grant in aterm.toml lets a Mac drive this one: {}",
                aterm_log::sanitize_record(why)
            ),
            true,
        );
    });
}

/// The grants among `grants` that admit beside the fleet state at `state` under `vault`, the
/// vault this process reads ([`vault_under`]), and why none does where that vault cannot be found
/// (`Some` only where `grants` held one): none where a run on the account's own state cannot find
/// its vault (`Err`), as the node job then serves none, and none while the state was taken under
/// another vault than the one `vault` configures ([`in_effect`]); with no state directory, which
/// holds no fleet key, as they are.
#[cfg(unix)]
fn grants_under(
    grants: Vec<aterm_net::fleet::DriveGrant>,
    state: Option<&std::path::Path>,
    vault: Result<Option<&aterm_secrets::client::Paths>, &String>,
) -> (Vec<aterm_net::fleet::DriveGrant>, Option<String>) {
    match (state, vault) {
        (_, Err(why)) => (Vec::new(), (!grants.is_empty()).then(|| why.clone())),
        (Some(state), Ok(vault)) => (in_effect(grants, state, vault), None),
        (None, Ok(_)) => (grants, None),
    }
}

/// The grants among `grants` that admit now in the fleet state at `state`, under the vault
/// `vault` (none: this process reads no vault): all of them where that fleet state was taken under
/// the vault configured, none where under another or none
/// ([`aterm_agent::fleet_vault::grants_in_effect`]), as the window's listener and the node job's
/// read them alike.
#[cfg(unix)]
pub(crate) fn in_effect(
    grants: Vec<aterm_net::fleet::DriveGrant>,
    state: &std::path::Path,
    vault: Option<&aterm_secrets::client::Paths>,
) -> Vec<aterm_net::fleet::DriveGrant> {
    aterm_agent::fleet_vault::grants_in_effect(state, vault, grants)
}

/// aterm.toml's text as it is on disk now, a missing one's empty: `None` when it cannot be read.
#[cfg(unix)]
fn config_now() -> Option<String> {
    let path = crate::app_config::config_path()?;
    crate::native_config_service::VersionedConfigService::observe_path(&path, true)
        .ok()
        .map(|observation| observation.text)
}

/// Cached authoritative swarm `deny_nodes`, retained across transient vault lock / I/O states.
#[cfg(unix)]
static DENIED_NODES: Mutex<DeniedCache> = Mutex::new(DeniedCache::new());

/// Why a listener that has never read its deny list refuses a peer it would otherwise admit:
/// a refusal to try again, never a verdict on that peer.
pub(crate) const DENY_LIST_UNREAD: &str =
    "this Mac has not read its deny list yet; try again once it has";

/// The last deny list a listener read, and the unavailable observation last recorded.
#[cfg(unix)]
pub(crate) struct DeniedCache {
    list: Option<Arc<[String]>>,
    /// A new cause is recorded once, without changing the retained admission verdict.
    unavailable_said: Option<DeniedCause>,
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum DeniedCause {
    Unavailable(aterm_secrets::client::SwarmDenialUnavailable),
    Read(String),
}

#[cfg(unix)]
impl std::fmt::Display for DeniedCause {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(out, "{reason}"),
            Self::Read(reason) => out.write_str(reason),
        }
    }
}

#[cfg(unix)]
impl DeniedCache {
    pub(crate) const fn new() -> Self {
        Self {
            list: None,
            unavailable_said: None,
        }
    }
}

/// The deny list after `observed`, a read of it, and the line to say now, if any: a list read now
/// is kept in `cache` and answered; an unavailable or failed read answers
/// the last list `cache` kept, and `None` while it has kept none, so the caller refuses whom that
/// unread list might name ([`DENY_LIST_UNREAD`]) rather than read it as naming no one.
///
/// A changed unavailable cause is recorded once, including while retaining a list. Recovery is
/// recorded once too. These journal observations never change the admission verdict.
#[cfg(unix)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetDeniedCache",
        action = "Known",
        reason = "Model setup chooses the previous verified deny-list state; the conformance harness reconstructs it before the production Observe transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetDeniedCache",
        action = "Cause",
        reason = "Model setup chooses the previous recorded diagnostic; the conformance harness reconstructs it before the production Observe transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetDeniedCache",
        action = "Input",
        reason = "The vault supplies the next observation; the conformance harness ranges over the bounded input classes before calling production Observe"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetDeniedCache",
        action = "Observe",
        project = "fleet_listen::denial_cache_model_tests::project"
    )
)]
fn update_denied_nodes_cache(
    cache: &mut DeniedCache,
    observed: aterm_secrets::Result<aterm_secrets::client::SwarmDenialSnapshot>,
) -> (Option<Arc<[String]>>, Option<String>) {
    use aterm_secrets::client::SwarmDenialSnapshot;

    let cause = match observed {
        Ok(SwarmDenialSnapshot::Available(nodes)) => {
            let retained = cache.list.is_some();
            let arc: Arc<[String]> = nodes.into();
            cache.list = Some(Arc::clone(&arc));
            let said = cache.unavailable_said.take().map(|_| {
                if retained {
                    "fleet: the deny list is current again"
                } else {
                    "fleet: the deny list is read; Macs that prove the fleet secret are admitted again"
                }
                .to_owned()
            });
            return (Some(arc), said);
        }
        Ok(SwarmDenialSnapshot::Unavailable(reason)) => DeniedCause::Unavailable(reason),
        Err(error) => DeniedCause::Read(aterm_codec::tty::one_line(&error.to_string(), 1024)),
    };
    let denied = cache.list.clone();
    if cache.unavailable_said.as_ref() == Some(&cause) {
        return (denied, None);
    }
    let line = if denied.is_some() {
        format!("fleet: retaining the last verified deny list: {cause}")
    } else {
        format!(
            "fleet: every Mac that dials this one is turned away, and none is dialed: {DENY_LIST_UNREAD} ({cause})"
        )
    };
    cache.unavailable_said = Some(cause);
    (denied, Some(line))
}

#[cfg(all(test, unix))]
#[path = "fleet_denial_model_tests.rs"]
mod denial_cache_model_tests;

/// Read the verified swarm roster's `deny_nodes`, retaining the last authoritative list when the
/// local vault is mid-apply or unreadable, and `None` until one was read
/// ([`update_denied_nodes_cache`]). It is read again only when one of the vault files it reads
/// changed ([`aterm_secrets::client::swarm_denial_snapshot_now`]), so the listener's per-dialer
/// lookup and each control request cost no vault decrypt. A process that reads no vault (one
/// under a private state root, [`vault_here`]) has a deny list that names no one; one on the
/// account's own state whose vault cannot be found has none, and says why.
///
/// The window's per-connection checks ([`admit_driver`], [`register_member`], [`share_book`],
/// [`receive_pushed_facts`], `served_live`, the vault-join judge) refuse while this answers `None`
/// too, but they run
/// only past the handshake gate ([`denied_under`]), which refuses every proved dialer while it
/// does, and once a list is read this never answers `None` again: theirs is a second line, which
/// no test reaches (a test reads no vault). The gate and the cache are what the tests hold
/// (`denied_nodes_cache_retains_list_while_locked_and_cuts_open_drive`, and the node job's
/// `a_node_job_that_has_not_read_its_deny_list_refuses_a_proven_peer_until_it_has`).
#[cfg(unix)]
fn denied_nodes_now() -> Option<Arc<[String]>> {
    let observed = match vault_here() {
        Ok(None) => return Some(Arc::from(&[][..])),
        Ok(Some(paths)) => aterm_secrets::client::swarm_denial_snapshot_now(&paths),
        Err(why) => Err(aterm_secrets::Error::new(why)),
    };
    let (denied, said) = update_denied_nodes_cache(&mut DENIED_NODES.lock_or_recover(), observed);
    if let Some(line) = said {
        note_fleet(&line, denied.is_none());
    }
    denied
}

/// Whether `node` may still drive this Mac under `grants`: a grant admits it under the key `bound`
/// (this Mac's keyring) binds it to ([`aterm_net::fleet::DriveGrant::admitting`]).
fn still_drives(
    grants: &[aterm_net::fleet::DriveGrant],
    bound: impl FnOnce() -> Option<aterm_net::fleet::NodeKey>,
    node: &str,
    denied_nodes: &[String],
) -> bool {
    !aterm_net::fleet::is_denied_node(node, denied_nodes)
        && grants.iter().any(|grant| grant.all() || grant.node == node)
        && bound().is_some_and(|key| {
            !aterm_net::fleet::is_denied_key(&key, denied_nodes)
                && aterm_net::fleet::DriveGrant::admitting(grants, node, key).is_some()
        })
}

/// The membership log's admission in this window's process (`docs/DESIGN-fluid-swarm-2026-10-28.md`
/// §4.1, F17: [`aterm_agent::fleet_log::LocalLog`] of the vault [`vault_here`] reads): none in a
/// process that reads no vault (under a private state root) or cannot find it, or while this Mac
/// holds no log. While the store cannot be read it is the verdict last read under the vault
/// configured now (the same vault, its configuration rewritten or not), and with none, one that
/// admits no key.
#[cfg(unix)]
pub(crate) fn window_log() -> Option<Arc<aterm_net::fleet_member::MemberKeys>> {
    static LOG: std::sync::OnceLock<Option<aterm_agent::fleet_log::LocalLog>> =
        std::sync::OnceLock::new();
    LOG.get_or_init(|| {
        vault_here()
            .ok()
            .flatten()
            .as_ref()
            .map(aterm_agent::fleet_log::LocalLog::of)
    })
    .as_ref()?
    .keys()
}

/// THE RULE for which vault this process's fleet readers read, the window's and the node job's
/// alike, and LAN toolset sharing's downloads (`aterm`'s `unit_peers`): the deny list, the
/// membership log, the vault the fleet's grants are of ([`aterm_agent::fleet_vault`]) and the
/// vault a `vault-join` is judged by and served on ([`vault_join_on`]). It is decided by where the
/// state root, which holds the fleet's key, resolved (`private_root`): under a private one none,
/// as that fleet state is no account's; under the account's own, the vault `live` finds,
/// whatever else the environment says (`CI`, or an `ATERM_STATE_HOME` a release build does not
/// read), as that fleet's key is the account's and so must be its deny list and its grants'
/// vault.
///
/// # Errors
/// A run on the account's own state that cannot find its vault (no home directory): its
/// listener admits by no grant ([`grants_under`]) and turns every dialer away
/// ([`denied_nodes_now`]), the node job does not serve, and a download dials no member, rather
/// than serve or dial as though the deny list named no one.
#[cfg(unix)]
pub(crate) fn vault_under(
    private_root: bool,
    live: impl FnOnce() -> aterm_secrets::Result<aterm_secrets::client::Paths>,
) -> Result<Option<aterm_secrets::client::Paths>, String> {
    if private_root {
        return Ok(None);
    }
    live()
        .map(Some)
        .map_err(|error| format!("this Mac's vault cannot be found ({error})"))
}

/// Whether this process's state root resolved to a private one, [`vault_under`]'s
/// `private_root` ([`aterm_types::dirs::runs_under_private_state_root`]): a lab instance's
/// `ATERM_STATE_HOME` in a build that reads that seam, or a test binary's scratch root, so no
/// test reads the person's vault. Neither `CI` nor an `ATERM_STATE_HOME` a release build does not
/// read makes one: such a run keeps the account's own state. A unit test of this crate counts as
/// one even where its scratch root could not be made; that also hides this predicate from this
/// crate's own tests, so `aterm`'s, which build this crate as a library, hold it
/// (`unit_peers`' `a_download_reads_the_vault_its_state_root_resolved_to`).
#[cfg(unix)]
pub(crate) fn state_root_is_private() -> bool {
    cfg!(test) || aterm_types::dirs::runs_under_private_state_root()
}

/// [`vault_under`] of where this process's state root resolved ([`state_root_is_private`]) and
/// of `live`, the read of this user's vault: [`vault_here`], with that read a test's.
///
/// # Errors
/// As [`vault_under`]: a run on the account's own state that cannot find its vault.
#[cfg(unix)]
pub fn vault_reading(
    live: impl FnOnce() -> aterm_secrets::Result<aterm_secrets::client::Paths>,
) -> Result<Option<aterm_secrets::client::Paths>, String> {
    vault_under(state_root_is_private(), live)
}

/// The vault this process reads ([`vault_reading`] of this user's vault). The fleet readers of
/// the vault the rule names read it here: the window's deny list ([`denied_nodes_now`]), its
/// membership log ([`window_log`]), its grants ([`window_grants`]), its heartbeat's log write and
/// vault record, its `vault-join` ([`vault_join_on`]), the node job's vault, and `aterm`'s LAN
/// toolset downloads. The members desk's own exchanges (`crate::members_desk`'s `ask`) read the
/// user's vault directly: they are the vault's desk, not a reader of the fleet's.
///
/// # Errors
/// As [`vault_under`]: a run on the account's own state that cannot find its vault.
#[cfg(unix)]
pub fn vault_here() -> Result<Option<aterm_secrets::client::Paths>, String> {
    vault_reading(aterm_secrets::client::Paths::live)
}

/// The key `keys` binds `node` to, unless the membership log `log` holds that key out of the
/// fleet: a removed Mac's key binds no drive, whatever a book says.
#[cfg(unix)]
pub(crate) fn bound_in(
    keys: &aterm_net::fleet_member::Keyring,
    log: Option<&aterm_net::fleet_member::MemberKeys>,
    node: &str,
) -> Option<aterm_net::fleet::NodeKey> {
    keys.bound(node)
        .filter(|key| !log.is_some_and(|log| log.holds_out(key)))
}

/// The key this Mac holds `node` to now: the key `admitted` records from its drive admission while
/// [`holds_in`] still holds `node` under it, else [`bound_in`]. A log-admitted driver with no book
/// entry here keeps its remote hold while the log admits its key and loses it once the log takes
/// that key out.
#[cfg(unix)]
pub(crate) fn bound_or_admitted_in(
    keys: &aterm_net::fleet_member::Keyring,
    log: Option<&aterm_net::fleet_member::MemberKeys>,
    admitted: Option<aterm_net::fleet::NodeKey>,
    node: &str,
) -> Option<aterm_net::fleet::NodeKey> {
    admitted
        .filter(|key| holds_in(keys, log, node, *key))
        .or_else(|| bound_in(keys, log, node))
}

#[cfg(unix)]
static ADMITTED_DRIVERS: Mutex<std::collections::BTreeMap<String, aterm_net::fleet::NodeKey>> =
    Mutex::new(std::collections::BTreeMap::new());

/// Record that `node` proved `key` when a drive connection was admitted here.
#[cfg(unix)]
pub(crate) fn note_admitted_driver(node: &str, key: aterm_net::fleet::NodeKey) {
    ADMITTED_DRIVERS
        .lock_or_recover()
        .insert(node.to_owned(), key);
}

#[cfg(unix)]
fn admitted_driver_key(node: &str) -> Option<aterm_net::fleet::NodeKey> {
    ADMITTED_DRIVERS.lock_or_recover().get(node).copied()
}

/// Whether this Mac still holds `node` under `key`, as [`aterm_net::fleet_member::proven_driver`]
/// admits a drive: the membership log `log` admits a member under `key` and binds here no other
/// key it admits to `node`, or `keys` binds `node` to `key` ([`bound_in`]). A drive the log admits
/// needs no book entry here, as its admission did not.
#[cfg(unix)]
pub(crate) fn holds_in(
    keys: &aterm_net::fleet_member::Keyring,
    log: Option<&aterm_net::fleet_member::MemberKeys>,
    node: &str,
    key: aterm_net::fleet::NodeKey,
) -> bool {
    match log {
        Some(log) if log.admits(&key) => keys
            .bound(node)
            .is_none_or(|bound| bound == key || !log.admits(&bound)),
        _ => bound_in(keys, log, node) == Some(key),
    }
}

/// What the listener's handshake refuses now ([`aterm_net::fleet::Denied::under`]): the swarm
/// roster's deny-listed nodes (`denied_nodes`), and each node key the membership log `log` holds
/// out; every dialer while no deny list was read yet (`None`) or the log exists but gave no verdict
/// yet ([`aterm_net::fleet_member::MemberKeys::unreadable`]).
#[cfg(unix)]
pub(crate) fn denied_under(
    denied_nodes: Option<&[String]>,
    log: Option<&aterm_net::fleet_member::MemberKeys>,
) -> Arc<aterm_net::fleet::Denied> {
    Arc::new(aterm_net::fleet::Denied::under(denied_nodes, log))
}

/// Hand this process's fleet dialers the verdict this Mac's listener judges each dialer by
/// ([`denied_under`] of the swarm's deny list and [`window_log`], read for each dial), so with the
/// mesh switch on both ends of this Mac judge a peer's key by one standing
/// (`aterm_net::fleet_member::judge_mesh_by`). Inert with the switch off.
#[cfg(unix)]
pub fn judge_mesh_dials() {
    aterm_net::fleet_member::judge_mesh_by(Arc::new(|| {
        denied_under(denied_nodes_now().as_deref(), window_log().as_deref())
    }));
}

/// The key this Mac holds a node to now ([`bound_or_admitted_in`] of its keyring, [`window_log`]
/// and [`admitted_driver_key`]), keyring and log read once at its first call.
#[cfg(unix)]
fn keyring_now() -> impl Fn(&str) -> Option<aterm_net::fleet::NodeKey> {
    let held = std::cell::OnceCell::new();
    move |node| {
        let (keys, log) = held.get_or_init(|| {
            (
                aterm_types::dirs::state_dir()
                    .map(|state| aterm_net::fleet_member::Keyring::read(&state))
                    .unwrap_or_default(),
                window_log(),
            )
        });
        bound_or_admitted_in(keys, log.as_deref(), admitted_driver_key(node), node)
    }
}

/// Whether this Mac holds a node under a key now ([`holds_in`] of its keyring and [`window_log`]),
/// both read once, at its first call.
#[cfg(unix)]
fn holds_now() -> impl Fn(&str, aterm_net::fleet::NodeKey) -> bool {
    let held = std::cell::OnceCell::new();
    move |node, key| {
        let (keys, log) = held.get_or_init(|| {
            (
                aterm_types::dirs::state_dir()
                    .map(|state| aterm_net::fleet_member::Keyring::read(&state))
                    .unwrap_or_default(),
                window_log(),
            )
        });
        holds_in(keys, log.as_deref(), node, key)
    }
}

/// Lift every remote hold of a node `grants` lets drive no more ([`still_drives`] under the keys
/// `bound` gives), as its own `hold off` would: it can no longer lift it.
#[cfg(unix)]
fn lift_undriving_holds(
    grants: &[aterm_net::fleet::DriveGrant],
    bound: &dyn Fn(&str) -> Option<aterm_net::fleet::NodeKey>,
    denied_nodes: &[String],
) {
    let lifted = crate::control::lift_ungranted_remote_holds(&|node| {
        still_drives(grants, || bound(node), node, denied_nodes)
    });
    if lifted > 0 {
        note_fleet(
            &format!("fleet: {lifted} remote hold(s) lifted: their nodes may drive no more"),
            false,
        );
    }
}

/// How long a local `hold off` waits for a remote hold's node to prove itself.
const HOLDER_WITHIN: Duration = Duration::from_secs(3);

#[cfg(unix)]
const HOLDER_SEAM: &str = "ATERM_DEBUG_FLEET_HOLDER_WITHIN_MS";

/// [`HOLDER_WITHIN`], or the dev seam [`HOLDER_SEAM`]'s milliseconds: a test's hang detector for
/// a node that listens on the same machine, and so must prove itself, however loaded it is.
#[cfg(unix)]
fn holder_within() -> Duration {
    seam_or(HOLDER_SEAM, HOLDER_WITHIN)
}

/// What this Mac finds of `hold`'s node now, for a local `hold off`: no grant here admitting it,
/// or its place in the book disowning it under the key this Mac's keyring binds it to within
/// [`HOLDER_WITHIN`] ([`aterm_net::fleet_member::Unreached::disowns`]), ends it; a place that does
/// not answer never ([`holder_after`]).
#[cfg(unix)]
fn remote_holder(hold: &crate::fabric::RemoteHold) -> crate::fabric::Holder {
    use crate::fabric::Holder;
    use aterm_net::fleet_member::{Keyring, Target, peer_of, proves_itself, target_of};
    let node = hold.node.as_str();
    let ready = (|| {
        let state = aterm_types::dirs::state_dir().ok_or("this Mac has no state directory")?;
        let keys = Keyring::read(&state);
        let grants = served_now().grants;
        let log = window_log();
        let key = bound_or_admitted_in(&keys, log.as_deref(), admitted_driver_key(node), node)
            .filter(|key| aterm_net::fleet::DriveGrant::admitting(&grants, node, *key).is_some())
            .ok_or("it may drive this Mac no more")?;
        let secret = read_fleet_key()
            .map_err(|_| "this Mac holds no fleet key it can use, so no node drives it")?;
        let me = read_node_key().ok_or("this Mac proves no node, so no node drives it")?;
        Ok((state, keys, key, secret, me))
    })();
    holder_after(
        node,
        ready,
        |(_, _, key, _, _)| *key,
        denied_nodes_now,
        |(state, keys, key, secret, me)| {
            let Some(peer) = peer_of(&state, node) else {
                return Holder::Again("no book here records where it is".to_owned());
            };
            let target = Target {
                node: Some(node.to_owned()),
                key: Some(key),
                ..target_of(&peer)
            };
            match proves_itself(&target, &keys, (&secret, &me), holder_within()) {
                Ok(_) => Holder::Drives,
                Err(why) if why.disowns() => Holder::Gone(why.to_string()),
                Err(why) => Holder::Again(why.to_string()),
            }
        },
    )
}

/// [`remote_holder`]'s verdict once it has read what it can without the deny list: `ready`, what
/// a dial of `node` needs (its key `key_of` names, under a grant here that admits it), or why the
/// node may drive this Mac no more, which ends the hold whatever the deny list says. Only then is
/// the deny list read (`denied`): while none was ever read the hold stands, to try again, and a
/// node or key it names ends it; otherwise `dial` judges.
#[cfg(unix)]
fn holder_after<T>(
    node: &str,
    ready: Result<T, &'static str>,
    key_of: impl FnOnce(&T) -> aterm_net::fleet::NodeKey,
    denied: impl FnOnce() -> Option<Arc<[String]>>,
    dial: impl FnOnce(T) -> crate::fabric::Holder,
) -> crate::fabric::Holder {
    use crate::fabric::Holder;
    let ready = match ready {
        Ok(ready) => ready,
        Err(why) => return Holder::Gone(why.to_owned()),
    };
    let Some(denied) = denied() else {
        return Holder::Again(DENY_LIST_UNREAD.to_owned());
    };
    if aterm_net::fleet::is_denied_node(node, &denied)
        || aterm_net::fleet::is_denied_key(&key_of(&ready), &denied)
    {
        return Holder::Gone("it is on the swarm roster's deny-list".to_owned());
    }
    dial(ready)
}

/// A local `hold <sid> off` that meets a remote hold alone: it lifts it once that node may drive
/// this Mac no more ([`remote_holder`]), else the refusal naming what lifts it; `None` where it
/// meets no such hold.
#[cfg(unix)]
pub(crate) fn local_off_of_remote(
    store: &crate::session_store::Store,
    sid: &str,
    wake: &dyn Fn(&aterm_session::SessionId),
) -> Option<String> {
    match crate::fabric::owner_off_remote(store, sid, &remote_holder, wake)? {
        crate::fabric::RemoteOff::Lifted { hold, why, reply } => {
            note_fleet(
                &format!(
                    "fleet: the remote hold of {} on {sid} lifted by a local hold off: that node \
                     may drive this Mac no more ({})",
                    aterm_log::sanitize_record(&hold.host),
                    aterm_log::sanitize_record(&why)
                ),
                false,
            );
            Some(reply)
        }
        crate::fabric::RemoteOff::Stands(reply) => Some(reply),
    }
}

struct FleetDesk {
    wanted: Option<Wanted>,
    /// Moved by every change of `wanted` that moves the listener ([`in_place`]).
    generation: u64,
    /// Moved by every change of `wanted` applied where the listener stands.
    revision: u64,
    /// A reload has set `wanted`, so [`start_fleet_listener`]'s launch-time table is older.
    configured: bool,
    started: bool,
    thread_spawned: bool,
    standing: Standing,
    /// The line that said where it stands ([`noted_move`]).
    said: String,
    /// The serving generation's flag: cleared to stop it.
    stop: Option<Arc<AtomicBool>>,
    /// The mesh is on (`aterm_types::mesh_switch::MESH`, read once as the listener starts): the
    /// node service is the one listener per Mac, and a window serves no port of its own.
    mesh: bool,
}

static FLEET_DESK: Mutex<FleetDesk> = Mutex::new(FleetDesk {
    wanted: None,
    generation: 0,
    revision: 0,
    configured: false,
    started: false,
    thread_spawned: false,
    standing: Standing::Off,
    said: String::new(),
    stop: None,
    mesh: false,
});

static FLEET_CHANGED: Condvar = Condvar::new();

fn ensure_fleet_listener_thread(desk: &mut FleetDesk) -> bool {
    if desk.thread_spawned {
        return true;
    }
    match std::thread::Builder::new()
        .name("aterm-fleet-listener".to_owned())
        .spawn(supervise_fleet_listener)
    {
        Ok(_) => {
            desk.thread_spawned = true;
            true
        }
        Err(error) => {
            desk.standing = Standing::Refused("thread");
            note_fleet(
                &format!("fleet listener could not start its thread: {error}"),
                true,
            );
            false
        }
    }
}

/// Start the listener's supervisor, and the first read of `status`'s `units=` word
/// ([`units_status_word`]).
pub(crate) fn start_fleet_listener(fleet: Option<&FleetConfig>) {
    #[cfg(unix)]
    let _ = UNITS_DESK.word(
        UnitsTiming {
            wait: Duration::ZERO,
            ..UNITS_WORD
        },
        read_units_word,
    );
    let mut desk = FLEET_DESK.lock_or_recover();
    if desk.started {
        return;
    }
    desk.started = true;
    desk.mesh = aterm_types::mesh_switch::MESH;
    if !desk.configured {
        desk.wanted = fleet_wanted(fleet);
        desk.generation = desk.generation.wrapping_add(1);
    }
    if desk.wanted.is_none() {
        desk.standing = Standing::Off;
        desk.said = "fleet listener off".to_owned();
        return;
    }
    let _ = ensure_fleet_listener_thread(&mut desk);
}

/// Follow a reloaded config's `[fleet]` table: a level or a grant changed is applied where the
/// listener stands ([`in_place`]), and any other change moves it.
pub(crate) fn configure_fleet_listener(fleet: Option<&FleetConfig>) {
    let wanted = fleet_wanted(fleet);
    let mut desk = FLEET_DESK.lock_or_recover();
    desk.configured = true;
    if desk.wanted == wanted {
        return;
    }
    if in_place(desk.wanted.as_ref(), wanted.as_ref()) {
        desk.wanted = wanted;
        desk.revision = desk.revision.wrapping_add(1);
        drop(desk);
        FLEET_CHANGED.notify_all();
        return;
    }
    move_fleet(desk, wanted);
}

/// Whether a listener serving `was` serves `now` where it stands: the same place, each at a level
/// a listener takes, so only a level or a grant changed, which [`follow_serving`] applies.
fn in_place(was: Option<&Wanted>, now: Option<&Wanted>) -> bool {
    let takes = |wanted: &Wanted| level_of(wanted.allow.as_deref()).is_some();
    matches!((was, now), (Some(was), Some(now)) if was.listen == now.listen && takes(was) && takes(now))
}

/// Move the listener to a new generation serving `wanted`, the current one's connections cut.
fn move_fleet(mut desk: std::sync::MutexGuard<'_, FleetDesk>, wanted: Option<Wanted>) {
    let needs_thread = wanted.is_some();
    desk.wanted = wanted;
    desk.generation = desk.generation.wrapping_add(1);
    if let Some(stop) = desk.stop.take() {
        stop.store(false, Ordering::Release);
    }
    if needs_thread && desk.started && !ensure_fleet_listener_thread(&mut desk) {
        return;
    }
    drop(desk);
    FLEET_CHANGED.notify_all();
}

/// `status`'s `fleet=` word: `off`, `listening`, `retrying` or `refused:<why>`.
pub(crate) fn fleet_status_word() -> String {
    FLEET_DESK.lock_or_recover().standing.word()
}

/// How `status`'s `units=` word is kept ([`units_status_word`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UnitsTiming {
    /// How long a word stands before a `status` reads it again.
    every: Duration,
    /// How long after a read starts a `status` waits for it.
    wait: Duration,
    /// The oldest word a `status` answers while the read after it has not landed; an older one
    /// is answered as `-`.
    stale: Duration,
}

/// `units=`'s timing. The read is one `lstat`, one small read, a liveness check and the fleet
/// book's read, which land well inside `wait`, so a store whose file system stalls costs the
/// event loop `wait` once per read, never more.
const UNITS_WORD: UnitsTiming = UnitsTiming {
    every: Duration::from_secs(2),
    wait: Duration::from_millis(10),
    stale: Duration::from_secs(4),
};

/// `status`'s `units=` word as last read ([`units_status_word`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UnitsWord {
    word: &'static str,
    /// When `word` was read; `None` before a read landed.
    read_at: Option<Instant>,
    /// When the read that is out started; `None` while none is out.
    reading: Option<Instant>,
}

impl UnitsWord {
    const UNREAD: Self = Self {
        word: "-",
        read_at: None,
        reading: None,
    };

    /// Whether to start a read at `now`: none is out, and none has landed or the last is `every`
    /// old. A read it starts is out from `now`.
    fn ask(&mut self, now: Instant, every: Duration) -> bool {
        let due = self.reading.is_none()
            && self
                .read_at
                .is_none_or(|at| now.saturating_duration_since(at) >= every);
        if due {
            self.reading = Some(now);
        }
        due
    }

    /// How much longer, at `now`, a `status` waits for the read that is out: until `wait` after
    /// that read started. `None` while none is out or that time has passed.
    fn wait_left(&self, now: Instant, wait: Duration) -> Option<Duration> {
        let left = self
            .reading?
            .checked_add(wait)?
            .saturating_duration_since(now);
        (!left.is_zero()).then_some(left)
    }

    /// The word to answer at `now`: the last read while it is less than `stale` old, else `-`.
    fn answer(&self, now: Instant, stale: Duration) -> &'static str {
        match self.read_at {
            Some(at) if now.saturating_duration_since(at) < stale => self.word,
            _ => "-",
        }
    }

    /// A read ended at `now` with `word`, or never ran (`None`), which keeps the last word.
    const fn landed(&mut self, word: Option<&'static str>, now: Instant) {
        self.reading = None;
        if let Some(word) = word {
            self.word = word;
            self.read_at = Some(now);
        }
    }
}

/// `units=`'s word and the `status` waiting on its read ([`units_status_word`]).
struct UnitsDesk {
    held: Mutex<UnitsWord>,
    landed: Condvar,
}

impl UnitsDesk {
    const fn new() -> Self {
        Self {
            held: Mutex::new(UnitsWord::UNREAD),
            landed: Condvar::new(),
        }
    }

    /// The word to answer now. A read due at this call ([`UnitsWord::ask`] over `timing.every`)
    /// is started by `read`, which says whether it started; then a read that is out is waited for
    /// until `timing.wait` after it started ([`UnitsWord::wait_left`]), and the word is answered
    /// as [`UnitsWord::answer`] reads it against `timing.stale`.
    fn word(
        &'static self,
        timing: UnitsTiming,
        read: impl FnOnce(&'static Self) -> bool,
    ) -> &'static str {
        let due = self
            .held
            .lock_or_recover()
            .ask(Instant::now(), timing.every);
        if due && !read(self) {
            self.land(None);
        }
        let mut held = self.held.lock_or_recover();
        while let Some(left) = held.wait_left(Instant::now(), timing.wait) {
            held = self
                .landed
                .wait_timeout(held, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        held.answer(Instant::now(), timing.stale)
    }

    /// A read ended with `word` (`None`: it never ran); a `status` waiting on it answers now.
    fn land(&self, word: Option<&'static str>) {
        self.held.lock_or_recover().landed(word, Instant::now());
        self.landed.notify_all();
    }
}

static UNITS_DESK: UnitsDesk = UnitsDesk::new();

/// `status`'s `units=` word: where this Mac's LAN toolset sharing stands ([`units_word`]). The
/// event loop answers `status`, so this reads no file itself. A word read less than
/// [`UNITS_WORD`]'s `every` (2 s) ago is answered as it stands. An older one starts a read on a
/// thread of its own, which this waits for until `wait` (10 ms) after it started, and answers
/// what it read. When that read has not landed by then, the last word is answered while it is
/// less than `stale` (4 s) old, else `-`; it is `-` too until the first read lands
/// ([`start_fleet_listener`] starts one as aterm starts, waiting for none).
pub(crate) fn units_status_word() -> &'static str {
    UNITS_DESK.word(UNITS_WORD, read_units_word)
}

/// Read the `units=` word into `desk` on a thread of its own ([`units_serves`]); false where none
/// started. A unit test starts none: the store and the fleet book it would read are the person's.
fn read_units_word(desk: &'static UnitsDesk) -> bool {
    if cfg!(test) {
        return false;
    }
    crate::qos::spawn_lane_thread(
        "aterm-units-word",
        crate::qos::Role::Background,
        move || {
            desk.land(Some(units_word(units_serves())));
        },
    )
    .is_ok()
}

/// Where this Mac's LAN toolset sharing stands, as the doctor's row decides it
/// ([`atpkg::peers::serves`]: the person's off, a live server's record), for as many fleet members
/// as a server of it answers ([`unit_members`]); `None` where this build's package manager is
/// inert or no store location resolves (no home directory). A store not made yet reads as on.
fn units_serves() -> Option<atpkg::peers::Serves> {
    if !atpkg::manager_enabled() {
        return None;
    }
    let layout = atpkg::store::resolve_configured()?;
    #[cfg(unix)]
    let members = unit_members().len();
    #[cfg(not(unix))]
    let members = 0;
    Some(atpkg::peers::serves(&layout, members))
}

/// `units=`'s word for where serving stands: `serving` (a server of this Mac's store listens,
/// this aterm's beside its fleet listener or a foreground `aterm pkg serve-units`), `idle` (on,
/// with fleet members, and nothing listens now), `no-member` (on, with no fleet member to serve),
/// `off` (the person's off: `aterm pkg serve-units --off`, or the Settings ▸ Packages switch), or
/// `-` (the manager is inert, or no store location resolves: [`units_serves`]).
pub(crate) const fn units_word(serves: Option<atpkg::peers::Serves>) -> &'static str {
    use atpkg::peers::Serves;
    match serves {
        Some(Serves::Serving(_)) => "serving",
        Some(Serves::Idle) => "idle",
        Some(Serves::NoMember) => "no-member",
        Some(Serves::Off) => "off",
        None => "-",
    }
}

fn supervise_fleet_listener() {
    if !cfg!(test) {
        crate::qos::set_self(crate::qos::Role::Background);
    }
    let mut ran = 0;
    loop {
        let (generation, wanted) = {
            let mut desk = FLEET_DESK.lock_or_recover();
            while desk.generation == ran {
                desk = FLEET_CHANGED
                    .wait(desk)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            (desk.generation, desk.wanted.clone())
        };
        ran = generation;
        run_fleet_generation(generation, wanted);
    }
}

fn run_fleet_generation(generation: u64, wanted: Option<Wanted>) {
    // A lift admits no one: with no deny list read yet it lifts by the grants alone, and the
    // next sweep after a read lifts by the list too.
    #[cfg(unix)]
    lift_undriving_holds(
        &window_grants(grants_of(wanted.as_ref())),
        &keyring_now(),
        denied_nodes_now().as_deref().unwrap_or_default(),
    );
    let Some(wanted) = wanted else {
        stand_fleet(generation, Standing::Off, "fleet listener off");
        return;
    };
    if launched_inside_aterm() {
        stand_fleet(
            generation,
            Standing::Refused("nested"),
            "fleet listener not started: this aterm runs inside another, which listens for it",
        );
        return;
    }
    let Ok(addr) = wanted.listen.parse::<SocketAddr>() else {
        stand_fleet(
            generation,
            Standing::Refused("listen"),
            &format!(
                "fleet listener not started: [fleet] listen {:?} is not a numeric IP:port",
                aterm_log::sanitize_record(&wanted.listen)
            ),
        );
        return;
    };
    if let Err(refused) = aterm_net::loopback::check(aterm_net::loopback::Reach::Listen(addr)) {
        stand_fleet(
            generation,
            Standing::Refused("loopback"),
            &format!("fleet listener not started: {refused}"),
        );
        return;
    }
    if level_of(wanted.allow.as_deref()).is_none() {
        stand_fleet(
            generation,
            Standing::Refused("allow"),
            "fleet listener not started: [fleet] allow is none of \"facts\", \"content\" and \
             \"drive\"",
        );
        return;
    }
    #[cfg(unix)]
    serve_fleet_port(generation, addr);
    #[cfg(not(unix))]
    {
        let _ = addr;
        stand_fleet(
            generation,
            Standing::Refused("unsupported"),
            "fleet listener not started: this platform serves no fleet connection",
        );
    }
}

/// Whether `systems.alab.aterm-node` serves the fleet port now (or is about to bind it): its port
/// lock, never its life alone, so a job that idles (no key yet, an aterm.toml it cannot read)
/// leaves the port to this window.
#[cfg(unix)]
fn node_job_running_now() -> bool {
    aterm_types::dirs::state_dir()
        .is_some_and(|state| aterm_agent::node_service::node_port_holds_lock(&state))
}

/// Why a window leaves the fleet port to the node service, if it does: always with the mesh on
/// (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.4, one listener per Mac), and otherwise while
/// the node job holds the port ([`node_job_running_now`]). With the mesh off, a window with no
/// node job listens itself, as before.
fn window_yields_port(mesh: bool, node_job: impl FnOnce() -> bool) -> Option<&'static str> {
    if mesh {
        Some("fleet served by systems.alab.aterm-node, the one listener on this Mac")
    } else if node_job() {
        Some("fleet served by systems.alab.aterm-node")
    } else {
        None
    }
}

/// The fleet port's `ask` handler: the members desk, the one path a caller without the fleet
/// secret reaches (`docs/DESIGN-fluid-swarm-2026-10-28.md` F10).
#[cfg(unix)]
fn members_ask() -> Option<aterm_net::fleet::AskHandler> {
    Some(crate::members_desk::ask_handler())
}

/// Bind `addr` and serve it until `generation` is no longer current, each connection at the level
/// and grants it is accepted under ([`served_now`]).
#[cfg(unix)]
fn serve_fleet_port(generation: u64, addr: SocketAddr) {
    let (mut revision, mesh) = {
        let desk = FLEET_DESK.lock_or_recover();
        (desk.revision, desk.mesh)
    };
    loop {
        if let Some(line) = window_yields_port(mesh, node_job_running_now) {
            let serving = served_now();
            lift_undriving_holds(
                &serving.grants,
                &keyring_now(),
                denied_nodes_now().as_deref().unwrap_or_default(),
            );
            stand_fleet(generation, Standing::Listening, line);
            if wait_fleet_tick(generation, &mut revision) {
                continue;
            }
            return;
        }
        let secret = match read_fleet_key() {
            Ok(secret) => secret,
            Err(error) => {
                stand_fleet(
                    generation,
                    Standing::Refused(error.word()),
                    &format!(
                        "fleet listener not started: {error}; read again every {:?}",
                        retry()
                    ),
                );
                if wait_fleet_unchanged(generation) {
                    continue;
                }
                return;
            }
        };
        let listener = match aterm_uds::kept_out(|| TcpListener::bind(addr)) {
            Ok(listener) => listener,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::AddrInUse | std::io::ErrorKind::AddrNotAvailable
                ) =>
            {
                let state = aterm_types::dirs::state_dir();
                stand_fleet(
                    generation,
                    Standing::Retrying,
                    &unbound_line(addr, error.kind(), state.as_deref()),
                );
                if wait_fleet_unchanged(generation) {
                    continue;
                }
                return;
            }
            Err(error) => {
                stand_fleet(
                    generation,
                    Standing::Refused("bind"),
                    &format!("fleet listener not started: bind {addr}: {error}"),
                );
                return;
            }
        };
        let Some(running) = arm_generation(generation) else {
            return;
        };
        let node = read_node_key();
        let rekeyed = Arc::new(AtomicBool::new(false));
        let watching = std::thread::Builder::new()
            .name("aterm-fleet-watch".to_owned())
            .spawn({
                let (running, rekeyed, secret) =
                    (Arc::clone(&running), Arc::clone(&rekeyed), secret.clone());
                let proved = node
                    .as_ref()
                    .map(|node| (node.node().to_owned(), node.key()));
                move || {
                    watch_fleet_generation(
                        (generation, addr),
                        (&running, &rekeyed),
                        (&secret, proved),
                    );
                }
            });
        if let Err(error) = watching {
            stand_fleet(
                generation,
                Standing::Refused("thread"),
                &format!("fleet listener not started: its watch could not start: {error}"),
            );
            return;
        }
        let serving = served_now();
        stand_fleet(generation, Standing::Listening, &serving.line(addr));
        // The heartbeat's dial hints come from this process's one `_alab._tcp` browse.
        crate::network_moved::hear_alab_tcp(&[]);
        let sharing = share_units(addr, &running);
        let serving_generation = Arc::clone(&running);
        let join_secret = Arc::new(secret.clone());
        let listening = aterm_net::fleet::Listening {
            secret,
            scope: serving.scope,
            node,
            serves: serving.ops(),
            slots: fleet_slots(),
            per_source: aterm_net::fleet::per_source(fleet_slots()),
            lifetime: lifetime(),
        };
        // One bound for a connection's TLS and both proofs and for each read and write of an op
        // this listener answers itself (join, book, facts, mail code, census, vault join; a drive
        // or a relayed op is bounded by its relay), so the seam lifts both: a test world's peers
        // are aterms on this machine, which finish both however loaded it is.
        let within = seam_or(
            aterm_net::fleet::HANDSHAKE_SEAM,
            aterm_net::fleet::HANDSHAKE_WITHIN,
        );
        aterm_net::fleet::serve_offering_with_ask_within(
            &listener,
            (
                &listening,
                Arc::new(|| {
                    let serving = served_now();
                    (serving.scope, serving.ops())
                }),
                Arc::new(|| denied_under(denied_nodes_now().as_deref(), window_log().as_deref())),
            ),
            members_ask(),
            &running,
            within,
            move |served, op| {
                let peer = served.peer_addr();
                if op == FleetOp::Join {
                    let proof = served.peer_node().cloned();
                    return served.answer(|stream| {
                        stream.set_read_timeout(Some(within))?;
                        register_member(stream, (peer, proof.as_ref()), (addr, &join_secret))
                    });
                }
                if op == FleetOp::Book {
                    return served.answer(|stream| {
                        stream.set_write_timeout(Some(within))?;
                        share_book(stream, peer, (addr, &serving_generation))
                    });
                }
                if op == FleetOp::Facts {
                    let proof = served.peer_node().cloned();
                    return served.answer(|stream| {
                        stream.set_read_timeout(Some(within))?;
                        stream.set_write_timeout(Some(within))?;
                        receive_pushed_facts(stream, proof.as_ref(), peer)
                    });
                }
                if op == FleetOp::MailCode {
                    let proof = served.peer_node().cloned();
                    return served.answer(|stream| {
                        stream.set_read_timeout(Some(within))?;
                        stream.set_write_timeout(Some(within))?;
                        serve_mail_code_request(stream, proof.as_ref(), peer)
                    });
                }
                if op == FleetOp::Census {
                    return served.answer(|stream| {
                        stream.set_write_timeout(Some(within))?;
                        serve_census_request(stream, peer)
                    });
                }
                if op == FleetOp::VaultJoin {
                    // No deny list read: the proof cannot be told from a removed Mac's, so it is
                    // judged by none until one is (the handshake gate refuses before this). The
                    // vault it is judged by and served on is the one this process reads: none
                    // under a private state root, which refuses it ([`vault_join_on`]).
                    let vault = vault_here();
                    let judged = aterm_types::dirs::state_dir()
                        .ok_or_else(|| "this Mac has no state directory".to_owned())
                        .and_then(|state| {
                            let denied =
                                denied_nodes_now().ok_or_else(|| DENY_LIST_UNREAD.to_owned())?;
                            judge_vault_join_at(
                                &served,
                                &join_secret,
                                (&state, &served_now().grants),
                                &denied,
                                window_log().as_deref(),
                                vault.as_ref().map_err(Clone::clone)?.as_ref(),
                            )
                        });
                    let on = vault_join_on(judged, vault.ok().flatten());
                    return served.answer(|stream| {
                        stream.set_read_timeout(Some(within))?;
                        stream.set_write_timeout(Some(within))?;
                        crate::members_desk::serve_vault_join(stream, peer, on);
                        Ok(())
                    });
                }
                if op == FleetOp::Drive {
                    let (driver, grant) = match admit_driver(&served) {
                        Ok(admitted) => admitted,
                        Err((why, ungranted)) => {
                            return refuse_driver(served, peer, &why, ungranted);
                        }
                    };
                    let (near, far) = aterm_uds::CtlStream::pair()?;
                    let driver_key = grant.key;
                    let Some(_open) = Opened::served_live(true, Some(grant), &near)? else {
                        return refuse_driver(
                            served,
                            peer,
                            "[fleet] took its grant back, or this Mac's books bind it no more, while \
                             it was admitted",
                            true,
                        );
                    };
                    if !crate::control::attach_fleet_drive(
                        near,
                        peer,
                        driver,
                        Some(driver_key),
                        true,
                    ) {
                        return Err(std::io::Error::other(
                            "this aterm's control server is not serving",
                        ));
                    }
                    return served.relay_answers(far);
                }
                let serving = served_now();
                let content =
                    served.scope() == FleetScope::Content && serving.scope == FleetScope::Content;
                let drive = !serving.grants.is_empty() && lists_driver(&served, &serving.grants);
                let (near, far) = aterm_uds::CtlStream::pair()?;
                let Some(_open) = Opened::served(content, None, &near, served_now, &|_, _| false)?
                else {
                    return Err(std::io::Error::other(
                        "[fleet] lowered its level while it was admitted",
                    ));
                };
                if !crate::control::attach_fleet_connection(near, (content, drive), peer) {
                    return Err(std::io::Error::other(
                        "this aterm's control server is not serving",
                    ));
                }
                served.relay_answers(far)
            },
            fleet_event,
        );
        let stopped_itself = running.swap(false, Ordering::AcqRel);
        // Its port is free before a next generation binds it.
        if let Some(sharing) = sharing {
            let _ = sharing.join();
        }
        if rekeyed.load(Ordering::Acquire) {
            continue;
        }
        if stopped_itself {
            stand_fleet(
                generation,
                Standing::Refused("stopped"),
                "fleet listener stopped",
            );
        }
        return;
    }
}

/// LAN TOOLSET SHARING, on by default (owner ruling 2026-10-27: "everything must be ON BY
/// DEFAULT in atpkg!!!"): while a fleet generation listens, serve this Mac's kept toolset
/// archives ([`atpkg::peers::share`]) on the fleet listener's IP at
/// [`atpkg::peers::DEFAULT_PORT`], each connection from a fleet member's LAN address alone
/// ([`unit_members`], read again at most every [`aterm_net::fleet::KEY_READ_EVERY`]), until
/// `running` is cleared; `aterm pkg serve-units --off` stops it. It opens only once the fleet
/// listener listens in this process, so macOS's firewall has already ruled on this app's
/// incoming connections and asks nothing new. `None` where atpkg is inert, no store resolves,
/// or the fleet's guard refuses the address ([`aterm_net::loopback::check`]: an isolated debug
/// drive's loopback, and every address of a private aterm's, since the port is the person's).
#[cfg(unix)]
fn share_units(
    listen: SocketAddr,
    running: &Arc<AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    if !atpkg::manager_enabled() {
        return None;
    }
    let layout = atpkg::store::resolve_configured()?;
    let at = SocketAddr::new(listen.ip(), atpkg::peers::DEFAULT_PORT);
    if let Err(refused) = aterm_net::loopback::check(aterm_net::loopback::Reach::Listen(at)) {
        note_fleet(
            &format!("LAN toolset sharing not started: {refused}"),
            false,
        );
        return None;
    }
    let running = Arc::clone(running);
    let spawned = std::thread::Builder::new()
        .name("aterm-fleet-units".to_owned())
        .spawn(move || {
            if !cfg!(test) {
                crate::qos::set_self(crate::qos::Role::Background);
            }
            atpkg::peers::share(
                &layout,
                &atpkg::peers::Share {
                    listen: at,
                    running: &running,
                    members: &unit_members,
                    refresh: aterm_net::fleet::KEY_READ_EVERY,
                    note: &|line| note_fleet(line, false),
                },
            );
        });
    match spawned {
        Ok(thread) => Some(thread),
        Err(error) => {
            note_fleet(
                &format!("LAN toolset sharing could not start its thread: {error}"),
                true,
            );
            None
        }
    }
}

/// The addresses whose connections may take a toolset archive ([`share_units`]): this Mac's
/// fleet members' LAN addresses ([`aterm_net::fleet_member::member_addrs_with_denied`]), each a
/// private or link-local one ([`aterm_net::fleet::is_fleet_source`]).
#[cfg(unix)]
fn unit_members() -> Vec<IpAddr> {
    // No deny list read yet: no member's address may take an archive until one is.
    let Some(denied) = denied_nodes_now() else {
        return Vec::new();
    };
    let log = window_log();
    aterm_types::dirs::state_dir()
        .map(|state| {
            aterm_net::fleet_member::member_addrs_with_denied(&state, &denied, log.as_deref())
        })
        .unwrap_or_default()
}

/// THE DRIVE GRANT (the step-in, `docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5.1): the
/// driver, and the node and key it was admitted under, which [`Serving::serves`] holds to the
/// grants from then on.
/// # Errors
/// Why the dialer may not drive: it proved no node bound here, a node no grant admits, one whose
/// grant holds another key, or one the membership log holds out; with whether no grant admits its
/// node ([`aterm_net::fleet_member::Undriven::ungranted`]).
#[cfg(unix)]
fn admit_driver(
    served: &aterm_net::fleet::Served,
) -> Result<(crate::fabric::Driver, aterm_net::fleet::DriveGrant), (String, bool)> {
    let state = aterm_types::dirs::state_dir()
        .ok_or_else(|| ("this Mac has no state directory".to_owned(), false))?;
    let denied = denied_nodes_now().ok_or_else(|| (DENY_LIST_UNREAD.to_owned(), false))?;
    let grants = served_now().grants;
    admit_driver_at(served, &state, &grants, &denied, window_log().as_deref())
}

/// [`admit_driver`] under `grants`, the deny list `denied` and the membership log's verdict `log`
/// ([`aterm_net::fleet_member::proven_driver`]; none: this Mac's keyring alone).
#[cfg(unix)]
fn admit_driver_at(
    served: &aterm_net::fleet::Served,
    state: &std::path::Path,
    grants: &[aterm_net::fleet::DriveGrant],
    denied: &[String],
    log: Option<&aterm_net::fleet_member::MemberKeys>,
) -> Result<(crate::fabric::Driver, aterm_net::fleet::DriveGrant), (String, bool)> {
    let node = aterm_net::fleet_member::proven_driver(served, state, grants, log);
    admit_proven_driver_at(served.peer_node(), node, state, grants, denied)
}

/// [`admit_driver_at`] for a node `proof` a mesh link proved, its stream's peer
/// ([`aterm_net::fleet::link::Incoming::peer`]): the same grants, bindings and revocations, judged
/// now ([`aterm_net::fleet_member::recheck_driver`]).
#[cfg(unix)]
fn admit_linked_driver_at(
    proof: &aterm_net::fleet::NodeProof,
    state: &std::path::Path,
    grants: &[aterm_net::fleet::DriveGrant],
    denied: &[String],
    log: Option<&aterm_net::fleet_member::MemberKeys>,
) -> Result<(crate::fabric::Driver, aterm_net::fleet::DriveGrant), (String, bool)> {
    let node = aterm_net::fleet_member::recheck_driver(proof, state, grants, log);
    admit_proven_driver_at(Some(proof), node, state, grants, denied)
}

#[cfg(unix)]
fn admit_proven_driver_at(
    proof: Option<&aterm_net::fleet::NodeProof>,
    node: Result<String, aterm_net::fleet_member::Undriven>,
    state: &std::path::Path,
    grants: &[aterm_net::fleet::DriveGrant],
    denied: &[String],
) -> Result<(crate::fabric::Driver, aterm_net::fleet::DriveGrant), (String, bool)> {
    use aterm_net::fleet_member::peer_of;
    let refused = |why: &str, ungranted| (why.to_owned(), ungranted);
    if proof.is_some_and(|proof| {
        aterm_net::fleet::is_denied_node(&proof.node, denied)
            || aterm_net::fleet::is_denied_key(&proof.key, denied)
    }) {
        return Err(refused(
            "that node is on the swarm roster's deny-list",
            true,
        ));
    }
    // Each node named by a head no other node this Mac knows shares.
    let knows = aterm_net::fleet_member::known_nodes(state);
    let node = node.map_err(|why| (why.said(&knows), why.ungranted()))?;
    let key = proof
        .filter(|proof| proof.node == node)
        .map(|proof| proof.key)
        .ok_or_else(|| refused("it proved no node", false))?;
    aterm_net::fleet::DriveGrant::admitting(grants, &node, key)
        .ok_or_else(|| refused("no grant admits it now", true))?;
    let grant = aterm_net::fleet::DriveGrant {
        node: node.clone(),
        key,
    };
    note_admitted_driver(&node, key);
    let host = driver_name(
        &node,
        peer_of(state, &node).map(|peer| peer.host).as_deref(),
        &knows,
    );
    Ok((crate::fabric::Driver { node, host }, grant))
}

/// What a `vault-join` channel is worth on this Mac ([`aterm_agent::vault_join::judge`]): the node
/// key it proved; the fleet's key only where the listener's secret (`listening`) is the fleet code
/// the vault at `vault` holds now and this Mac's drive admission takes the node under `grants`
/// ([`aterm_net::fleet_member::proven_driver`]); whether the book in `state` binds the node id
/// to a key `log` holds out; and what the book's peers say of the fleet
/// ([`aterm_agent::vault_join::book_word`]). Read on the connection's thread.
#[cfg(unix)]
pub(crate) fn judge_vault_join_at(
    served: &aterm_net::fleet::Served,
    listening: &aterm_net::fleet::FleetSecret,
    (state, grants): (&std::path::Path, &[aterm_net::fleet::DriveGrant]),
    denied: &[String],
    log: Option<&aterm_net::fleet_member::MemberKeys>,
    vault: Option<&aterm_secrets::client::Paths>,
) -> Result<aterm_agent::vault_join::Judged, String> {
    let proof = served.peer_node();
    let driven = aterm_net::fleet_member::proven_driver(served, state, grants, log).is_ok();
    let bound =
        proof.and_then(|proof| aterm_net::fleet_member::Keyring::read(state).bound(&proof.node));
    let peers = aterm_net::fleet_member::read_peers(state).ok();
    aterm_agent::vault_join::judge(
        proof,
        (
            listening.mark(),
            vault.and_then(aterm_agent::vault_join::vault_code_mark),
        ),
        driven,
        (bound, denied, log),
        (
            peers.as_deref(),
            served.peer_addr().map(|at| at.ip()),
            aterm_time::unix_millis().unwrap_or(0),
        ),
    )
}

/// What a `vault-join` its judge found `judged` is served on: the vault that judge read
/// (`vault`, [`vault_here`]), in the window and the node job alike. A process that reads no vault
/// (under a private state root) refuses it, as its proof was judged by no vault's fleet code and
/// the vault its desk would let the Mac into is none this fleet follows.
#[cfg(unix)]
pub(crate) fn vault_join_on(
    judged: Result<aterm_agent::vault_join::Judged, String>,
    vault: Option<aterm_secrets::client::Paths>,
) -> Result<
    (
        aterm_agent::vault_join::Judged,
        aterm_secrets::client::Paths,
    ),
    String,
> {
    let judged = judged?;
    let vault = vault.ok_or_else(|| "this aterm follows no vault".to_owned())?;
    Ok((judged, vault))
}

/// Whether this Mac lets the peer of `served` drive it under `grants`.
#[cfg(unix)]
fn lists_driver(
    served: &aterm_net::fleet::Served,
    grants: &[aterm_net::fleet::DriveGrant],
) -> bool {
    let Some(denied) = denied_nodes_now() else {
        return false;
    };
    aterm_types::dirs::state_dir().is_some_and(|state| {
        lists_driver_at(served, &state, grants, &denied, window_log().as_deref())
    })
}

/// Whether a drive of `served` would be admitted under `grants`, `denied` and the log's verdict
/// `log`, as [`admit_driver_at`] judges it, so a survey's `drive=1` is what a drive gets.
#[cfg(unix)]
fn lists_driver_at(
    served: &aterm_net::fleet::Served,
    state: &std::path::Path,
    grants: &[aterm_net::fleet::DriveGrant],
    denied: &[String],
    log: Option<&aterm_net::fleet_member::MemberKeys>,
) -> bool {
    !served.peer_node().is_some_and(|proof| {
        aterm_net::fleet::is_denied_node(&proof.node, denied)
            || aterm_net::fleet::is_denied_key(&proof.key, denied)
    }) && aterm_net::fleet_member::proven_driver(served, state, grants, log).is_ok()
}

/// [`lists_driver_at`] for a node `proof` a mesh link proved.
#[cfg(unix)]
fn lists_linked_driver_at(
    proof: &aterm_net::fleet::NodeProof,
    state: &std::path::Path,
    grants: &[aterm_net::fleet::DriveGrant],
    denied: &[String],
    log: Option<&aterm_net::fleet_member::MemberKeys>,
) -> bool {
    !(aterm_net::fleet::is_denied_node(&proof.node, denied)
        || aterm_net::fleet::is_denied_key(&proof.key, denied))
        && aterm_net::fleet_member::recheck_driver(proof, state, grants, log).is_ok()
}

/// A connection the listener handed to the control server, which [`cut_unserved`] hangs up once
/// `[fleet]` serves it no more.
struct Open {
    id: u64,
    /// It is served at content.
    content: bool,
    /// The node and key a drive was admitted under.
    driver: Option<aterm_net::fleet::DriveGrant>,
    /// The control server's end.
    line: aterm_uds::CtlStream,
}

static OPEN: Mutex<Vec<Open>> = Mutex::new(Vec::new());

static OPENED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// An [`Open`] entry, taken out when its connection ends.
struct Opened(u64);

impl Opened {
    /// Hold the entry of `line`, then read `[fleet]` again (`now`) beside what this Mac holds (`holds`):
    /// `None`, the entry taken out, where it serves the connection no more. A change that took it
    /// away while it was admitted is seen here, or, made after this read, by the [`cut_unserved`]
    /// that follows it.
    fn served(
        content: bool,
        driver: Option<aterm_net::fleet::DriveGrant>,
        line: &aterm_uds::CtlStream,
        now: impl FnOnce() -> Serving,
        holds: &dyn Fn(&str, aterm_net::fleet::NodeKey) -> bool,
    ) -> std::io::Result<Option<Self>> {
        Self::served_with_denied(content, driver, line, now, holds, &[])
    }

    #[cfg(unix)]
    fn served_live(
        content: bool,
        driver: Option<aterm_net::fleet::DriveGrant>,
        line: &aterm_uds::CtlStream,
    ) -> std::io::Result<Option<Self>> {
        let id = OPENED.fetch_add(1, Ordering::Relaxed);
        let line = aterm_uds::kept_out(|| line.try_clone())?;
        OPEN.lock_or_recover().push(Open {
            id,
            content,
            driver: driver.clone(),
            line,
        });
        let held = Self(id);
        let now = served_now();
        let holds = holds_now();
        // No deny list read yet: nothing is served until one is.
        let denied = denied_nodes_now();
        Ok(denied
            .is_some_and(|denied| now.serves(content, driver.as_ref(), &holds, &denied))
            .then_some(held))
    }

    fn served_with_denied(
        content: bool,
        driver: Option<aterm_net::fleet::DriveGrant>,
        line: &aterm_uds::CtlStream,
        now: impl FnOnce() -> Serving,
        holds: &dyn Fn(&str, aterm_net::fleet::NodeKey) -> bool,
        denied_nodes: &[String],
    ) -> std::io::Result<Option<Self>> {
        let id = OPENED.fetch_add(1, Ordering::Relaxed);
        let line = aterm_uds::kept_out(|| line.try_clone())?;
        OPEN.lock_or_recover().push(Open {
            id,
            content,
            driver: driver.clone(),
            line,
        });
        let held = Self(id);
        Ok(now()
            .serves(content, driver.as_ref(), holds, denied_nodes)
            .then_some(held))
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        OPEN.lock_or_recover().retain(|open| open.id != self.0);
    }
}

/// Hang up each open connection `serving` serves no more ([`Serving::serves`] beside what this
/// Mac holds, `holds`): a drive no grant it names admits any more, or whose node this Mac holds no
/// more under the key it was admitted under, any drive below drive, and a read served at content
/// once it serves facts. How many.
#[cfg(test)]
fn cut_unserved(
    serving: &Serving,
    holds: &dyn Fn(&str, aterm_net::fleet::NodeKey) -> bool,
) -> usize {
    cut_unserved_with_denied(serving, holds, &[])
}

fn cut_unserved_with_denied(
    serving: &Serving,
    holds: &dyn Fn(&str, aterm_net::fleet::NodeKey) -> bool,
    denied_nodes: &[String],
) -> usize {
    let mut cut = 0;
    OPEN.lock_or_recover().retain(|open| {
        let kept = serving.serves(open.content, open.driver.as_ref(), holds, denied_nodes);
        if !kept {
            aterm_uds::hangup::hang_up(&open.line);
            cut += 1;
        }
        kept
    });
    cut
}

/// The proven `node` of a drive as its credit, its hold and its records name it, by a head no
/// other node this Mac `knows` shares ([`aterm_net::fleet_member::node_head`]).
#[cfg(unix)]
fn driver_name(node: &str, host: Option<&str>, knows: &[String]) -> String {
    let short = aterm_net::fleet_member::node_head(node, knows);
    match host.filter(|host| aterm_net::fleet_member::is_host(host)) {
        Some(host) => format!("{host} (node {short})"),
        None => format!("node {short}"),
    }
}

/// Record a refused drive from `peer` among the fleet gate's denials, as each refusal is.
#[cfg(unix)]
fn note_refused_driver(peer: Option<SocketAddr>, why: &str) {
    let at = peer.map_or_else(|| "a fleet peer".to_owned(), |peer| peer.to_string());
    let line = format!(
        "{at} was refused a drive: {}",
        aterm_log::sanitize_record(why)
    );
    if let Some(line) = crate::control::control_denials::FLEET_GATE_DENIALS
        .record_network(&line, std::time::Instant::now())
    {
        aterm_log::warn!("fleet: {line}");
    }
}

/// A drive this Mac refuses ([`admit_driver`]), under
/// [`aterm_types::control_verbs::DRIVE_UNGRANTED`] where no grant admits its node (`ungranted`).
#[cfg(unix)]
fn refuse_driver(
    served: aterm_net::fleet::Served,
    peer: Option<SocketAddr>,
    why: &str,
    ungranted: bool,
) -> std::io::Result<()> {
    note_refused_driver(peer, why);
    served.answer(|stream| {
        stream.set_write_timeout(Some(aterm_net::fleet::HANDSHAKE_WITHIN))?;
        let head = if ungranted {
            aterm_types::control_verbs::DRIVE_UNGRANTED
        } else {
            "ERR denied: "
        };
        std::io::Write::write_all(stream, format!("{head}{why}\n").as_bytes())
    })
}

/// A member's `join` (`aterm fleet join` on another machine), proved under `secret`.
#[cfg(unix)]
fn register_member<S: std::io::Read + std::io::Write>(
    stream: &mut S,
    (peer, proof): (Option<SocketAddr>, Option<&aterm_net::fleet::NodeProof>),
    (addr, secret): (SocketAddr, &aterm_net::fleet::FleetSecret),
) -> std::io::Result<()> {
    let Some(state) = aterm_types::dirs::state_dir() else {
        return Err(std::io::Error::other(
            "a join with no peer address or no state directory",
        ));
    };
    let denied = denied_nodes_now().ok_or_else(|| std::io::Error::other(DENY_LIST_UNREAD))?;
    let log = window_log();
    register_member_at(
        stream,
        &state,
        (peer, proof),
        (addr, secret),
        (&denied, log.as_deref()),
    )
}

/// Record the join `stream` carries, as any member does (`docs/DESIGN-fluid-swarm-2026-10-28.md`
/// §4.1, F17): this Mac listens at `addr`, refuses a node `denied` lists, and with a membership
/// log (`log`) records only a node key it admits.
#[cfg(unix)]
fn register_member_at<S: std::io::Read + std::io::Write>(
    stream: &mut S,
    state: &std::path::Path,
    (peer, proof): (Option<SocketAddr>, Option<&aterm_net::fleet::NodeProof>),
    (addr, secret): (SocketAddr, &aterm_net::fleet::FleetSecret),
    (denied, log): (&[String], Option<&aterm_net::fleet_member::MemberKeys>),
) -> std::io::Result<()> {
    use aterm_net::fleet_member::{Issuer, read_node_id, serve_join_with_denied};
    let Some(peer) = peer else {
        return Err(std::io::Error::other(
            "a join with no peer address or no state directory",
        ));
    };
    let member = || {
        let node = read_node_id(state)
            .map_err(|e| e.to_string())?
            .ok_or_else(no_node_id)?;
        let host = own_host(addr).ok_or_else(|| "it has no LAN address to advertise".to_owned())?;
        Ok(Issuer {
            node,
            host,
            port: addr.port(),
            version: aterm_stamp::app_version().to_owned(),
        })
    };
    let now_ms = aterm_time::unix_millis().unwrap_or(0);
    match serve_join_with_denied(
        stream,
        (peer.ip(), proof),
        (state, secret),
        member,
        now_ms,
        (denied, log),
    )? {
        Ok((recorded, member)) => aterm_log::info!("{}", registration_note(recorded, &member)),
        Err(why) => aterm_log::warn!(
            "fleet: the join from {peer} was refused: {}",
            aterm_log::sanitize_record(&why)
        ),
    }
    Ok(())
}

/// Why this machine, a member, cannot name itself: its node file is gone.
#[cfg(unix)]
fn no_node_id() -> String {
    format!(
        "it has no node id (the {} file in its state directory is gone): restore that file, \
         or `aterm fleet leave` and join again there",
        aterm_net::fleet_member::NODE_FILE
    )
}

/// The address this Mac advertises while it listens at `addr`: its explicit bind
/// address, or the same LAN route a first automatic join advertises for a wildcard bind.
#[cfg(unix)]
pub(crate) fn own_host(addr: SocketAddr) -> Option<String> {
    own_host_from(addr, aterm_secrets::server::route_sources)
}

#[cfg(unix)]
fn own_host_from(
    addr: SocketAddr,
    routes: impl FnOnce() -> [Option<std::net::IpAddr>; 3],
) -> Option<String> {
    if !addr.ip().is_unspecified() {
        return Some(addr.ip().to_string());
    }
    aterm_net::fleet_member::automatic_host(routes())
}

/// A member's read of this machine's book of peers.
#[cfg(unix)]
fn share_book<S: std::io::Write>(
    stream: &mut S,
    peer: Option<SocketAddr>,
    (addr, current): (SocketAddr, &AtomicBool),
) -> std::io::Result<()> {
    let Some(state) = aterm_types::dirs::state_dir() else {
        return Err(std::io::Error::other("a book read with no state directory"));
    };
    let denied = denied_nodes_now().ok_or_else(|| std::io::Error::other(DENY_LIST_UNREAD))?;
    let log = window_log();
    share_book_at(
        stream,
        &state,
        peer,
        (&denied, log.as_deref()),
        (addr, current),
    )
}

/// Serve this Mac's book to `peer`, its own entry first, durably signed for the place it listens
/// at (`addr`; [`aterm_net::fleet_member::self_entry_while`]), every member alike (F17). A worker
/// from an ended generation commits no later descriptor and aborts its response.
#[cfg(unix)]
fn share_book_at<S: std::io::Write>(
    stream: &mut S,
    state: &std::path::Path,
    peer: Option<SocketAddr>,
    (denied, log): (&[String], Option<&aterm_net::fleet_member::MemberKeys>),
    (addr, current): (SocketAddr, &AtomicBool),
) -> std::io::Result<()> {
    use aterm_net::fleet_member::{
        read_node_id, read_node_identity, self_entry_while, serve_book_with_log,
    };
    let ended = || std::io::Error::other("the fleet listener generation ended");
    if !current.load(Ordering::Acquire) {
        return Err(ended());
    }
    let now_ms = aterm_time::unix_millis();
    let own = read_node_identity(state)
        .ok()
        .flatten()
        .zip(own_host(addr))
        .map(|(me, host)| {
            self_entry_while(
                state,
                &me,
                (&host, addr.port(), aterm_stamp::app_version()),
                current,
            )
        })
        .transpose()?
        .flatten();
    if !current.load(Ordering::Acquire) {
        return Err(ended());
    }
    let node = || {
        read_node_id(state)
            .map_err(|e| e.to_string())?
            .ok_or_else(no_node_id)
    };
    let peer = peer.map_or_else(|| "a peer".to_owned(), |peer| peer.to_string());
    match serve_book_with_log(stream, state, node, now_ms, (denied, log, own.as_ref()))? {
        Ok(shared) => aterm_log::debug!("fleet: {peer} read this machine's book ({shared} peers)"),
        Err(why) => aterm_log::info!(
            "fleet: {peer}'s read of the book was refused: {}",
            aterm_log::sanitize_record(&why)
        ),
    }
    Ok(())
}

/// Receive a member's C16 `survey facts` push on [`FleetOp::Facts`].
#[cfg(unix)]
fn receive_pushed_facts<S: std::io::Read + std::io::Write>(
    stream: &mut S,
    proof: Option<&aterm_net::fleet::NodeProof>,
    peer: Option<SocketAddr>,
) -> std::io::Result<()> {
    let Some(state) = aterm_types::dirs::state_dir() else {
        return Err(std::io::Error::other(
            "a facts push with no state directory",
        ));
    };
    let denied = denied_nodes_now().ok_or_else(|| std::io::Error::other(DENY_LIST_UNREAD))?;
    let log = window_log();
    receive_pushed_facts_at(stream, &state, proof, peer, (&denied, log.as_deref()))
}

#[cfg(unix)]
fn receive_pushed_facts_at<S: std::io::Read + std::io::Write>(
    stream: &mut S,
    state: &std::path::Path,
    proof: Option<&aterm_net::fleet::NodeProof>,
    peer: Option<SocketAddr>,
    (denied, log): (&[String], Option<&aterm_net::fleet_member::MemberKeys>),
) -> std::io::Result<()> {
    let peer = peer.map_or_else(|| "a peer".to_owned(), |peer| peer.to_string());
    match aterm_net::fleet_member::serve_facts_with_denied(
        stream,
        proof,
        state,
        (denied, log),
        |payload| {
            aterm_agent::survey_facts::SwarmSurveyFacts::from_survey_reply(payload).map(|f| f.node)
        },
    )? {
        Ok(node) => {
            aterm_log::debug!("fleet: stored pushed survey facts from node {node} ({peer})")
        }
        Err(why) => aterm_log::info!(
            "fleet: {peer}'s survey facts push was refused: {}",
            aterm_log::sanitize_record(&why)
        ),
    }
    Ok(())
}

/// Answer a member's C17c catch-all sign-in code request on [`FleetOp::MailCode`].
#[cfg(unix)]
fn serve_mail_code_request<S: std::io::Read + std::io::Write>(
    stream: &mut S,
    proof: Option<&aterm_net::fleet::NodeProof>,
    peer: Option<SocketAddr>,
) -> std::io::Result<()> {
    let Some(state) = aterm_types::dirs::state_dir() else {
        return Err(std::io::Error::other(
            "a mail-code request with no state directory",
        ));
    };
    let home = aterm_types::dirs::home_dir();
    serve_mail_code_request_at(stream, &state, home.as_deref(), proof, peer)
}

#[cfg(unix)]
fn serve_mail_code_request_at<S: std::io::Read + std::io::Write>(
    stream: &mut S,
    state: &std::path::Path,
    home: Option<&std::path::Path>,
    proof: Option<&aterm_net::fleet::NodeProof>,
    peer: Option<SocketAddr>,
) -> std::io::Result<()> {
    let now = aterm_time::unix_secs()
        .and_then(|s| i64::try_from(s).ok())
        .unwrap_or(0);
    let peer = peer.map_or_else(|| "a peer".to_owned(), |peer| peer.to_string());
    match aterm_agent::harness::ai_signin::serve_fleet_mail_code(stream, proof, state, home, now)? {
        Ok(audit) => aterm_log::info!(
            "fleet: {} ({peer})",
            aterm_log::sanitize_record(&audit.log_line())
        ),
        Err(why) => aterm_log::debug!(
            "fleet: refused mail-code to {peer}: {}",
            aterm_log::sanitize_record(&why)
        ),
    }
    Ok(())
}

/// Answer a reader's C24 cached swarm census request on [`FleetOp::Census`].
#[cfg(unix)]
fn serve_census_request<S: std::io::Read + std::io::Write>(
    stream: &mut S,
    peer: Option<SocketAddr>,
) -> std::io::Result<()> {
    let Some(state) = aterm_types::dirs::state_dir() else {
        return Err(std::io::Error::other(
            "a census request with no state directory",
        ));
    };
    let home = aterm_types::dirs::home_dir();
    serve_census_request_at(stream, &state, home.as_deref(), peer)
}

#[cfg(unix)]
fn serve_census_request_at<S: std::io::Read + std::io::Write>(
    stream: &mut S,
    state: &std::path::Path,
    home: Option<&std::path::Path>,
    peer: Option<SocketAddr>,
) -> std::io::Result<()> {
    let now_s = aterm_time::unix_secs().unwrap_or(0);
    let peer = peer.map_or_else(|| "a peer".to_owned(), |peer| peer.to_string());
    match aterm_net::fleet_member::serve_census(stream, || {
        aterm_agent::fleet_census::serve_fleet_census(state, home, now_s)
    })? {
        Ok(bytes) => aterm_log::debug!("fleet: served {bytes}B census to {peer}"),
        Err(why) => aterm_log::debug!(
            "fleet: refused census to {peer}: {}",
            aterm_log::sanitize_record(&why)
        ),
    }
    Ok(())
}

#[cfg(unix)]
fn maybe_refresh_local_census() {
    let Some(state) = aterm_types::dirs::state_dir() else {
        return;
    };
    let home = aterm_types::dirs::home_dir();
    maybe_refresh_local_census_at(&state, home.as_deref());
}

#[cfg(unix)]
fn maybe_refresh_local_census_at(state: &std::path::Path, home: Option<&std::path::Path>) {
    let now_s = aterm_time::unix_secs().unwrap_or(0);
    if let Err(why) =
        aterm_agent::fleet_census::refresh_local_census(state, home, Vec::new(), now_s)
    {
        aterm_log::debug!(
            "fleet: periodic swarm census refresh skipped: {}",
            aterm_log::sanitize_record(&why)
        );
    }
}

/// Where [`push_facts_to_issuer_with_denied`] delivered a C16 `survey facts` snapshot.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FactsPushOutcome {
    /// Kept in this Mac's `<state>/swarm-facts/` alone: its book names no issuer but itself.
    StoredLocally(String),
    /// Kept here and pushed over [`FleetOp::Facts`] to the fleet issuer at `(host, port)`.
    PushedRemote { host: String, port: u16 },
}

#[cfg(all(unix, test))]
pub(crate) fn push_facts_to_issuer(
    state_dir: &std::path::Path,
    secret: &aterm_net::fleet::FleetSecret,
    payload: &str,
) -> Result<FactsPushOutcome, String> {
    push_facts_to_issuer_with_denied(state_dir, secret, (&[], None), payload)
}

/// Keep `payload` (`OK <n>\n…` C16 survey facts, this Mac's own) here, where this Mac computes its
/// census from (docs/DESIGN-fluid-swarm-2026-10-28.md F17), and push it to the fleet issuer this
/// Mac's book names, whose census a Mac that predates F17 dials for. The facts stay here whatever
/// the issuer answers; one that names another node is pushed (and refused there), never kept.
#[cfg(unix)]
pub(crate) fn push_facts_to_issuer_with_denied(
    state_dir: &std::path::Path,
    secret: &aterm_net::fleet::FleetSecret,
    (denied_nodes, log): (&[String], Option<&aterm_net::fleet_member::MemberKeys>),
    payload: &str,
) -> Result<FactsPushOutcome, String> {
    use aterm_net::fleet_member::{
        Via, push_facts_to_bound_peer, read_node_identity, read_peers, store_pushed_facts_with_key,
    };
    let parsed = aterm_agent::survey_facts::SwarmSurveyFacts::from_survey_reply(payload)?;
    let me = read_node_identity(state_dir)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no fleet node identity on disk".to_owned())?;
    if parsed.node.as_deref().is_none_or(|node| node == me.node()) {
        store_pushed_facts_with_key(state_dir, me.node(), Some(me.key()), payload)
            .map_err(|e| e.to_string())?;
    }
    let peers = read_peers(state_dir).unwrap_or_default();
    let Some(peer) = peers
        .iter()
        .find(|p| p.via == Via::Issuer && p.node.as_deref() != Some(me.node()))
    else {
        return Ok(FactsPushOutcome::StoredLocally(me.node().to_owned()));
    };
    push_facts_to_bound_peer(
        peer,
        (secret, Some(&me)),
        (denied_nodes, log),
        payload,
        FACTS_PUSH_WITHIN,
    )?;
    Ok(FactsPushOutcome::PushedRemote {
        host: peer.host.clone(),
        port: peer.port,
    })
}

#[cfg(unix)]
fn push_facts_heartbeat(secret: &aterm_net::fleet::FleetSecret) {
    let Some(state) = aterm_types::dirs::state_dir() else {
        return;
    };
    let here = crate::survey::Here::cached_for_listener();
    let Some(mut facts) = here.facts else {
        return;
    };
    if facts.node.is_none() {
        facts.node = aterm_net::fleet_member::read_node_id(&state).ok().flatten();
    }
    let at_ms = aterm_time::unix_millis().unwrap_or(0);
    let payload = facts.to_survey_reply(
        std::process::id(),
        here.host.as_deref(),
        at_ms,
        here.instances,
        false,
    );
    // No deny list read yet: this Mac's facts go to no one until one is.
    let Some(denied) = denied_nodes_now() else {
        aterm_log::debug!("fleet: survey facts heartbeat push skipped: {DENY_LIST_UNREAD}");
        return;
    };
    let log = window_log();
    if let Err(why) =
        push_facts_to_issuer_with_denied(&state, secret, (&denied, log.as_deref()), &payload)
    {
        aterm_log::debug!(
            "fleet: survey facts heartbeat push skipped: {}",
            aterm_log::sanitize_record(&why)
        );
    }
}

/// The audit line of a member's recorded join ([`register_member`]).
#[cfg(unix)]
fn registration_note(
    recorded: aterm_net::fleet_member::Recorded,
    member: &aterm_net::fleet_member::Peer,
) -> String {
    use aterm_net::fleet_member::Recorded;
    let at = member.addr.map_or_else(
        || format!("port {}", member.port),
        |addr| std::net::SocketAddr::new(addr, member.port).to_string(),
    );
    let node = member.node.as_deref().unwrap_or("-");
    let host = aterm_log::sanitize_record(&member.host);
    match recorded {
        Recorded::New => {
            format!(
                "fleet: node {node} at {at} (host {host}) registered with this machine's book of peers"
            )
        }
        Recorded::Updated | Recorded::Already => format!(
            "fleet: node {node} at {at} (host {host}) registered again; its entry in the book is \
             updated"
        ),
    }
}

/// The listener's line for a bind refused `kind` at `addr`: a place another process holds names
/// the act that moves this listener ([`aterm_net::fleet_member::moves_listener`]).
#[cfg(unix)]
fn unbound_line(
    addr: SocketAddr,
    kind: std::io::ErrorKind,
    state: Option<&std::path::Path>,
) -> String {
    let why = match (kind, state) {
        (std::io::ErrorKind::AddrInUse, Some(state)) => format!(
            "is held by another process (another node's listener, or another program): {}",
            aterm_net::fleet_member::moves_listener(state)
        ),
        (std::io::ErrorKind::AddrInUse, None) => "is held by another process".to_owned(),
        _ => "is not an address of this machine yet".to_owned(),
    };
    format!(
        "fleet listener: {addr} {why}; tried again every {:?}",
        retry()
    )
}

/// This machine's fleet secret, from `<state>/fleet.key`.
#[cfg(unix)]
fn read_fleet_key() -> Result<aterm_net::fleet::FleetSecret, aterm_net::fleet::KeyFileError> {
    match aterm_types::dirs::state_dir() {
        Some(dir) => aterm_net::fleet::read_key_file(&dir),
        None => Err(aterm_net::fleet::KeyFileError::Missing),
    }
}

/// This machine's node and its node key.
#[cfg(unix)]
fn read_node_key() -> Option<aterm_net::fleet::NodeIdentity> {
    use aterm_net::fleet_member::{NODE_KEY_FILE, read_node_identity};
    match read_node_identity(&aterm_types::dirs::state_dir()?) {
        Ok(node) => node,
        Err(error) => {
            aterm_log::warn!(
                "fleet listener: proves no node: {NODE_KEY_FILE}: {}",
                aterm_log::sanitize_record(&error.to_string())
            );
            None
        }
    }
}

/// While `generation` listens at `addr`, serves `secret` and proves the node `proved` (its id and
/// key), read them again every [`RETRY`], or at once when a reload changes `[fleet]` in place:
/// another key moves the listener, and `[fleet]` is followed where it stands ([`follow_serving`]).
#[cfg(unix)]
fn watch_fleet_generation(
    (generation, addr): (u64, SocketAddr),
    (running, rekeyed): (&Arc<AtomicBool>, &AtomicBool),
    (secret, proved): (
        &aterm_net::fleet::FleetSecret,
        Option<(String, aterm_net::fleet::NodeKey)>,
    ),
) {
    use aterm_net::fleet::KEY_FILE;
    use aterm_net::fleet_member::{NODE_FILE, NODE_KEY_FILE, read_node_identity};
    let node_now = || {
        aterm_types::dirs::state_dir()
            .and_then(|dir| read_node_identity(&dir).ok().flatten())
            .map(|node| (node.node().to_owned(), node.key()))
    };
    if let (Some(state), Ok(Some(paths))) = (aterm_types::dirs::state_dir(), vault_here())
        && let Some(me) = read_node_identity(&state).ok().flatten()
    {
        let _ = aterm_agent::fleet_log::ensure_local_node_key_in_log(
            &paths,
            &me,
            window_log().as_deref(),
        );
    }
    let mut revision = FLEET_DESK.lock_or_recover().revision;
    let mut last_facts_push = std::time::Instant::now();
    let mut last_census_refresh = std::time::Instant::now();
    let every = heartbeat_every();
    let hb_in_flight = Arc::new(AtomicBool::new(false));
    let mut heartbeat = HeartbeatClock::new(
        initial_heartbeat_anchor(every),
        every,
        crate::network_moved::watch(),
    );
    while wait_fleet_tick(generation, &mut revision) && running.load(Ordering::Acquire) {
        if node_job_running_now() {
            rekeyed.store(true, Ordering::Release);
            running.store(false, Ordering::Release);
            note_fleet(
                "fleet listener: systems.alab.aterm-node started; handing over the fleet port",
                false,
            );
            return;
        }
        let why = match read_fleet_key() {
            Ok(now) if now == *secret && node_now() == proved => {
                follow_serving(generation, addr);
                // Each dial selects the broker at the transport boundary for LAN targets.
                // Loopback peers remain reachable without a LAN broker.
                let now = std::time::Instant::now();
                let facts_due = last_facts_push.elapsed() >= FACTS_HEARTBEAT_EVERY;
                // A round still out takes no move: the move waits for the round after it.
                let busy = hb_in_flight.load(Ordering::Acquire);
                if facts_due {
                    last_facts_push = std::time::Instant::now();
                    push_facts_heartbeat(secret);
                }
                if last_census_refresh.elapsed() >= CENSUS_REFRESH_EVERY {
                    last_census_refresh = std::time::Instant::now();
                    maybe_refresh_local_census();
                }
                if let Some(woke) = heartbeat.due_when_free(now, busy) {
                    say_heartbeat_woke("fleet listener", woke);
                    spawn_member_heartbeat(addr, secret.clone(), (running, &hb_in_flight));
                }
                continue;
            }
            Ok(now) if now == *secret => format!("{NODE_FILE} or {NODE_KEY_FILE} changed"),
            Ok(_) => format!("{KEY_FILE} changed"),
            Err(error) => error.to_string(),
        };
        rekeyed.store(true, Ordering::Release);
        running.store(false, Ordering::Release);
        note_fleet(
            &format!("fleet listener: {why}; every fleet connection is cut and the key read again"),
            false,
        );
        return;
    }
}

/// Now, when a member answered this Mac's heartbeat within `every`
/// ([`aterm_net::fleet_member::last_answered`]): the window's first heartbeat then waits its turn.
#[cfg(unix)]
fn initial_heartbeat_anchor(every: Duration) -> Option<std::time::Instant> {
    let state = aterm_types::dirs::state_dir()?;
    let seen = aterm_net::fleet_member::last_answered(&state)?;
    let now = aterm_time::unix_millis()?;
    let every_ms = u64::try_from(every.as_millis()).unwrap_or(u64::MAX);
    (now.saturating_sub(seen) < every_ms).then(std::time::Instant::now)
}

#[cfg(unix)]
fn spawn_member_heartbeat(
    addr: SocketAddr,
    secret: aterm_net::fleet::FleetSecret,
    (current, in_flight): (&Arc<AtomicBool>, &Arc<AtomicBool>),
) {
    let Some(state) = aterm_types::dirs::state_dir() else {
        return;
    };
    // A record of the vault the fleet state is of, adopted where an older build left none.
    let vault = vault_here().ok().flatten();
    aterm_agent::fleet_vault::keep(&state, vault.as_ref());
    let mut log = window_log();
    if log.as_deref().is_some_and(|keys| {
        aterm_net::fleet_member::read_node_identity(&state)
            .ok()
            .flatten()
            .is_some_and(|me| !keys.admits(&me.key()))
    }) && let Some(paths) = vault.as_ref()
        && let Some(me) = aterm_net::fleet_member::read_node_identity(&state)
            .ok()
            .flatten()
        && aterm_agent::fleet_log::ensure_local_node_key_in_log(paths, &me, log.as_deref())
    {
        log = window_log();
    }
    // No deny list read yet: no member is dialed until one is; the next round is due as usual.
    let Some(denied) = denied_nodes_now() else {
        aterm_log::debug!("fleet heartbeat skipped: {DENY_LIST_UNREAD}");
        return;
    };
    spawn_member_heartbeat_at(
        &state,
        addr,
        secret,
        (current, in_flight),
        (denied, log, crate::network_moved::alab_tcp()),
    );
}

/// How long a heartbeat round resolves the `_alab._tcp` adverts its browse holds.
#[cfg(unix)]
const HINTS_WITHIN: Duration = Duration::from_secs(3);

/// The places the `_alab._tcp` adverts `browse` holds resolve to within `budget`: dial hints a
/// heartbeat round tries after the book's members, each judged by the proof its listener gives
/// (an advert names no node).
#[cfg(unix)]
pub(crate) fn alab_hints(
    browse: &aterm_secrets::discover::Watch,
    budget: Duration,
) -> Vec<SocketAddr> {
    let deadline = std::time::Instant::now() + budget;
    let places: Vec<(String, u16)> = browse
        .adverts(budget)
        .into_iter()
        .map(|advert| (advert.host, advert.port))
        .collect();
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    aterm_net::fleet_member::resolve_ipv4_hints_within(&places, left)
}

/// Whom a heartbeat round ([`spawn_member_heartbeat_at`]) registers with: the nodes `denied`
/// lists are left out, the membership log (none: the book) admits the rest, and the adverts of
/// the browse watch are tried after the book's places.
#[cfg(unix)]
type HeartbeatReach = (
    Arc<[String]>,
    Option<Arc<aterm_net::fleet_member::MemberKeys>>,
    Option<Arc<aterm_secrets::discover::Watch>>,
);

/// One heartbeat round of this Mac, which listens at `addr` under `secret`, unless one is in
/// flight (`in_flight`): its signed entry registered with three members, then up to three more of
/// those marked missed ([`aterm_net::fleet_member::heartbeat_members_with_denied`]), its
/// membership log admits (`log`; none: those its book binds) and `denied` does not list, the
/// places `browse`'s adverts resolve to tried after the book's, and its facts pushed to each that
/// answered. Its answers are noted in the network place's evidence under `state`
/// (`aterm_net::fleet_member`'s `note_place`), the state root this process's member desk reads
/// it under (`aterm_secrets::home_lan::home_net`), each marked logged only where the round ran
/// under the vault's membership log (`log`): a process that reads no vault (under a private state
/// root, [`vault_under`]) has none, so nothing it hears admits anyone. A generation that is no
/// longer `current` stops at the next exchange boundary and cannot supersede a newer generation.
#[cfg(unix)]
fn spawn_member_heartbeat_at(
    state: &std::path::Path,
    addr: SocketAddr,
    secret: aterm_net::fleet::FleetSecret,
    (current, in_flight): (&Arc<AtomicBool>, &Arc<AtomicBool>),
    (denied, log, browse): HeartbeatReach,
) {
    use aterm_net::fleet_member::{Registration, heartbeat_members_while, read_node_identity};
    if !current.load(Ordering::Acquire) {
        return;
    }
    let Some(me) = read_node_identity(state).ok().flatten() else {
        return;
    };
    let Some(host) = own_host(addr) else {
        return;
    };
    if in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let flag = Arc::clone(in_flight);
    let current = Arc::clone(current);
    let state = state.to_path_buf();
    let reg = Registration {
        node: me.node().to_owned(),
        host,
        port: addr.port(),
        version: aterm_stamp::app_version().to_owned(),
        key: None,
        signed: None,
    };
    let started = std::thread::Builder::new()
        .name("aterm-fleet-hb".to_owned())
        .spawn(move || {
            if !cfg!(test) {
                crate::qos::set_self(crate::qos::Role::Background);
            }
            struct Guard(Arc<AtomicBool>);
            impl Drop for Guard {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let _guard = Guard(flag);
            let within = heartbeat_within();
            let hb_deadline = std::time::Instant::now() + within;
            let hints = || {
                let left = hb_deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .min(HINTS_WITHIN);
                browse
                    .as_deref()
                    .map(|browse| alab_hints(browse, left))
                    .unwrap_or_default()
            };
            match heartbeat_members_while(
                &state,
                (&secret, &me),
                &reg,
                within,
                (log.as_deref(), &denied, &hints),
                &current,
            ) {
                Ok(answered) => {
                    if !current.load(Ordering::Acquire) {
                        return;
                    }
                    // Each answer proved its node to this Mac, and the round noted it in the
                    // network place's evidence under `state` (`aterm_net::fleet_member`'s
                    // `note_place`), which this Mac's member desk tells the home ALab LAN by.
                    if !answered.is_empty()
                        && let Some((_, payload)) =
                            aterm_net::fleet_member::read_pushed_facts(&state)
                                .into_iter()
                                .find(|(n, _)| n == me.node())
                    {
                        let keyring = aterm_net::fleet_member::Keyring::read(&state);
                        for done in &answered {
                            if !current.load(Ordering::Acquire) {
                                return;
                            }
                            let peer = aterm_net::fleet_member::Peer {
                                node: Some(done.node.clone()),
                                key: keyring.bound(&done.node),
                                key_from: None,
                                host: done.joined.issuer.host.clone(),
                                addr: Some(done.at.ip()),
                                port: done.at.port(),
                                version: Some(done.joined.issuer.version.clone()),
                                seen_ms: None,
                                via: aterm_net::fleet_member::Via::Joined,
                                fleet: None,
                                signed: None,
                            };
                            let _ = aterm_net::fleet_member::push_facts_to_bound_peer(
                                &peer,
                                (&secret, Some(&me)),
                                (&denied, log.as_deref()),
                                &payload,
                                Duration::from_secs(3),
                            );
                        }
                    }
                    aterm_log::debug!("fleet heartbeat: {} member(s) answered", answered.len())
                }
                Err(why) => aterm_log::debug!(
                    "fleet heartbeat failed: {}",
                    aterm_log::sanitize_record(&why)
                ),
            }
        });
    // A round that never started is in flight no more, so the next one due runs.
    if started.is_err() {
        in_flight.store(false, Ordering::Release);
    }
}

/// Follow `[fleet]` while `generation` listens at `addr`, as each connection is served
/// ([`served_now`]): hang up each open one it serves no more ([`cut_unserved_with_denied`]), lift
/// each hold of a node that may drive no more ([`lift_undriving_holds`]), and say where it stands
/// once that moved.
#[cfg(unix)]
fn follow_serving(generation: u64, addr: SocketAddr) {
    let serving = served_now();
    let denied = denied_nodes_now();
    // No deny list read yet: every open drive or content read is cut, as the handshake refuses
    // each new connection, until one is read.
    let unread = Serving {
        scope: FleetScope::Facts,
        grants: Vec::new(),
    };
    let cut = match denied.as_deref() {
        Some(denied) => cut_unserved_with_denied(&serving, &holds_now(), denied),
        None => cut_unserved_with_denied(&unread, &holds_now(), &[]),
    };
    if cut > 0 {
        note_fleet(
            &format!(
                "fleet listener: [fleet] took back a grant or lowered its level, or its books \
                 bind a driving node no more; the {cut} connection(s) it no longer serves are cut"
            ),
            false,
        );
    }
    lift_undriving_holds(
        &serving.grants,
        &keyring_now(),
        denied.as_deref().unwrap_or_default(),
    );
    stand_fleet(generation, Standing::Listening, &serving.line(addr));
}

fn wait_fleet_unchanged(generation: u64) -> bool {
    let desk = FLEET_DESK.lock_or_recover();
    let (desk, _) = FLEET_CHANGED
        .wait_timeout_while(desk, retry(), |desk| desk.generation == generation)
        .unwrap_or_else(PoisonError::into_inner);
    desk.generation == generation
}

/// [`wait_fleet_unchanged`], ended early too by a change of `[fleet]` in place past `revision`,
/// which it moves to.
fn wait_fleet_tick(generation: u64, revision: &mut u64) -> bool {
    let desk = FLEET_DESK.lock_or_recover();
    let (desk, _) = FLEET_CHANGED
        .wait_timeout_while(desk, retry(), |desk| {
            desk.generation == generation && desk.revision == *revision
        })
        .unwrap_or_else(PoisonError::into_inner);
    *revision = desk.revision;
    desk.generation == generation
}

fn arm_generation(generation: u64) -> Option<Arc<AtomicBool>> {
    let mut desk = FLEET_DESK.lock_or_recover();
    if desk.generation != generation {
        return None;
    }
    let running = Arc::new(AtomicBool::new(true));
    desk.stop = Some(Arc::clone(&running));
    Some(running)
}

fn stand_fleet(generation: u64, standing: Standing, line: &str) {
    let moved = {
        let mut desk = FLEET_DESK.lock_or_recover();
        if desk.generation != generation {
            return;
        }
        noted_move(&mut desk, standing, line)
    };
    if moved {
        note_fleet(
            line,
            !matches!(standing, Standing::Off | Standing::Listening),
        );
    }
}

/// Whether the listener, `standing` as `line` says, moved from where `desk` last stood: another
/// kind, or another place or level (a bind tried again, or a rekey where it was, says nothing new).
fn noted_move(desk: &mut FleetDesk, standing: Standing, line: &str) -> bool {
    let moved = desk.standing != standing || desk.said != line;
    desk.standing = standing;
    if moved {
        line.clone_into(&mut desk.said);
    }
    moved
}

fn fleet_event(event: FleetEvent) {
    use aterm_log::sanitize_record;
    match event {
        FleetEvent::Admitted { peer, op } => {
            aterm_log::debug!("fleet: {peer} admitted ({})", op.word());
        }
        FleetEvent::Closed { peer, error: None } => {
            aterm_log::debug!("fleet: {peer} closed");
        }
        FleetEvent::Closed {
            peer,
            error: Some(error),
        } => {
            aterm_log::warn!("fleet: {peer} closed: {}", sanitize_record(&error));
        }
        FleetEvent::Refused { peer, reason } => {
            let line = format!("fleet: {peer} not admitted: {}", sanitize_record(&reason));
            if let Some(line) = crate::control::control_denials::FLEET_REFUSALS
                .record_network(&line, std::time::Instant::now())
            {
                note_fleet(&line, true);
            }
        }
        FleetEvent::Stopped(why) => {
            note_fleet(
                &format!("fleet listener stopped: {}", sanitize_record(&why)),
                true,
            );
        }
    }
}

fn note_fleet(line: &str, refusal: bool) {
    crate::logging::stderr_line!("aterm-gui: {line}");
    if refusal {
        aterm_log::warn!("{line}");
    } else {
        aterm_log::info!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Which vault a process reads follows where its state root resolved, and nothing else
    /// (ATERM-KI-1105-10, [`vault_under`]): on the account's own state it reads the account's
    /// vault, with `CI` set as without; under a private root it reads none; on the account's own
    /// state with no vault to be found it reads none and says why. A test binary runs under its
    /// scratch root, so no test reads the person's vault.
    ///
    /// NEGATIVE CONTROL (2026-11-05): `vault_under` deciding by the environment
    /// (`private_root` or `ATERM_STATE_HOME` or `CI` set, a mutation inside the rule; the base
    /// read the environment in the predicate that feeds it, [`state_root_is_private`]'s place)
    /// fails "`CI` on the account's own state reads its vault". That predicate put back to the
    /// base's reading fails no test of this crate, as `cfg!(test)` holds it true here; `aterm`'s
    /// `unit_peers` test holds it.
    #[cfg(unix)]
    #[test]
    fn the_vault_a_process_reads_follows_where_its_state_root_resolved() {
        if !crate::test_alone::alone() {
            return;
        }
        let home = aterm_tempfile::tempdir().unwrap();
        let vault = aterm_secrets::client::Paths::for_home(home.path());
        let found = || Ok(vault.clone());
        crate::test_env::scoped("CI", "1", || {
            assert_eq!(
                vault_under(false, found),
                Ok(Some(vault.clone())),
                "`CI` on the account's own state reads its vault"
            );
        });
        assert_eq!(
            vault_under(false, found),
            Ok(Some(vault.clone())),
            "the account's own state reads its vault"
        );
        assert_eq!(
            vault_under(true, || panic!("a private state root reads no vault")),
            Ok(None),
            "a private state root reads none"
        );
        let unfound = vault_under(false, || {
            Err(aterm_secrets::Error::new("no home directory"))
        });
        assert!(
            unfound
                .as_ref()
                .is_err_and(|why| why.contains("no home directory")),
            "the account's own state with no vault to be found reads none and says why: \
             {unfound:?}"
        );
        assert!(
            aterm_types::dirs::runs_under_private_state_root() && state_root_is_private(),
            "PRECONDITION: a test binary runs under its scratch root"
        );
        assert_eq!(vault_here(), Ok(None), "a test reads no vault");
    }

    /// The window's grants admit by the vault the process reads ([`grants_under`] of the rule's
    /// answer, [`vault_under`], as [`window_grants`] reads them of [`vault_here`];
    /// ATERM-KI-1105-10): beside a fleet state taken under the vault configured they admit as
    /// made, beside one taken under another vault none does, `"*"` included, `CI` set or not, and
    /// on the account's own state with no vault to be found none does, as the node job then
    /// serves none, and why is said; under a private state root, which reads no vault, they admit
    /// as made.
    ///
    /// NEGATIVE CONTROLS (2026-11-05): `grants_under` reading a vault that cannot be found as
    /// none (its `Err` arm admitting by [`in_effect`] under no vault, the window's reading before
    /// 2026-11-05) fails "on the account's own state with no vault to be found no grant admits";
    /// `vault_under` deciding by the environment (as in the rule's own test) fails "beside a
    /// fleet taken under another vault no grant admits, `CI` set or not". A `window_grants` that
    /// takes its vault from anything but [`vault_here`] fails no test: in a test binary the rule
    /// reads no vault, as such a revert would.
    #[cfg(unix)]
    #[test]
    fn the_windows_grants_admit_only_beside_the_vault_its_fleet_was_taken_under() {
        if !crate::test_alone::alone() {
            return;
        }
        let root = aterm_tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        aterm_secrets::fsx::ensure_private_dir(&state).unwrap();
        aterm_net::fleet_member::write_held(&state, aterm_net::fleet_member::Began::Here, |book| {
            book.write_key(&[0x5a; 24]).expect("write fleet key");
        })
        .expect("write_held");
        let vault = aterm_secrets::client::Paths::for_home(root.path());
        let configure = |name: &str, signer: u8| {
            aterm_secrets::fsx::ensure_private_dir(&vault.dir).unwrap();
            aterm_secrets::fsx::write_atomic(
                &vault.dir.join("source.toml"),
                format!(
                    "url = \"http://127.0.0.1:9/v1/{name}\"\nname = \"{name}\"\nsigner = \"{}\"\n\
                     host = \"m22\"\n",
                    aterm_secrets::b64::encode_padded(&[signer; 32])
                )
                .as_bytes(),
                0o600,
            )
            .unwrap();
        };
        configure("alab", 1);
        aterm_agent::fleet_vault::keep(&state, Some(&vault));
        assert!(
            aterm_net::fleet_member::read_vault(&state)
                .unwrap()
                .is_some(),
            "PRECONDITION: the fleet state records the vault \"alab\""
        );
        let all = || {
            vec![
                aterm_net::fleet::DriveGrant::from_word(aterm_net::fleet::ALL_MEMBERS)
                    .expect("the default grant"),
            ]
        };
        let found = || Ok(vault.clone());
        let unfound = || Err(aterm_secrets::Error::new("no home directory"));
        /// `grants` beside `state` under the vault the rule names for `private_root` and `live`.
        fn under(
            grants: Vec<aterm_net::fleet::DriveGrant>,
            state: Option<&std::path::Path>,
            private_root: bool,
            live: impl FnOnce() -> aterm_secrets::Result<aterm_secrets::client::Paths>,
        ) -> (Vec<aterm_net::fleet::DriveGrant>, Option<String>) {
            let vault = vault_under(private_root, live);
            grants_under(grants, state, vault.as_ref().map(Option::as_ref))
        }
        let (admitted, said) = under(all(), Some(&state), false, found);
        assert_eq!(
            (admitted.len(), said),
            (1, None),
            "the grant made under the vault the fleet was taken under admits"
        );
        let (admitted, said) = under(all(), Some(&state), false, unfound);
        assert_eq!(
            admitted,
            Vec::new(),
            "on the account's own state with no vault to be found no grant admits"
        );
        assert!(
            said.as_deref()
                .is_some_and(|why| why.contains("no home directory")),
            "and why is said: {said:?}"
        );
        assert_eq!(
            under(Vec::new(), Some(&state), false, unfound),
            (Vec::new(), None),
            "with no grant there is nothing to say"
        );
        assert_eq!(
            under(all(), None, false, unfound).0,
            Vec::new(),
            "nor with no state directory"
        );
        configure("other", 9);
        crate::test_env::scoped("CI", "1", || {
            assert_eq!(
                under(all(), Some(&state), false, found).0,
                Vec::new(),
                "beside a fleet taken under another vault no grant admits, `CI` set or not"
            );
        });
        assert_eq!(
            under(all(), Some(&state), false, found),
            (Vec::new(), None),
            "beside a fleet taken under another vault no grant admits"
        );
        assert_eq!(
            under(all(), Some(&state), true, || panic!("reads no vault"))
                .0
                .len(),
            1,
            "under a private state root, which reads no vault, the grants admit as made"
        );
    }

    /// A `vault-join` is served on the vault its judge read, in the window and the node job alike
    /// ([`vault_join_on`]; ATERM-KI-1105-10): a process that reads no vault (under a private
    /// state root) refuses it, a judged one is served on that vault, and one its judge refused
    /// is refused with the judge's reason.
    ///
    /// NEGATIVE CONTROL (2026-11-05): `vault_join_on` serving a join whose judge read no vault on
    /// another vault anyway (the window read `Paths::live()` for it before; here a vault under a
    /// scratch path, so the control reads no person's vault) fails "a process that reads no vault
    /// refuses a vault-join".
    #[cfg(unix)]
    #[test]
    fn a_vault_join_is_served_only_on_the_vault_its_judge_read() {
        let home = aterm_tempfile::tempdir().unwrap();
        let vault = aterm_secrets::client::Paths::for_home(home.path());
        let judged = aterm_agent::vault_join::Judged {
            node: [7; 32],
            fleet: false,
            book: aterm_secrets::client::Book::default(),
        };
        let refused = vault_join_on(Ok(judged), None);
        assert!(
            refused
                .as_ref()
                .is_err_and(|why| why.contains("follows no vault")),
            "a process that reads no vault refuses a vault-join: {refused:?}"
        );
        assert_eq!(
            vault_join_on(Ok(judged), Some(vault.clone())),
            Ok((judged, vault.clone())),
            "a judged vault-join is served on the vault its judge read"
        );
        let why = "this Mac has no state directory".to_owned();
        assert_eq!(
            vault_join_on(Err(why.clone()), Some(vault)),
            Err(why),
            "one its judge refused is refused with the judge's reason"
        );
    }

    /// With MESH off a window still listens, leaving the port only to a node job that holds it;
    /// with MESH on it leaves the port to the node service, the one listener on this Mac.
    #[test]
    fn with_the_mesh_off_a_window_still_listens_and_with_it_on_leaves_the_port_to_the_node() {
        // MESH off, no node job: the window binds the port itself.
        assert_eq!(window_yields_port(false, || false), None);
        // MESH off, a node job holds the port: the window leaves it, as before the mesh.
        assert_eq!(
            window_yields_port(false, || true),
            Some("fleet served by systems.alab.aterm-node")
        );
        // MESH on: the node service is the one listener, held or not; the job is not even asked.
        assert!(
            window_yields_port(true, || unreachable!("no port lock is read"))
                .is_some_and(|line| line.contains("the one listener"))
        );
    }

    /// A detached book response that passed its early generation check must not publish its
    /// old listener endpoint after waiting on the descriptor lock and then being canceled.
    #[cfg(unix)]
    #[test]
    fn an_ended_book_response_keeps_the_current_listener_descriptor() {
        use aterm_net::fleet_member::{hold_book, lock_waiting_told, node_identity, self_entry};

        let state = aterm_tempfile::tempdir().unwrap();
        let me = node_identity(state.path()).unwrap();
        self_entry(
            state.path(),
            &me,
            ("127.0.0.1", 7200, aterm_stamp::app_version()),
        )
        .unwrap();
        let descriptor = state.path().join("fleet-self-entry");
        let committed = std::fs::read(&descriptor).unwrap();
        let current = AtomicBool::new(true);
        let within = aterm_spec::wait::MUST_HAPPEN;
        let (waiting, waited) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let held = hold_book(state.path()).unwrap();
            let worker = scope.spawn(|| {
                let mut response = Vec::new();
                let result = lock_waiting_told(within, waiting, || {
                    share_book_at(
                        &mut response,
                        state.path(),
                        None,
                        (&[], None),
                        ("127.0.0.1:7100".parse().unwrap(), &current),
                    )
                });
                (result, response)
            });
            waited.recv_timeout(within).unwrap();
            current.store(false, Ordering::Release);
            drop(held);
            let (result, response) = worker.join().unwrap();
            assert!(result.is_err());
            assert!(response.is_empty());
            assert_eq!(std::fs::read(descriptor).unwrap(), committed);
        });
    }

    #[cfg(unix)]
    #[test]
    fn wildcard_heartbeats_keep_the_automatic_joins_lan_address() {
        let routes = [
            Some("10.0.0.2".parse().unwrap()),
            Some("192.168.1.102".parse().unwrap()),
            Some("2001:db8::1".parse().unwrap()),
        ];
        assert_eq!(
            own_host_from("0.0.0.0:7100".parse().unwrap(), || routes).as_deref(),
            Some("192.168.1.102")
        );
        assert_eq!(
            own_host_from("0.0.0.0:7100".parse().unwrap(), || [None; 3]),
            None
        );
        for address in ["127.0.0.1:7100", "[::1]:7100", "192.168.1.2:7100"] {
            let address: SocketAddr = address.parse().unwrap();
            assert_eq!(
                own_host_from(address, || panic!("an explicit bind needs no route")),
                Some(address.ip().to_string())
            );
        }
    }

    /// What a Mac whose keyring binds each node as `bound` says holds: a node under that key.
    fn held_by(
        bound: impl Fn(&str) -> Option<aterm_net::fleet::NodeKey>,
    ) -> impl Fn(&str, aterm_net::fleet::NodeKey) -> bool {
        move |node, key| bound(node) == Some(key)
    }

    /// A drive the membership log admits is held, as its admission was, with no book entry for its
    /// node here; a book that binds the node to another key the log admits is that member's, and
    /// a key the log took out is held nowhere.
    /// NEGATIVE CONTROL: judge by the keyring alone (`bound_in(keys, log, node) == Some(key)`):
    /// the log-admitted drive with no book entry is not held, and the cut hangs it up.
    #[cfg(unix)]
    #[test]
    fn a_drive_the_log_admits_is_held_with_no_book_entry_for_its_node() {
        use aterm_net::fleet::NodeKey;
        use aterm_net::fleet_member::{Keyring, MemberKeys, Peer, Standing, Via};
        let (member, other, removed) = (
            NodeKey::from_bytes([0x31; 32]),
            NodeKey::from_bytes([0x32; 32]),
            NodeKey::from_bytes([0x33; 32]),
        );
        let node = "6d".repeat(16);
        let log: MemberKeys = [
            (member, Standing::Member),
            (other, Standing::Member),
            (removed, Standing::Out("removed".to_owned())),
        ]
        .into_iter()
        .collect();
        let none = Keyring::default();
        assert!(
            holds_in(&none, Some(&log), &node, member),
            "a key the log admits, with no book entry for its node"
        );
        assert!(
            !holds_in(&none, None, &node, member),
            "with no log, the book decides"
        );
        assert!(
            !holds_in(&none, Some(&log), &node, removed),
            "a key the log took out"
        );
        let bound_to = |key: NodeKey| {
            Keyring::of(&[Peer {
                node: Some(node.clone()),
                key: Some(key),
                key_from: None,
                host: "m9.local".to_owned(),
                addr: None,
                port: 7100,
                version: None,
                seen_ms: None,
                via: Via::Joined,
                fleet: None,
                signed: None,
            }])
        };
        assert!(
            !holds_in(&bound_to(other), Some(&log), &node, member),
            "the book binds the node to another member's key"
        );
        assert!(
            holds_in(&bound_to(removed), Some(&log), &node, member),
            "the book still binds the node to the key the log took out"
        );
        let star = Serving {
            scope: FleetScope::Content,
            grants: vec![
                aterm_net::fleet::DriveGrant::from_word(aterm_net::fleet::ALL_MEMBERS)
                    .expect("the all-member grant"),
            ],
        };
        let driver =
            aterm_net::fleet::DriveGrant::from_word(&format!("{node}:{}", "31".repeat(32)))
                .expect("the driver's grant");
        assert!(star.serves(
            true,
            Some(&driver),
            &|node, key| holds_in(&none, Some(&log), node, key),
            &[]
        ));

        // Remote holds (`lift_undriving_holds` and `remote_holder`) keep a log-admitted driver's
        // hold while the log admits its key (even with no book entry or an old retired key in the
        // book), and lift it once the log takes its key out or `peers rm` removes a book-only
        // driver.
        assert_eq!(
            bound_or_admitted_in(&none, Some(&log), Some(member), &node),
            Some(member),
            "log-admitted driver with no book entry keeps its bound key for remote holds"
        );
        assert!(still_drives(
            &star.grants,
            || bound_or_admitted_in(&none, Some(&log), Some(member), &node),
            &node,
            &[],
        ));
        assert_eq!(
            bound_or_admitted_in(&bound_to(removed), Some(&log), Some(member), &node),
            Some(member),
            "log-admitted driver whose book entry still holds its retired key keeps its hold"
        );
        assert_eq!(
            bound_or_admitted_in(&bound_to(removed), Some(&log), Some(removed), &node),
            None,
            "a Mac the membership log removed drives no more even while the book names it"
        );
        let founder = NodeKey::from_bytes([0x34; 32]);
        assert_eq!(
            bound_or_admitted_in(&bound_to(founder), Some(&log), Some(founder), &node),
            Some(founder),
            "a book-admitted founder not yet in the log keeps its hold while in the book"
        );
        assert_eq!(
            bound_or_admitted_in(&none, Some(&log), Some(founder), &node),
            None,
            "a book-only driver loses its hold once peers rm removes it from the book"
        );
        // Negative controls: `bound_in` alone drops the log-admitted driver with no book entry,
        // and `keys.bound` alone keeps a Mac the membership log removed.
        assert_eq!(bound_in(&none, Some(&log), &node), None);
        assert_eq!(bound_to(removed).bound(&node), Some(removed));
    }

    /// A Commons peer heard again after a silence makes one fleet heartbeat due at once, named
    /// by its source, between the interval's own; the next look makes none, and the interval's
    /// own still comes.
    /// NEGATIVE CONTROL: unhook the clock from the signal (a `due` that reads only the interval),
    /// or the beacon source from the Commons peers (a look that hears no one): no heartbeat is
    /// due before the interval, and the wake reason reads `Timer`.
    #[cfg(unix)]
    #[test]
    fn a_new_commons_beacon_sends_one_heartbeat() {
        use crate::network_moved::tests::{HANG, peer};
        use crate::network_moved::{Heard, Moved, Woke, spawn_source};
        use aterm_types::network_state::Reachability::{LastSeen, Reachable};
        let moved = std::sync::Arc::new(Moved::new());
        let now = std::time::Instant::now();
        let every = Duration::from_secs(3600);
        let mut heartbeat = HeartbeatClock::new(Some(now), every, moved.watch());
        let mut seen = moved.watch();
        let (looks, looked) = std::sync::mpsc::channel();
        let mut heard = Heard::new(1);
        assert!(spawn_source(
            "test-commons",
            std::sync::Arc::clone(&moved),
            (Woke::Beacon, Duration::ZERO),
            move || {
                let peers: Vec<_> = looked.recv().ok()?;
                Some(heard.look(&peers))
            }
        ));
        assert_eq!(heartbeat.due(now), None, "the interval has not run out");
        looks
            .send(vec![peer(7, LastSeen)])
            .expect("the beacon source runs");
        looks
            .send(vec![peer(7, Reachable)])
            .expect("the beacon source runs");
        assert_eq!(seen.wait(HANG), Woke::Beacon);
        assert_eq!(
            heartbeat.due(now),
            Some(Woke::Beacon),
            "one heartbeat at once"
        );
        assert_eq!(heartbeat.due(now), None, "and only one");
        assert_eq!(heartbeat.due(now + every), Some(Woke::Timer));
    }

    /// A HEARTBEAT A MOVE MAKES DUE WAITS OUT THIS MAC'S SPREAD (docs/DESIGN-fluid-swarm-2026-10-28.md,
    /// F4's follow-up): with ~35 Macs back on the LAN at once, each registers with the issuer at
    /// its own offset after the move, not at the move's instant; then once, as before.
    /// NEGATIVE CONTROL: a `due` that takes the move by the wall clock (`take()`) or a signal
    /// that ignores its spread fails one of the first two: the heartbeat comes before the spread,
    /// or not at it.
    #[cfg(unix)]
    #[test]
    fn a_heartbeat_a_move_makes_due_waits_out_this_macs_spread() {
        use crate::network_moved::{Moved, Woke};
        let spread = Duration::from_millis(2_500);
        let moved = std::sync::Arc::new(Moved::spread_by(spread));
        let t0 = std::time::Instant::now() + Duration::from_secs(3600);
        let every = Duration::from_secs(3 * 3600);
        let mut heartbeat = HeartbeatClock::new(Some(t0), every, moved.watch());
        moved.tell_at(Woke::Route, t0);
        assert_eq!(heartbeat.due(t0), None, "not before this Mac's spread");
        assert_eq!(
            heartbeat.due(t0 + spread),
            Some(Woke::Route),
            "at this Mac's spread"
        );
        assert_eq!(heartbeat.due(t0 + spread), None, "and only one");
    }

    /// A MOVE TOLD WHILE A HEARTBEAT ROUND IS OUT STILL GETS A ROUND: back home alone (no herd
    /// spread), the route move starts a round at once, often while the LAN is still down, and
    /// the settled place's move comes seconds later while that round is out. It is not taken
    /// then (a round could not start), and once the round ends it is due, once.
    /// NEGATIVE CONTROL: `due_when_free` taking the move whatever `busy` says (the old
    /// `heartbeat.due(now)` at the call sites) answers it while the round is out, where the
    /// caller drops it, and "not while a round is out" fails.
    #[cfg(unix)]
    #[test]
    fn a_move_told_while_a_round_is_out_still_gets_a_round() {
        use crate::network_moved::{Moved, Woke};
        let moved = std::sync::Arc::new(Moved::new());
        let t0 = std::time::Instant::now() + Duration::from_secs(3600);
        let every = Duration::from_secs(3 * 3600);
        let mut heartbeat = HeartbeatClock::new(Some(t0), every, moved.watch());
        moved.tell_at(Woke::Place, t0);
        assert_eq!(
            heartbeat.due_when_free(t0, true),
            None,
            "not while a round is out"
        );
        assert_eq!(
            heartbeat.due_when_free(t0 + Duration::from_secs(3), false),
            Some(Woke::Place),
            "once it ended"
        );
        assert_eq!(
            heartbeat.due_when_free(t0 + Duration::from_secs(3), false),
            None,
            "and only one"
        );
    }

    /// A DRIVE IS NAMED BY ITS PROVEN NODE.
    #[cfg(unix)]
    #[test]
    fn a_drive_is_named_by_the_node_it_proved() {
        let (n, m) = ("1a2b3c4d".repeat(4), "9f8e7d6c".repeat(4));
        let knows = [n.clone(), m.clone()];
        assert_eq!(
            driver_name(&n, Some("mac-11.local"), &knows),
            "mac-11.local (node 1a2b3c4d)"
        );
        assert_ne!(
            driver_name(&n, Some("mac-11.local"), &knows),
            driver_name(&m, Some("mac-11.local"), &knows),
            "a node that names itself as another Mac reads as itself"
        );
        for unshown in [None, Some("the message bus"), Some("")] {
            assert_eq!(
                driver_name(&n, unshown, &knows),
                "node 1a2b3c4d",
                "{unshown:?}"
            );
        }
        // NEGATIVE CONTROL: name the node by its first eight characters again, and the holder
        // reads as the node it shares them with.
        let (holder, reader) = (
            "c0ffee34a18b0000000000000000000a".to_owned(),
            "c0ffee34a27c0000000000000000000b".to_owned(),
        );
        assert_eq!(
            driver_name(&holder, Some("127.0.0.1"), &[holder.clone(), reader]),
            "127.0.0.1 (node c0ffee34a1)",
            "a head another node this Mac knows shares is never the name"
        );
    }

    /// THE JOIN'S AUDIT LINE NAMES THE MEMBER AS THE BOOK DOES.
    #[cfg(unix)]
    #[test]
    fn a_registration_names_the_member_by_its_fleet_address_and_node() {
        use aterm_net::fleet_member::{Peer, Recorded, Via};
        let member = Peer {
            node: Some("6f860a011103430d".to_owned()),
            key: None,
            key_from: None,
            host: "lab-mini".to_owned(),
            addr: Some(std::net::IpAddr::from([127, 0, 0, 1])),
            port: 58054,
            version: Some("0.105.0".to_owned()),
            seen_ms: Some(1),
            via: Via::Joined,
            fleet: None,
            signed: None,
        };
        for recorded in [Recorded::New, Recorded::Updated] {
            let note = registration_note(recorded, &member);
            assert!(note.contains(" 127.0.0.1:58054 "), "{note}");
            assert!(note.contains("6f860a011103430d"), "{note}");
        }
        assert_ne!(
            registration_note(Recorded::New, &member),
            registration_note(Recorded::Updated, &member)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_held_place_names_the_act_that_moves_the_listener() {
        let addr = SocketAddr::from(([127, 0, 0, 1], 58733));
        let dir = aterm_tempfile::tempdir().expect("scratch folder");
        let state = dir.path().join("state");
        aterm_uds::private_file::ensure_private_dir(&state).unwrap();
        let held = unbound_line(addr, std::io::ErrorKind::AddrInUse, Some(&state));
        assert!(
            held.starts_with("fleet listener: 127.0.0.1:58733 is held by another process")
                && held.contains(": `aterm fleet join --port <n>` moves it; tried again every"),
            "{held}"
        );
        aterm_uds::private_file::write_private(
            &state,
            aterm_net::fleet_member::CODE_FILE,
            b"code\n",
        )
        .unwrap();
        let issuer = unbound_line(addr, std::io::ErrorKind::AddrInUse, Some(&state));
        assert!(
            issuer.contains(": `aterm fleet init --rotate --port <n>` moves it (every member"),
            "{issuer}"
        );
        let absent = unbound_line(addr, std::io::ErrorKind::AddrNotAvailable, Some(&state));
        assert!(
            absent.contains("is not an address of this machine yet") && !absent.contains("moves"),
            "{absent}"
        );
    }

    #[test]
    fn every_move_of_the_listener_is_noted() {
        let mut desk = FleetDesk {
            wanted: None,
            generation: 0,
            revision: 0,
            configured: false,
            started: false,
            thread_spawned: false,
            standing: Standing::Off,
            said: String::new(),
            stop: None,
            mesh: false,
        };
        let at = |place: &str, level: &str| format!("fleet listening on {place} (serving {level})");
        assert!(noted_move(
            &mut desk,
            Standing::Listening,
            &at("127.0.0.1:52510", "facts")
        ));
        assert!(
            !noted_move(
                &mut desk,
                Standing::Listening,
                &at("127.0.0.1:52510", "facts")
            ),
            "a rekey where it was"
        );
        assert!(noted_move(
            &mut desk,
            Standing::Listening,
            &at("127.0.0.1:52506", "facts")
        ));
        assert!(noted_move(
            &mut desk,
            Standing::Listening,
            &at("0.0.0.0:52506", "content")
        ));
        let held = "fleet listener: 0.0.0.0:52506 is held by another process";
        assert!(noted_move(&mut desk, Standing::Retrying, held));
        assert!(
            !noted_move(&mut desk, Standing::Retrying, held),
            "tried again"
        );
        assert_eq!(desk.standing, Standing::Retrying);
    }

    #[test]
    fn the_listener_follows_listen_and_allow() {
        let table = |listen: Option<&str>, allow: Option<&str>| FleetConfig {
            listen: listen.map(str::to_owned),
            allow: allow.map(str::to_owned),
            watch: None,
            drive_from: None,
        };
        assert_eq!(fleet_wanted(None), None);
        assert_eq!(fleet_wanted(Some(&table(None, Some("content")))), None);
        assert_eq!(fleet_wanted(Some(&table(Some(""), None))), None);
        let on = fleet_wanted(Some(&table(Some("0.0.0.0:7100"), None)));
        assert_eq!(
            on,
            Some(Wanted {
                listen: "0.0.0.0:7100".to_owned(),
                allow: None,
                drive_from: Vec::new(),
            })
        );
        assert_ne!(
            on,
            fleet_wanted(Some(&table(Some("0.0.0.0:7100"), Some("content"))))
        );
        assert_ne!(on, fleet_wanted(Some(&table(Some("0.0.0.0:7101"), None))));
    }

    #[test]
    fn the_drive_level_serves_only_the_nodes_its_list_names() {
        let node = "ab".repeat(16);
        let grant = format!("{node}:{}", "cd".repeat(32));
        let wanted = |allow: Option<&str>, from: &[&str]| {
            fleet_wanted(Some(&FleetConfig {
                listen: Some("0.0.0.0:7100".to_owned()),
                allow: allow.map(str::to_owned),
                watch: None,
                drive_from: Some(from.iter().map(|s| (*s).to_owned()).collect()),
            }))
            .expect("a listener")
        };
        assert_eq!(level_of(Some("drive")), Some((FleetScope::Content, true)));
        assert_eq!(level_of(Some("DRIVE")), Some((FleetScope::Content, true)));
        for (allow, scope) in [
            (None, FleetScope::Facts),
            (Some("facts"), FleetScope::Facts),
            (Some("content"), FleetScope::Content),
        ] {
            assert_eq!(level_of(allow), Some((scope, false)), "{allow:?}");
        }
        assert_eq!(level_of(Some("steer")), None);
        let listed = wanted(Some("drive"), &[&node, "mac-03", "", "zz", &grant]);
        assert_eq!(
            listed.drive_from,
            vec![aterm_net::fleet::DriveGrant::from_word(&grant).expect("a grant")],
            "only a node with its key grants"
        );
        assert_eq!(listed.drive_from[0].node, node);
        assert!(
            wanted(Some("drive"), &[]).drive_from.is_empty(),
            "none named: nobody"
        );
    }

    #[test]
    fn a_grant_written_applies_at_once_and_cuts_only_what_it_takes_away() {
        let grant = |n: u8| {
            aterm_net::fleet::DriveGrant::from_word(&format!(
                "{}:{}",
                format!("{n:02x}").repeat(16),
                "cd".repeat(32)
            ))
            .expect("a grant")
        };
        let (a, b) = (grant(0x0a), grant(0x0b));
        let toml = |listen: &str, allow: &str, from: &[&aterm_net::fleet::DriveGrant]| {
            format!(
                "theme = \"Nord\"\n[fleet]\nlisten = \"{listen}\"\nallow = \"{allow}\"\n\
                 drive_from = [{}]\n",
                from.iter()
                    .map(|grant| format!("\"{}\"", grant.word()))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let wanted = |text: &str| table_in(text).expect("a [fleet] that reads");
        let running = wanted(&toml("127.0.0.1:7100", "drive", &[&b]));
        let now = |text: &str| serving_now(running.as_ref(), Some(text));
        let drive = |grants: Vec<aterm_net::fleet::DriveGrant>| Serving {
            scope: FleetScope::Content,
            grants,
        };
        let both = toml("127.0.0.1:7100", "drive", &[&a, &b]);
        assert_eq!(
            now(&both),
            drive(vec![a.clone(), b.clone()]),
            "a grant added drives at once"
        );
        assert_eq!(
            now(&toml("127.0.0.1:7100", "DRIVE", &[&b])),
            drive(vec![b.clone()])
        );
        assert_eq!(
            serving_now(
                wanted(&toml("127.0.0.1:7100", "content", &[])).as_ref(),
                Some(&both)
            ),
            drive(vec![a.clone(), b.clone()]),
            "so does the first, which raises the level to drive"
        );
        assert_eq!(
            now(&toml("127.0.0.1:7100", "facts", &[&a, &b])),
            Serving {
                scope: FleetScope::Facts,
                grants: Vec::new()
            }
        );
        let unrelated = format!(
            "font_px = \"not a number\"\n{}",
            toml("127.0.0.1:7100", "content", &[])
        );
        for gone in [
            toml("127.0.0.1:7100", "content", &[]),
            toml("", "drive", &[&a, &b]),
            toml("127.0.0.1:7100", "steer", &[&a, &b]),
            "theme = \"Nord\"\n".to_owned(),
            String::new(),
            unrelated,
        ] {
            assert_eq!(now(&gone), drive(Vec::new()), "{gone}");
        }
        for broken in [
            "[fleet\n".to_owned(),
            "theme = Nord\n".to_owned(),
            "[fleet]\nallow = 5\n".to_owned(),
        ] {
            assert_eq!(
                now(&broken),
                drive(vec![b.clone()]),
                "a [fleet] that does not read leaves the running grants: {broken}"
            );
        }
        assert_eq!(
            serving_now(running.as_ref(), None),
            drive(vec![b.clone()]),
            "so does an aterm.toml that cannot be read"
        );

        let bound = |node: &str| {
            [&a, &b]
                .into_iter()
                .find(|grant| grant.node == node)
                .map(|grant| grant.key)
        };
        let served = now(&toml("127.0.0.1:7100", "drive", &[&b]));
        assert!(
            served.serves(true, Some(&b), &held_by(bound), &[]),
            "a drive whose grant stands"
        );
        assert!(
            served.serves(true, None, &held_by(bound), &[])
                && served.serves(false, None, &held_by(bound), &[])
        );
        assert!(
            !served.serves(true, Some(&a), &held_by(bound), &[]),
            "a drive whose grant went"
        );
        let facts = now(&toml("127.0.0.1:7100", "facts", &[]));
        assert!(
            !facts.serves(true, None, &held_by(bound), &[])
                && facts.serves(false, None, &held_by(bound), &[])
        );
        assert!(!now(&toml("127.0.0.1:7100", "content", &[])).serves(
            true,
            Some(&b),
            &held_by(bound),
            &[]
        ));

        let place = |listen: &str, allow: &str| wanted(&toml(listen, allow, &[&a]));
        let here = place("127.0.0.1:7100", "content");
        for stays in [
            place("127.0.0.1:7100", "drive"),
            place("127.0.0.1:7100", "facts"),
        ] {
            assert!(in_place(here.as_ref(), stays.as_ref()), "{stays:?}");
        }
        for moves in [
            place("127.0.0.1:7101", "content"),
            place("127.0.0.1:7100", "steer"),
            None,
        ] {
            assert!(!in_place(here.as_ref(), moves.as_ref()), "{moves:?}");
        }
        assert!(!in_place(None, here.as_ref()));
    }

    #[test]
    fn a_node_drives_on_while_a_grant_names_it_with_the_key_it_is_bound_to() {
        let grant = |n: u8, key: u8| {
            aterm_net::fleet::DriveGrant::from_word(&format!(
                "{}:{}",
                format!("{n:02x}").repeat(16),
                format!("{key:02x}").repeat(32)
            ))
            .expect("a grant")
        };
        let (a, b) = (grant(0x0a, 0xcd), grant(0x0b, 0xcd));
        let grants = [a.clone(), b];
        let (key, other) = (a.key, grant(0x0a, 0xef).key);
        assert!(still_drives(&grants, || Some(key), &a.node, &[]));
        assert!(
            !still_drives(&grants, || Some(other), &a.node, &[]),
            "its issuer's book binds it to another key"
        );
        assert!(
            !still_drives(&grants, || None, &a.node, &[]),
            "its issuer's book names it no more"
        );
        assert!(
            !still_drives(
                &grants,
                || unreachable!("no grant names it"),
                &"0c".repeat(16),
                &[],
            ),
            "no grant names it"
        );
        assert!(!still_drives(&[], || Some(key), &a.node, &[]));
        let all = aterm_net::fleet::DriveGrant::from_word(aterm_net::fleet::ALL_MEMBERS)
            .expect("the all-member grant");
        let every = [all.clone()];
        assert!(still_drives(&every, || Some(other), &a.node, &[]));
        assert!(!still_drives(&every, || None, &a.node, &[]));
        let drive = Serving {
            scope: FleetScope::Content,
            grants: vec![all],
        };
        let (c, bound) = (grant(0x0c, 0xef), |node: &str| {
            (node == a.node).then_some(key)
        });
        assert!(drive.serves(true, Some(&a), &held_by(bound), &[]));
        assert!(
            !drive.serves(true, Some(&c), &held_by(bound), &[]),
            "under `*`, a drive of a node no book here binds"
        );
        assert!(
            !drive.serves(true, Some(&a), &|_, key| key == other, &[]),
            "under `*`, a drive admitted under a key its node is bound to no more"
        );
        let named = Serving {
            grants: vec![a.clone()],
            ..drive.clone()
        };
        assert!(named.serves(true, Some(&a), &held_by(bound), &[]));
        assert!(
            !named.serves(true, Some(&a), &|_, _| false, &[]),
            "under its own grant, a drive of a node no book here binds"
        );
        assert!(
            !Serving {
                grants: vec![grant(0x0a, 0xef)],
                ..drive
            }
            .serves(true, Some(&a), &held_by(bound), &[]),
            "a named grant of another key"
        );
    }

    #[cfg(unix)]
    static OPEN_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[cfg(unix)]
    #[test]
    fn a_change_that_takes_a_connection_away_as_it_is_admitted_refuses_or_cuts_it() {
        let _guard = OPEN_TEST_LOCK.lock_or_recover();
        let grant = aterm_net::fleet::DriveGrant::from_word(&format!(
            "{}:{}",
            "5e".repeat(16),
            "cd".repeat(32)
        ))
        .expect("a grant");
        let drive = |grants: Vec<aterm_net::fleet::DriveGrant>| Serving {
            scope: FleetScope::Content,
            grants,
        };
        let entered = |grant: &aterm_net::fleet::DriveGrant| {
            OPEN.lock_or_recover()
                .iter()
                .any(|open| open.driver.as_ref() == Some(grant))
        };
        let (near, _far) = aterm_uds::CtlStream::pair().expect("a line");
        let bound = |_: &str| Some(grant.key);
        let refused = Opened::served(
            true,
            Some(grant.clone()),
            &near,
            || drive(Vec::new()),
            &held_by(bound),
        )
        .expect("held");
        assert!(
            refused.is_none() && !entered(&grant),
            "its grant taken back while its admission read the issuer's book"
        );
        let facts = || Serving {
            scope: FleetScope::Facts,
            grants: Vec::new(),
        };
        assert!(
            Opened::served(true, None, &near, facts, &held_by(bound))
                .expect("held")
                .is_none(),
            "a read at content once it serves facts"
        );
        let open = Opened::served(
            true,
            Some(grant.clone()),
            &near,
            || drive(vec![grant.clone()]),
            &held_by(bound),
        )
        .expect("held")
        .expect("its grant stands");
        assert!(entered(&grant));
        assert!(
            cut_unserved(&drive(Vec::new()), &held_by(bound)) >= 1 && !entered(&grant),
            "taken back once it is held: the cut that follows hangs it up"
        );
        drop(open);
        let all = || {
            drive(vec![
                aterm_net::fleet::DriveGrant::from_word(aterm_net::fleet::ALL_MEMBERS)
                    .expect("the all-member grant"),
            ])
        };
        let open = Opened::served(true, Some(grant.clone()), &near, all, &held_by(bound))
            .expect("held")
            .expect("`*` admits a node bound here");
        cut_unserved(&all(), &held_by(bound));
        assert!(
            entered(&grant),
            "`*` serves a node bound here to its key still"
        );
        assert!(
            cut_unserved(&all(), &|_, _| false) >= 1 && !entered(&grant),
            "under `*`, a drive whose node no book binds any more is cut too"
        );
        drop(open);
    }

    #[test]
    fn the_slots_are_a_share_of_the_descriptor_limit() {
        let most = aterm_net::fleet::MAX_SLOTS;
        assert_eq!(slots_for(Some(256), 64), 8, "a Dock-launched app's 256");
        assert_eq!(slots_for(Some(512), 128), most);
        assert_eq!(
            slots_for(None, 1024),
            most,
            "an unreadable limit bounds nothing"
        );
        assert_eq!(slots_for(Some(1 << 20), 8), 8, "the control server's cap");
        assert_eq!(slots_for(Some(16), 8), 1, "a configured listener serves");
    }

    /// `units=` SAYS WHERE LAN SHARING STANDS, AS THE DOCTOR'S ROW DECIDES IT
    /// (`atpkg::peers::Serves`), one word for each place.
    #[test]
    fn the_units_word_says_where_lan_sharing_stands() {
        use atpkg::peers::Serves;
        let at = SocketAddr::from(([10, 0, 0, 2], atpkg::peers::DEFAULT_PORT));
        let words = [
            Some(Serves::Serving(at)),
            Some(Serves::Idle),
            Some(Serves::NoMember),
            Some(Serves::Off),
            None,
        ]
        .map(units_word);
        assert_eq!(words, ["serving", "idle", "no-member", "off", "-"]);
        assert_eq!(
            units_word(Some(Serves::of(true, Some(at), 3))),
            "off",
            "the person's off stands over a live server"
        );
    }

    /// `units=` READS NO FILE ON THE EVENT LOOP: one read is out at a time, started once none has
    /// landed or the last is [`UNITS_WORD`]'s `every` old; a `status` waits for it until `wait`
    /// after it started, and answers a word younger than `stale`, else `-`.
    #[test]
    fn the_units_word_is_read_off_the_event_loop_one_read_at_a_time() {
        let UnitsTiming { every, wait, stale } = UNITS_WORD;
        let t0 = Instant::now();
        let mut held = UnitsWord::UNREAD;
        assert!(held.ask(t0, every), "none read: the first asks");
        assert!(!held.ask(t0, every), "one read at a time");
        assert_eq!(held.wait_left(t0, wait), Some(wait));
        assert_eq!(held.wait_left(t0 + wait, wait), None, "waited for once");
        assert_eq!(held.answer(t0, stale), "-", "none landed");
        held.landed(Some("serving"), t0);
        assert_eq!(held.wait_left(t0, wait), None, "none out");
        assert!(!held.ask(t0 + every / 2, every));
        assert_eq!(held.answer(t0 + every / 2, stale), "serving");
        assert!(held.ask(t0 + every, every), "a word this old is read again");
        held.landed(None, t0 + every);
        assert!(
            held.ask(t0 + every, every),
            "a read that never ran keeps the word and is asked again"
        );
        assert_eq!(held.answer(t0 + every, stale), "serving");
        assert_eq!(
            held.answer(t0 + stale, stale),
            "-",
            "a word as old as the bound is no answer while its read is out"
        );
        held.landed(Some("off"), t0 + stale);
        assert!(!held.ask(t0 + stale, every));
        assert_eq!(held.answer(t0 + stale, stale), "off");
    }

    /// A `status` AFTER A QUIET SPELL SAYS WHAT THE STORE HOLDS NOW, NOT THE WORD LAST READ: the
    /// read it starts lands before it answers (the person's off over an earlier `serving`).
    #[test]
    fn a_status_after_a_quiet_spell_answers_the_read_it_started() {
        static DESK: UnitsDesk = UnitsDesk::new();
        DESK.land(Some("serving"));
        let reads = std::sync::atomic::AtomicUsize::new(0);
        let read_off = |desk: &'static UnitsDesk| {
            reads.fetch_add(1, Ordering::Relaxed);
            std::thread::spawn(move || desk.land(Some("off")));
            true
        };
        // The word in hand is already old (`every` zero); the wait is a hang detector, and no
        // answer here is held to a clock.
        let hang = Duration::from_secs(60);
        let quiet = UnitsTiming {
            every: Duration::ZERO,
            wait: hang,
            stale: hang,
        };
        assert_eq!(DESK.word(quiet, read_off), "off");
        assert_eq!(reads.load(Ordering::Relaxed), 1);
        let fresh = UnitsTiming {
            every: hang,
            ..quiet
        };
        assert_eq!(
            DESK.word(fresh, |_| unreachable!("the word is fresh")),
            "off",
            "a fresh word is answered as it stands"
        );
        assert_eq!(
            DESK.word(quiet, |_| false),
            "off",
            "a read that could not start keeps the word while it is young"
        );
    }

    /// Facts stay on the Mac that made them and go to the fleet issuer its book names, never one
    /// known by a name, so neither case reads this host's own names.
    #[cfg(unix)]
    #[test]
    fn facts_push_stores_locally_or_over_fleet_and_rejects_forged_node() {
        use aterm_agent::survey_facts::SwarmSurveyFacts;
        use aterm_net::fleet::{FleetOp, FleetScope, FleetSecret, Listening, NodeIdentity};
        use aterm_net::fleet_member::{
            Began, Peer, Via, node_identity, read_pushed_facts, serve_facts, write_held,
        };

        let secret = FleetSecret::new(&[0x44; 24]).expect("secret");
        let holder_dir = aterm_tempfile::tempdir().expect("holder dir");
        let member_dir = aterm_tempfile::tempdir().expect("member dir");

        let holder_id: NodeIdentity = node_identity(holder_dir.path()).expect("holder node");
        let member_id: NodeIdentity = node_identity(member_dir.path()).expect("member node");

        // 1. The issuer (its book names no issuer but itself) keeps its own facts alone.
        let holder_facts = SwarmSurveyFacts {
            release_lines: Vec::new(),
            node: Some(holder_id.node().to_owned()),
            label: Some("fixture-holder".to_owned()),
            roster_unread: false,
            version: "0.146.0".to_owned(),
            window: true,
            window_last: Some(1_793_160_000),
            sessions: 2,
            ssh_host_key: Some("SHA256:holderkey".to_owned()),
            accounts: Vec::new(),
            accounts_unread: Vec::new(),
            session_accounts: std::collections::BTreeMap::new(),
            moves: None,
            server_heard: Vec::new(),
            home: None,
            home_free_mb: None,
            repos: None,
        };
        let holder_payload = holder_facts.to_survey_reply(
            100,
            Some("fixture-holder.local"),
            1_793_160_000_000,
            1,
            false,
        );
        assert_eq!(
            push_facts_to_issuer(holder_dir.path(), &secret, &holder_payload).expect("local store"),
            FactsPushOutcome::StoredLocally(holder_id.node().to_owned()),
        );
        assert_eq!(
            read_pushed_facts(holder_dir.path()),
            vec![(holder_id.node().to_owned(), holder_payload.clone())],
        );

        // 2. A member (`m7`) dials the issuer its book names over `FleetOp::Facts`.
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().unwrap().port();
        let running = Arc::new(AtomicBool::new(true));
        let holder_state = holder_dir.path().to_path_buf();
        let listening = Listening {
            secret: secret.clone(),
            scope: FleetScope::Facts,
            node: Some(holder_id.clone()),
            serves: SERVES,
            slots: 4,
            per_source: 4,
            lifetime: aterm_spec::wait::MUST_HAPPEN,
        };
        let server_running = Arc::clone(&running);
        let server = std::thread::spawn(move || {
            aterm_net::fleet::serve_offering_with_ask_within(
                &listener,
                (
                    &listening,
                    Arc::new(|| (FleetScope::Facts, SERVES)),
                    Arc::new(|| Arc::new(aterm_net::fleet::Denied::default())),
                ),
                None,
                &server_running,
                aterm_spec::wait::MUST_HAPPEN,
                move |served, op| {
                    assert_eq!(op, FleetOp::Facts);
                    let proof = served.peer_node().cloned();
                    let state = holder_state.clone();
                    served.answer(|stream| {
                        let _ = serve_facts(stream, proof.as_ref(), &state, |payload| {
                            SwarmSurveyFacts::from_survey_reply(payload).map(|f| f.node)
                        })?;
                        Ok(())
                    })
                },
                |_| {},
            );
        });

        write_held(member_dir.path(), Began::Here, |book| {
            book.write_key(&[0x44; 24]).unwrap();
            book.update(|peers| {
                peers.push(Peer {
                    host: "fixture-holder.local".to_owned(),
                    addr: Some("127.0.0.1".parse().unwrap()),
                    port,
                    via: Via::Issuer,
                    node: Some(holder_id.node().to_owned()),
                    version: Some("0.146.0".to_owned()),
                    seen_ms: Some(1_793_160_000_000),
                    key: Some(holder_id.key()),
                    key_from: None,
                    fleet: None,
                    signed: None,
                });
                Ok(())
            })
            .unwrap();
        })
        .unwrap();

        let member_facts = SwarmSurveyFacts {
            release_lines: Vec::new(),
            node: Some(member_id.node().to_owned()),
            label: Some("m7".to_owned()),
            roster_unread: false,
            version: "0.146.0".to_owned(),
            window: false,
            window_last: Some(1_793_150_000),
            sessions: 0,
            ssh_host_key: Some("SHA256:memberkey".to_owned()),
            accounts: Vec::new(),
            accounts_unread: Vec::new(),
            session_accounts: std::collections::BTreeMap::new(),
            moves: None,
            server_heard: Vec::new(),
            home: None,
            home_free_mb: None,
            repos: None,
        };
        let member_payload =
            member_facts.to_survey_reply(200, Some("m7.local"), 1_793_160_000_000, 0, false);
        assert_eq!(
            push_facts_to_issuer(member_dir.path(), &secret, &member_payload).expect("remote push"),
            FactsPushOutcome::PushedRemote {
                host: "fixture-holder.local".to_owned(),
                port,
            },
        );

        // The member keeps its own facts besides, for the census it computes (F17).
        assert_eq!(
            read_pushed_facts(member_dir.path()),
            vec![(member_id.node().to_owned(), member_payload.clone())],
        );

        // Negative control 1: forging another node's `node=` on `member_id`'s connection is rejected.
        let mut forged = member_facts;
        forged.node = Some(holder_id.node().to_owned());
        let forged_payload =
            forged.to_survey_reply(200, Some("m7.local"), 1_793_160_000_000, 0, false);
        let err = push_facts_to_issuer(member_dir.path(), &secret, &forged_payload)
            .expect_err("forged node id must be refused");
        assert!(err.contains("ERR node-mismatch"), "{err}");

        // Negative control 2: spoofing the listener's own `node_id` with a different Ed25519 key
        // is rejected with `ERR key-mismatch` and never overwrites `holder_dir`'s own facts.
        let spoof_holder =
            NodeIdentity::from_seed(holder_id.node(), &[0x99; 32]).expect("spoof holder id");
        let spoof_err = aterm_net::fleet_member::push_facts_to_peer(
            "fixture-holder.local",
            Some("127.0.0.1".parse().unwrap()),
            port,
            (&secret, Some(&spoof_holder)),
            &forged_payload,
            aterm_spec::wait::MUST_HAPPEN,
        )
        .expect_err("spoofed holder key must be refused");
        assert!(spoof_err.contains("ERR key-mismatch"), "{spoof_err}");

        running.store(false, Ordering::Release);
        let _ = std::net::TcpStream::connect(("127.0.0.1", port));
        let _ = server.join();

        let stored = read_pushed_facts(holder_dir.path());
        assert_eq!(stored.len(), 2);
        assert!(
            stored
                .iter()
                .any(|(n, p)| n == holder_id.node() && p == &holder_payload)
        );
        assert!(
            stored
                .iter()
                .any(|(n, p)| n == member_id.node() && p == &member_payload)
        );
    }

    /// A local `hold off` ends a remote hold whose node no grant here admits (or that this Mac
    /// cannot be driven by at all) while no deny list was ever read, without waiting for one;
    /// where the answer turns on the deny list, the hold stands while none was read, and a list
    /// that names the node ends it; otherwise the dial judges.
    ///
    /// NEGATIVE CONTROL (2026-11-01): with `holder_after` reading the deny list before it looks at
    /// `ready` (the order the review found: an unread list stood every hold, even one no grant
    /// admits), this fails (`left: Again(..), right: Gone("it may drive this Mac no more")`).
    #[cfg(unix)]
    #[test]
    fn a_hold_off_ends_an_ungranted_remote_hold_before_any_deny_list_is_read() {
        use crate::fabric::Holder;
        let node = "9a".repeat(16);
        let key = aterm_net::fleet::NodeKey::from_bytes([0xcd; 32]);
        let unread = || None;
        let dialed = |_| -> Holder { panic!("no dial where the deny list decides") };
        assert_eq!(
            holder_after(
                &node,
                Err("it may drive this Mac no more"),
                |k| *k,
                unread,
                dialed
            ),
            Holder::Gone("it may drive this Mac no more".to_owned())
        );
        assert_eq!(
            holder_after(&node, Ok(key), |k| *k, unread, dialed),
            Holder::Again(DENY_LIST_UNREAD.to_owned())
        );
        let naming = || Some(Arc::from(vec![node.clone()]));
        assert_eq!(
            holder_after(&node, Ok(key), |k| *k, naming, dialed),
            Holder::Gone("it is on the swarm roster's deny-list".to_owned())
        );
        assert_eq!(
            holder_after(
                &node,
                Ok(key),
                |k| *k,
                || Some(Arc::from(Vec::new())),
                |_| Holder::Drives
            ),
            Holder::Drives
        );
    }

    /// A deny list that stays unread says why, once for each new cause and never again for the
    /// same one (the read runs per dialer and per tick), and says so once more when a list is
    /// read at last; a failed read after that keeps the list and records its new cause once.
    ///
    /// NEGATIVE CONTROL (2026-11-01): with `update_denied_nodes_cache` saying nothing while the
    /// list is unread (the shape before: a standing cause looked like "try again", logged at
    /// `debug!` where it was logged at all), this fails (`an unread list says why`).
    #[cfg(unix)]
    #[test]
    fn an_unread_deny_list_says_why_once_for_each_cause() {
        use aterm_secrets::client::{
            SwarmDenialSnapshot as Snapshot, SwarmDenialUnavailable as Why,
        };

        let mut cache = DeniedCache::new();
        let failed = |why: &str| Err(aterm_secrets::Error::new(why.to_owned()));
        let (denied, said) =
            update_denied_nodes_cache(&mut cache, failed(".lock: owned by uid 0, not this user"));
        assert!(denied.is_none());
        let said = said.expect("an unread list says why");
        assert!(
            said.contains(DENY_LIST_UNREAD) && said.contains("owned by uid 0"),
            "{said}"
        );
        assert_eq!(
            update_denied_nodes_cache(&mut cache, failed(".lock: owned by uid 0, not this user")),
            (None, None),
            "the same cause is said once"
        );
        let said =
            update_denied_nodes_cache(&mut cache, Ok(Snapshot::Unavailable(Why::WriterBusy))).1;
        assert!(
            said.is_some_and(|said| said.contains(&Why::WriterBusy.to_string())),
            "a new cause is said again"
        );
        let (denied, said) =
            update_denied_nodes_cache(&mut cache, Ok(Snapshot::Available(Vec::new())));
        assert!(denied.is_some());
        assert!(
            said.is_some_and(|said| said.contains("admitted again")),
            "a list read at last is said"
        );
        let (denied, said) = update_denied_nodes_cache(&mut cache, failed("transient"));
        assert!(
            denied.is_some(),
            "a failed read retains the authoritative list"
        );
        assert!(
            said.is_some_and(|said| said.contains("transient")),
            "a new retained-list cause is recorded"
        );
        assert!(
            update_denied_nodes_cache(&mut cache, failed("transient"))
                .1
                .is_none(),
            "a repeated failure is quiet"
        );
        assert!(
            update_denied_nodes_cache(&mut cache, Ok(Snapshot::Available(Vec::new())))
                .1
                .is_some(),
            "recovery is recorded once"
        );
        assert!(
            update_denied_nodes_cache(&mut cache, Ok(Snapshot::Available(Vec::new())))
                .1
                .is_none(),
            "an unchanged current list is quiet"
        );
    }

    /// No deny list until one was read, then the last one read through a mid-apply or failed
    /// read, and an open drive cut once its node is denied.
    ///
    /// NEGATIVE CONTROL (2026-11-01): with `update_denied_nodes_cache` answering an empty list
    /// while none was read (the shape before), the first read is a list (`assertion failed:
    /// update_denied_nodes_cache(&mut cache, Ok(None)).is_none()`).
    #[cfg(unix)]
    #[test]
    fn denied_nodes_cache_retains_list_while_locked_and_cuts_open_drive() {
        use aterm_secrets::client::{
            SwarmDenialSnapshot as Snapshot, SwarmDenialUnavailable as Why,
        };

        let _guard = OPEN_TEST_LOCK.lock_or_recover();
        let mut cache = DeniedCache::new();
        // Never read: a mid-apply read and a failed one leave no list, which the handshake reads
        // as unread and refuses every dialer under, never as a list that names no one.
        assert!(
            update_denied_nodes_cache(&mut cache, Ok(Snapshot::Unavailable(Why::WriterBusy)))
                .0
                .is_none()
        );
        let failed =
            update_denied_nodes_cache(&mut cache, Err(aterm_secrets::Error::new("wide"))).0;
        assert!(failed.is_none());
        assert!(denied_under(failed.as_deref(), None).unread);

        let revoked_node = "9a".repeat(16);
        let good_node = "9b".repeat(16);
        let updated = update_denied_nodes_cache(
            &mut cache,
            Ok(Snapshot::Available(vec![revoked_node.clone()])),
        )
        .0
        .expect("a list read now");
        assert_eq!(&*updated, std::slice::from_ref(&revoked_node));
        assert!(!denied_under(Some(&updated), None).unread);

        // A transient lock or parse error retains the last authoritative deny-list.
        assert_eq!(
            update_denied_nodes_cache(&mut cache, Ok(Snapshot::Unavailable(Why::WriterBusy)))
                .0
                .as_deref(),
            Some(std::slice::from_ref(&revoked_node))
        );
        assert_eq!(
            update_denied_nodes_cache(&mut cache, Err(aterm_secrets::Error::new("transient")))
                .0
                .as_deref(),
            Some(std::slice::from_ref(&revoked_node))
        );

        // Open drive for `revoked_node` is cut as soon as it enters `deny_nodes`, while `good_node`
        // (negative control) remains open under `"*"`.
        let grant_of = |node: &str, key_byte: u8| {
            aterm_net::fleet::DriveGrant::from_word(&format!(
                "{node}:{}",
                format!("{key_byte:02x}").repeat(32)
            ))
            .expect("grant")
        };
        let revoked_grant = grant_of(&revoked_node, 0xcd);
        let good_grant = grant_of(&good_node, 0xef);
        let all = Serving {
            scope: FleetScope::Content,
            grants: vec![
                aterm_net::fleet::DriveGrant::from_word(aterm_net::fleet::ALL_MEMBERS)
                    .expect("all"),
            ],
        };
        let bound = |node: &str| {
            if node == revoked_node {
                Some(revoked_grant.key)
            } else if node == good_node {
                Some(good_grant.key)
            } else {
                None
            }
        };
        let (near_revoked, _far_revoked) = aterm_uds::CtlStream::pair().expect("pair");
        let (near_good, _far_good) = aterm_uds::CtlStream::pair().expect("pair");
        let open_revoked = Opened::served_with_denied(
            true,
            Some(revoked_grant.clone()),
            &near_revoked,
            || all.clone(),
            &held_by(bound),
            &[],
        )
        .expect("io")
        .expect("admitted before revocation");
        let open_good = Opened::served_with_denied(
            true,
            Some(good_grant.clone()),
            &near_good,
            || all.clone(),
            &held_by(bound),
            &[],
        )
        .expect("io")
        .expect("admitted good node");

        let cut =
            cut_unserved_with_denied(&all, &held_by(bound), std::slice::from_ref(&revoked_node));
        assert_eq!(cut, 1, "only the denied node's open drive must be cut");
        let still_open = |grant: &aterm_net::fleet::DriveGrant| {
            OPEN.lock_or_recover()
                .iter()
                .any(|o| o.driver.as_ref() == Some(grant))
        };
        assert!(!still_open(&revoked_grant));
        assert!(still_open(&good_grant));
        drop((open_revoked, open_good));
    }
}

#[cfg(unix)]
#[path = "node_serve.rs"]
pub(crate) mod node_serve;
