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
use aterm_uds::stop::Stop;

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

    /// Start a due round only while free. A refusal acknowledges no move and starts
    /// no new interval. An accepted dispatch consumes only its selected generation;
    /// a newer move arriving inside `start` remains pending for the next round.
    pub(crate) fn dispatch_when_free(
        &mut self,
        now: std::time::Instant,
        busy: bool,
        start: impl FnOnce() -> bool,
    ) -> Option<crate::network_moved::Woke> {
        if busy {
            return None;
        }
        let pending = self.moved.pending_at(now);
        let woke = if let Some((_, woke)) = pending {
            woke
        } else if self
            .last
            .is_none_or(|last| now.saturating_duration_since(last) >= self.every)
        {
            crate::network_moved::Woke::Timer
        } else {
            return None;
        };
        if !start() {
            return None;
        }
        if let Some((generation, _)) = pending {
            self.moved.acknowledge(generation);
        }
        self.last = Some(now);
        Some(woke)
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
/// does. A vault-tenure mismatch also answers `None`, independently of any cached list. Theirs
/// is a second line, which
/// no test reaches (a test reads no vault). The gate and the cache are what the tests hold
/// (`denied_nodes_cache_retains_list_while_locked_and_cuts_open_drive`, and the node job's
/// `unknown_custody_cannot_use_a_cached_deny_list`).
#[cfg(unix)]
fn denied_nodes_now() -> Option<Arc<[String]>> {
    let state = aterm_types::dirs::state_dir()?;
    let vault = vault_here().ok()?;
    if !aterm_agent::fleet_vault::traffic_in_effect(&state, vault.as_ref()) {
        return None;
    }
    let observed = match vault {
        None => return Some(Arc::from(&[][..])),
        Some(paths) => aterm_secrets::client::swarm_denial_snapshot_now(&paths),
    };
    let (denied, said) = update_denied_nodes_cache(&mut DENIED_NODES.lock_or_recover(), observed);
    if let Some(line) = said {
        note_fleet(&line, denied.is_none());
    }
    denied
}

#[cfg(unix)]
fn capture_traffic_here() -> Option<aterm_agent::fleet_vault::Traffic> {
    let state = aterm_types::dirs::state_dir()?;
    let vault = vault_here().ok()?;
    aterm_agent::fleet_vault::Traffic::capture(&state, vault.as_ref())
}

/// Both listener hosts bind live membership policy to the take captured by their generation.
#[cfg(unix)]
fn denial_under_take(
    traffic: aterm_agent::fleet_vault::Traffic,
    policy: impl Fn() -> Arc<aterm_net::fleet::Denied> + Send + Sync + 'static,
) -> aterm_net::fleet::DenyFn {
    Arc::new(move || {
        if !traffic.current() {
            return Arc::new(aterm_net::fleet::Denied::withdrawn());
        }
        let verdict = policy();
        if traffic.current() {
            verdict
        } else {
            Arc::new(aterm_net::fleet::Denied::withdrawn())
        }
    })
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

/// Ordinary fleet handshakes and mesh links require custody as well as peer membership.
/// Withdrawal is distinct from an unread peer log: an existing link must end immediately
/// when its table judges the local fleet against another vault.
#[cfg(unix)]
pub(crate) fn denied_for_fleet(
    state: &std::path::Path,
    vault: Option<&aterm_secrets::client::Paths>,
    denied_nodes: Option<&[String]>,
    log: Option<&aterm_net::fleet_member::MemberKeys>,
) -> Arc<aterm_net::fleet::Denied> {
    if aterm_agent::fleet_vault::tenure(state, vault) != aterm_agent::fleet_vault::Tenure::Here {
        Arc::new(aterm_net::fleet::Denied::withdrawn())
    } else {
        denied_under(denied_nodes, log)
    }
}

#[cfg(unix)]
fn window_denied() -> Arc<aterm_net::fleet::Denied> {
    let (Some(state), Ok(vault)) = (aterm_types::dirs::state_dir(), vault_here()) else {
        return Arc::new(aterm_net::fleet::Denied::withdrawn());
    };
    denied_for_fleet(
        &state,
        vault.as_ref(),
        denied_nodes_now().as_deref(),
        window_log().as_deref(),
    )
}

/// Hand this process's fleet dialers the verdict this Mac's listener judges each dialer by
/// ([`denied_for_fleet`] of local custody, the swarm's deny list and [`window_log`], read for each dial), so with the
/// mesh switch on both ends of this Mac judge a peer's key by one standing
/// (`aterm_net::fleet_member::judge_mesh_by`). Inert with the switch off.
#[cfg(unix)]
pub fn judge_mesh_dials() {
    aterm_net::fleet_member::judge_mesh_by(Arc::new(window_denied));
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

/// What this process has seen of this Mac's fleet access ([`observe_access`]).
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct AccessSeen {
    /// The level at its last look; `None` before the first.
    level: Option<aterm_types::fleet_access::FleetAccess>,
    /// A drop below Read and Write this process saw, whose remote holds still stand: when it
    /// has rested long enough for them to be lifted.
    resting: Option<Instant>,
    /// The menu-bar lock holds a raise its person already clicked for: the drop is on its way
    /// back up, and the holds wait for where it settles.
    raising: bool,
}

/// What one look at the level found ([`access_seen`]).
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AccessLook {
    /// The level changed since the last look: from, to.
    change: Option<(
        aterm_types::fleet_access::FleetAccess,
        aterm_types::fleet_access::FleetAccess,
    )>,
    /// A drop below Read and Write has rested: how many remote holds were lifted.
    lifted: Option<usize>,
    /// When to look again: a drop that has not rested yet.
    again: Option<Instant>,
}

#[cfg(unix)]
static ACCESS_SEEN: Mutex<AccessSeen> = Mutex::new(AccessSeen {
    level: None,
    resting: None,
    raising: false,
});

/// This process sees this Mac's fleet access at `level`, stated `age` ago where the file says
/// when (each fleet line a window serves, and the App after its own write, on the change
/// notification and at its lock's clock). A change since the last look is journalled; `by`
/// says what changed it, where the caller made the change (`the menu-bar lock`).
///
/// A drop below Read and Write refuses every write at once, and lifts every remote hold once
/// the level has rested there for [`aterm_types::fleet_access::SETTLE`], as a revoked grant
/// does ([`lift_undriving_holds`]): its holder can lift it no more, so this Mac does. The rest
/// is the lock's own rule turned round: a click moves the level one step on, so the way from
/// Full Access down to Read and Write passes Read Only, and a hold another Mac's person placed
/// is not ended by a level that was on the bar for a second. `raising` says the lock holds a
/// raise its person already clicked for (`None`: as last told); the holds then wait for where
/// the level settles. Answers when to look again, where a drop is still resting: the App wakes
/// for it, and a window with no status item looks at its next fleet line.
#[cfg(unix)]
#[cfg_attr(
    test,
    expect(
        dead_code,
        reason = "a test's level is its thread's, never this process's: tests call access_seen"
    )
)]
pub(crate) fn observe_access(
    level: aterm_types::fleet_access::FleetAccess,
    age: Option<Duration>,
    by: Option<&str>,
    raising: Option<bool>,
) -> Option<Instant> {
    let mut seen = ACCESS_SEEN.lock().unwrap_or_else(|p| p.into_inner());
    let look = access_seen(&mut seen, (level, age), raising, Instant::now(), &|| {
        crate::control::lift_ungranted_remote_holds(&|_| false)
    });
    drop(seen);
    if let Some((was, now)) = look.change {
        let by = by.map(|by| format!(" ({by})")).unwrap_or_default();
        note_fleet(
            &format!("fleet: access {} \u{2192} {}{by}", was.word(), now.word()),
            false,
        );
    }
    if let Some(lifted) = look.lifted.filter(|lifted| *lifted > 0) {
        note_fleet(
            &format!(
                "fleet: {lifted} remote hold(s) lifted: fleet access rests at {}, so no other \
                 Mac may hand them back",
                level.label()
            ),
            false,
        );
    }
    look.again
}

/// [`observe_access`] over what was `seen` before, at `now`: the change of level, the remote
/// holds `lift` lifted (it answers how many) where a drop below Read and Write has rested, and
/// when to look again. The first look finds no change: nothing changed that this process saw.
#[cfg(unix)]
fn access_seen(
    seen: &mut AccessSeen,
    (level, age): (aterm_types::fleet_access::FleetAccess, Option<Duration>),
    raising: Option<bool>,
    now: Instant,
    lift: &dyn Fn() -> usize,
) -> AccessLook {
    use aterm_types::fleet_access::{FleetAccess, SETTLE};
    if let Some(raising) = raising {
        seen.raising = raising;
    }
    let change = seen
        .level
        .replace(level)
        .filter(|was| *was != level)
        .map(|was| (was, level));
    if level >= FleetAccess::ReadWrite {
        seen.resting = None;
    } else if change.is_some_and(|(was, _)| was >= FleetAccess::ReadWrite) {
        // Rested already for as long as the file says the level has stood.
        seen.resting = Some(now + SETTLE.saturating_sub(age.unwrap_or_default()));
    }
    let lifted = match seen.resting {
        Some(due) if due <= now && !seen.raising => {
            seen.resting = None;
            Some(lift())
        }
        _ => None,
    };
    AccessLook {
        change,
        lifted,
        again: seen.resting.filter(|_| !seen.raising),
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
    /// The serving generation's stop.
    stop: Option<Arc<Stop>>,
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
        stop.set();
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
        let Some(traffic) = capture_traffic_here() else {
            stand_fleet(
                generation,
                Standing::Refused("vault"),
                "fleet traffic paused: this Mac's fleet tenure does not match its current vault",
            );
            if wait_fleet_unchanged(generation) {
                continue;
            }
            return;
        };
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
        let Some(stop) = arm_generation(generation) else {
            return;
        };
        let node = read_node_key();
        let rekeyed = Arc::new(AtomicBool::new(false));
        let watching = std::thread::Builder::new()
            .name("aterm-fleet-watch".to_owned())
            .spawn({
                let (stop, rekeyed, secret) =
                    (Arc::clone(&stop), Arc::clone(&rekeyed), secret.clone());
                let traffic = traffic.clone();
                let proved = node
                    .as_ref()
                    .map(|node| (node.node().to_owned(), node.key()));
                move || {
                    watch_fleet_generation(
                        (generation, addr),
                        (&stop, &rekeyed),
                        (&secret, proved),
                        &traffic,
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
        let sharing = share_units(addr, &stop);
        let serving_generation = Arc::clone(&stop);
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
                denial_under_take(traffic, window_denied),
            ),
            members_ask(),
            &stop,
            within,
            move |served, op| {
                let peer = served.peer_addr();
                if op == FleetOp::Join {
                    let proof = served.peer_node().cloned();
                    return served.answer(|stream| {
                        stream.set_read_timeout(Some(within))?;
                        let authorize = stream.authorization();
                        register_member(
                            stream,
                            (peer, proof.as_ref()),
                            (addr, &join_secret),
                            &|| authorize().is_ok(),
                        )
                    });
                }
                if op == FleetOp::Book {
                    return served.answer(|stream| {
                        stream.set_write_timeout(Some(within))?;
                        let authorize = stream.authorization();
                        share_book(stream, peer, (addr, &serving_generation), &|| {
                            authorize().is_ok()
                        })
                    });
                }
                if op == FleetOp::Facts {
                    let proof = served.peer_node().cloned();
                    return served.answer(|stream| {
                        stream.set_read_timeout(Some(within))?;
                        stream.set_write_timeout(Some(within))?;
                        let authorize = stream.authorization();
                        receive_pushed_facts(stream, proof.as_ref(), peer, &|| authorize().is_ok())
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
                        crate::control::DriveRequester::OnStream,
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
                let reader = crate::fleet_activity::Reader::of(served.peer_node());
                if !crate::control::attach_fleet_connection(near, (content, drive), peer, reader) {
                    return Err(std::io::Error::other(
                        "this aterm's control server is not serving",
                    ));
                }
                served.relay_answers(far)
            },
            fleet_event,
        );
        let stopped_itself = stop.set();
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

/// LAN TOOLSET SHARING, on by default (docs/OWNER_RULINGS.md, 2026-10-27): while a fleet
/// generation listens, serve this Mac's kept toolset
/// archives ([`atpkg::peers::share`]) on the fleet listener's IP at
/// [`atpkg::peers::DEFAULT_PORT`], each connection from a fleet member's LAN address alone
/// ([`unit_members`], read again at most every [`aterm_net::fleet::KEY_READ_EVERY`]), until
/// `stop` is set; `aterm pkg serve-units --off` stops it. It opens only once the fleet
/// listener listens in this process, so macOS's firewall has already ruled on this app's
/// incoming connections and asks nothing new. `None` where atpkg is inert, no store resolves,
/// or the fleet's guard refuses the address ([`aterm_net::loopback::check`]: an isolated debug
/// drive's loopback, and every address of a private aterm's, since the port is the person's).
#[cfg(unix)]
fn share_units(listen: SocketAddr, stop: &Arc<Stop>) -> Option<std::thread::JoinHandle<()>> {
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
    let stop = Arc::clone(stop);
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
                    stop: &stop,
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
    admit_proven_driver_at(served.peer_node(), state, grants, denied, log)
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
    admit_proven_driver_at(Some(proof), state, grants, denied, log)
}

#[cfg(unix)]
fn admit_proven_driver_at(
    proof: Option<&aterm_net::fleet::NodeProof>,
    state: &std::path::Path,
    grants: &[aterm_net::fleet::DriveGrant],
    denied: &[String],
    log: Option<&aterm_net::fleet_member::MemberKeys>,
) -> Result<(crate::fabric::Driver, aterm_net::fleet::DriveGrant), (String, bool)> {
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
    // Each node named by a head no other node this Mac knows shares, from one read of its book.
    let (verdict, knows) =
        aterm_net::fleet_member::recheck_driver_and_peer(proof, state, grants, log);
    let (node, peer) = verdict.map_err(|why| (why.said(&knows), why.ungranted()))?;
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
    let host = driver_name(&node, peer.map(|peer| peer.host).as_deref(), &knows);
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
    let (driven, bound, peers) = match proof {
        Some(proof)
            if !aterm_net::fleet::is_denied_node(&proof.node, denied)
                && !aterm_net::fleet::is_denied_key(&proof.key, denied) =>
        {
            aterm_net::fleet_member::vault_join_driver_and_book(proof, state, grants, log)
        }
        _ => (false, None, None),
    };
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
    current: &dyn Fn() -> bool,
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
        current,
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
    current: &dyn Fn() -> bool,
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
        current,
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
    (addr, ended): (SocketAddr, &Stop),
    current: &dyn Fn() -> bool,
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
        (addr, ended),
        current,
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
    (addr, ended): (SocketAddr, &Stop),
    current: &dyn Fn() -> bool,
) -> std::io::Result<()> {
    use aterm_net::fleet_member::{
        read_node_id, read_node_identity, self_entry_while, serve_book_with_log,
    };
    let gone = || std::io::Error::other("the fleet listener generation ended");
    if ended.is_set() || !current() {
        return Err(gone());
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
                ended,
                current,
            )
        })
        .transpose()?
        .flatten();
    if ended.is_set() || !current() {
        return Err(gone());
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
    current: &dyn Fn() -> bool,
) -> std::io::Result<()> {
    let Some(state) = aterm_types::dirs::state_dir() else {
        return Err(std::io::Error::other(
            "a facts push with no state directory",
        ));
    };
    let denied = denied_nodes_now().ok_or_else(|| std::io::Error::other(DENY_LIST_UNREAD))?;
    let log = window_log();
    receive_pushed_facts_at(
        stream,
        &state,
        proof,
        peer,
        (&denied, log.as_deref()),
        current,
    )
}

#[cfg(unix)]
fn receive_pushed_facts_at<S: std::io::Read + std::io::Write>(
    stream: &mut S,
    state: &std::path::Path,
    proof: Option<&aterm_net::fleet::NodeProof>,
    peer: Option<SocketAddr>,
    (denied, log): (&[String], Option<&aterm_net::fleet_member::MemberKeys>),
    current: &dyn Fn() -> bool,
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
        current,
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
    push_facts_to_issuer_with_denied(state_dir, secret, (&[], None), payload, &|| true)
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
    current: &dyn Fn() -> bool,
) -> Result<FactsPushOutcome, String> {
    use aterm_net::fleet_member::{
        Via, push_facts_to_bound_peer, read_node_identity, read_peers, store_pushed_facts_with_key,
    };
    if !current() {
        return Err("fleet custody changed".into());
    }
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
        current,
    )?;
    Ok(FactsPushOutcome::PushedRemote {
        host: peer.host.clone(),
        port: peer.port,
    })
}

#[cfg(unix)]
fn push_facts_heartbeat(
    secret: &aterm_net::fleet::FleetSecret,
    traffic: &aterm_agent::fleet_vault::Traffic,
) {
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
        (here.access, false),
    );
    // No deny list read yet: this Mac's facts go to no one until one is.
    let Some(denied) = denied_nodes_now() else {
        aterm_log::debug!("fleet: survey facts heartbeat push skipped: {DENY_LIST_UNREAD}");
        return;
    };
    let log = window_log();
    if let Err(why) = push_facts_to_issuer_with_denied(
        &state,
        secret,
        (&denied, log.as_deref()),
        &payload,
        &|| traffic.current(),
    ) {
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
    (stop, rekeyed): (&Arc<Stop>, &AtomicBool),
    (secret, proved): (
        &aterm_net::fleet::FleetSecret,
        Option<(String, aterm_net::fleet::NodeKey)>,
    ),
    traffic: &aterm_agent::fleet_vault::Traffic,
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
    while wait_fleet_tick(generation, &mut revision) && !stop.is_set() {
        if node_job_running_now() {
            rekeyed.store(true, Ordering::Release);
            stop.set();
            note_fleet(
                "fleet listener: systems.alab.aterm-node started; handing over the fleet port",
                false,
            );
            return;
        }
        if !traffic.current() {
            rekeyed.store(true, Ordering::Release);
            stop.set();
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
                    push_facts_heartbeat(secret, traffic);
                }
                if last_census_refresh.elapsed() >= CENSUS_REFRESH_EVERY {
                    last_census_refresh = std::time::Instant::now();
                    maybe_refresh_local_census();
                }
                if let Some(woke) = heartbeat.dispatch_when_free(now, busy, || {
                    spawn_member_heartbeat(addr, secret.clone(), (stop, &hb_in_flight), traffic)
                }) {
                    say_heartbeat_woke("fleet listener", woke);
                }
                continue;
            }
            Ok(now) if now == *secret => format!("{NODE_FILE} or {NODE_KEY_FILE} changed"),
            Ok(_) => format!("{KEY_FILE} changed"),
            Err(error) => error.to_string(),
        };
        rekeyed.store(true, Ordering::Release);
        stop.set();
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
    (ended, in_flight): (&Arc<Stop>, &Arc<AtomicBool>),
    traffic: &aterm_agent::fleet_vault::Traffic,
) -> bool {
    let Some(state) = aterm_types::dirs::state_dir() else {
        return false;
    };
    // Widen only an existing matching vault record.
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
        return false;
    };
    spawn_member_heartbeat_at(
        &state,
        addr,
        secret,
        (ended, in_flight),
        (denied, log, crate::network_moved::alab_tcp()),
        traffic.clone(),
    )
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
/// root, [`vault_under`]) has none, so nothing it hears admits anyone. A generation whose stop
/// (`ended`) is set stops at the next exchange boundary and cannot supersede a newer generation.
#[cfg(unix)]
fn spawn_member_heartbeat_at(
    state: &std::path::Path,
    addr: SocketAddr,
    secret: aterm_net::fleet::FleetSecret,
    (ended, in_flight): (&Arc<Stop>, &Arc<AtomicBool>),
    (denied, log, browse): HeartbeatReach,
    traffic: aterm_agent::fleet_vault::Traffic,
) -> bool {
    use aterm_net::fleet_member::{Registration, heartbeat_members_while, read_node_identity};
    if ended.is_set() {
        return false;
    }
    let Some(me) = read_node_identity(state).ok().flatten() else {
        return false;
    };
    let Some(host) = own_host(addr) else {
        return false;
    };
    if in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }
    let flag = Arc::clone(in_flight);
    let ended = Arc::clone(ended);
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
            let current = || traffic.current();
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
                &ended,
                &current,
            ) {
                Ok(answered) => {
                    if ended.is_set() || !current() {
                        return;
                    }
                    // Each answer proved its node to this Mac, and the round noted it in the
                    // network place's evidence under `state` (`aterm_net::fleet_member`'s
                    // `note_place`), which this Mac's member desk tells the home ALab LAN by.
                    if !answered.is_empty()
                        && let Some(payload) =
                            aterm_net::fleet_member::read_pushed_facts_of(&state, me.node())
                    {
                        let keyring = aterm_net::fleet_member::Keyring::read(&state);
                        for done in &answered {
                            if ended.is_set() || !current() {
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
                                &current,
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
    started.is_ok()
}

/// Follow `[fleet]` while `generation` listens at `addr`, as each connection is served
/// ([`served_now`]): hang up each open one it serves no more ([`cut_unserved_with_denied`]), lift
/// each hold of a node that may drive no more ([`lift_undriving_holds`]), and say where it stands
/// once that moved.
#[cfg(unix)]
fn follow_serving(generation: u64, addr: SocketAddr) {
    let serving = served_now();
    let denied = denied_nodes_now();
    let held = std::cell::OnceCell::new();
    let keys_and_log = || {
        held.get_or_init(|| {
            (
                aterm_types::dirs::state_dir()
                    .map(|state| aterm_net::fleet_member::Keyring::read(&state))
                    .unwrap_or_default(),
                window_log(),
            )
        })
    };
    // No deny list read yet: every open drive or content read is cut, as the handshake refuses
    // each new connection, until one is read.
    let unread = Serving {
        scope: FleetScope::Facts,
        grants: Vec::new(),
    };
    let cut = match denied.as_deref() {
        Some(denied) => cut_unserved_with_denied(
            &serving,
            &|node, key| {
                let (keys, log) = keys_and_log();
                holds_in(keys, log.as_deref(), node, key)
            },
            denied,
        ),
        None => cut_unserved_with_denied(&unread, &|_, _| false, &[]),
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
        &|node| {
            let (keys, log) = keys_and_log();
            bound_or_admitted_in(keys, log.as_deref(), admitted_driver_key(node), node)
        },
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

fn arm_generation(generation: u64) -> Option<Arc<Stop>> {
    let mut desk = FLEET_DESK.lock_or_recover();
    if desk.generation != generation {
        return None;
    }
    let stop = Arc::new(Stop::new());
    desk.stop = Some(Arc::clone(&stop));
    Some(stop)
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
#[path = "fleet_listen_tests.rs"]
mod tests;

#[cfg(unix)]
#[path = "node_serve.rs"]
pub(crate) mod node_serve;
