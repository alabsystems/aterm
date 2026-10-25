// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FLEET LISTENER (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5).

#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use aterm_net::fleet::{FleetEvent, FleetOp, FleetScope};
use aterm_types::MutexExt as _;

use crate::app_config::FleetConfig;

/// How often a bind refused `EADDRINUSE` or `EADDRNOTAVAIL`, or a key that cannot be used, is
/// tried again, and how often a served key is read again.
pub(crate) const RETRY: Duration = aterm_net::fleet::KEY_READ_EVERY;

const RETRY_SEAM: &str = "ATERM_DEBUG_FLEET_RETRY_MS";

fn retry() -> Duration {
    static EVERY: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *EVERY.get_or_init(|| {
        aterm_types::dev_seam!(RETRY_SEAM)
            .and_then(|v| v.to_str()?.trim().parse::<u64>().ok())
            .filter(|&ms| ms > 0)
            .map_or(RETRY, Duration::from_millis)
    })
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

const SERVES: &[FleetOp] = &[FleetOp::Survey, FleetOp::Book, FleetOp::Join];

const SERVES_DRIVE: &[FleetOp] = &[
    FleetOp::Survey,
    FleetOp::Book,
    FleetOp::Join,
    FleetOp::Drive,
];

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

/// The drive grants `[fleet]` names in `text`, none unless it listens at the drive level:
/// `None` when `text` is no TOML or its `[fleet]` does not read.
fn granted_in(text: &str) -> Option<Vec<aterm_net::fleet::DriveGrant>> {
    #[derive(serde::Deserialize)]
    struct Fleet {
        fleet: Option<FleetConfig>,
    }
    let config = aterm_toml::from_str::<Fleet>(text).ok()?;
    Some(grants_of(fleet_wanted(config.fleet.as_ref()).as_ref()))
}

/// The grants `wanted` serves: its `drive_from` at the drive level, none at any other.
fn grants_of(wanted: Option<&Wanted>) -> Vec<aterm_net::fleet::DriveGrant> {
    wanted
        .filter(|wanted| level_of(wanted.allow.as_deref()).is_some_and(|(_, drive)| drive))
        .map(|wanted| wanted.drive_from.clone())
        .unwrap_or_default()
}

/// The grants of `serving` that aterm.toml as read `now` still names: all of them while it is
/// unread or its `[fleet]` does not read, as the running settings stand then.
fn granted_now(
    serving: &[aterm_net::fleet::DriveGrant],
    now: Option<&str>,
) -> Vec<aterm_net::fleet::DriveGrant> {
    let Some(named) = now.and_then(granted_in) else {
        return serving.to_vec();
    };
    serving
        .iter()
        .filter(|grant| named.contains(grant))
        .cloned()
        .collect()
}

/// aterm.toml's text as it is on disk now, a missing one's empty: `None` when it cannot be read.
#[cfg(unix)]
fn config_now() -> Option<String> {
    let path = crate::app_config::config_path()?;
    crate::native_config_service::VersionedConfigService::observe_path(&path, true)
        .ok()
        .map(|observation| observation.text)
}

/// Whether `node` may still drive this Mac under `grants`: one names it with the key `bound`
/// (this Mac's keyring) binds it to.
fn still_drives(
    grants: &[aterm_net::fleet::DriveGrant],
    bound: impl FnOnce() -> Option<aterm_net::fleet::NodeKey>,
    node: &str,
) -> bool {
    grants
        .iter()
        .find(|grant| grant.node == node)
        .is_some_and(|grant| bound() == Some(grant.key))
}

/// Lift every remote hold of a node `grants` lets drive no more ([`still_drives`]), as its own
/// `hold off` would: it can no longer lift it.
#[cfg(unix)]
fn lift_undriving_holds(grants: &[aterm_net::fleet::DriveGrant]) {
    let keys = std::cell::OnceCell::new();
    let keyring = || {
        keys.get_or_init(|| {
            aterm_types::dirs::state_dir()
                .map(|state| aterm_net::fleet_member::Keyring::read(&state))
                .unwrap_or_default()
        })
    };
    let lifted = crate::control::lift_ungranted_remote_holds(&|node| {
        still_drives(grants, || keyring().bound(node), node)
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

/// What this Mac finds of `hold`'s node now, for a local `hold off`: its grant here gone, or its
/// place in the book disowning it under the key the grant binds within [`HOLDER_WITHIN`]
/// ([`aterm_net::fleet_member::Unreached::disowns`]), ends it; a place that does not answer never.
#[cfg(unix)]
fn remote_holder(hold: &crate::fabric::RemoteHold) -> crate::fabric::Holder {
    use crate::fabric::Holder;
    use aterm_net::fleet_member::{Keyring, Target, peer_of, proves_itself, target_of};
    let node = hold.node.as_str();
    let gone = |why: &str| Holder::Gone(why.to_owned());
    let Some(state) = aterm_types::dirs::state_dir() else {
        return gone("this Mac has no state directory");
    };
    let serving = grants_of(FLEET_DESK.lock_or_recover().wanted.as_ref());
    let keys = Keyring::read(&state);
    let Some(grant) = granted_now(&serving, config_now().as_deref())
        .into_iter()
        .find(|grant| grant.node == node && keys.bound(node) == Some(grant.key))
    else {
        return gone("it may drive this Mac no more");
    };
    let Ok(secret) = read_fleet_key() else {
        return gone("this Mac holds no fleet key it can use, so no node drives it");
    };
    let Some(me) = read_node_key() else {
        return gone("this Mac proves no node, so no node drives it");
    };
    let Some(peer) = peer_of(&state, node) else {
        return gone("no book here records where it is");
    };
    let target = Target {
        node: Some(node.to_owned()),
        key: Some(grant.key),
        ..target_of(&peer)
    };
    match proves_itself(&target, &keys, (&secret, &me), HOLDER_WITHIN) {
        Ok(_) => Holder::Drives,
        Err(why) if why.disowns() => Holder::Gone(why.to_string()),
        Err(why) => Holder::Again(why.to_string()),
    }
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

/// The grants a listener serving `serving` moves to once aterm.toml reads `now`: those it
/// still names, when it names fewer ([`granted_now`]).
fn narrowed(
    serving: &[aterm_net::fleet::DriveGrant],
    now: Option<&str>,
) -> Option<Vec<aterm_net::fleet::DriveGrant>> {
    let named = granted_now(serving, now);
    (named.len() < serving.len()).then_some(named)
}

struct FleetDesk {
    wanted: Option<Wanted>,
    /// Moved by every change of `wanted`.
    generation: u64,
    /// A reload has set `wanted`, so [`start_fleet_listener`]'s launch-time table is older.
    configured: bool,
    started: bool,
    standing: Standing,
    /// The line that said where it stands ([`noted_move`]).
    said: String,
    /// The serving generation's flag: cleared to stop it.
    stop: Option<Arc<AtomicBool>>,
}

static FLEET_DESK: Mutex<FleetDesk> = Mutex::new(FleetDesk {
    wanted: None,
    generation: 0,
    configured: false,
    started: false,
    standing: Standing::Off,
    said: String::new(),
    stop: None,
});

static FLEET_CHANGED: Condvar = Condvar::new();

/// Start the listener's supervisor.
pub(crate) fn start_fleet_listener(fleet: Option<&FleetConfig>) {
    {
        let mut desk = FLEET_DESK.lock_or_recover();
        if desk.started {
            return;
        }
        desk.started = true;
        if !desk.configured {
            desk.wanted = fleet_wanted(fleet);
            desk.generation = desk.generation.wrapping_add(1);
        }
    }
    let spawned = std::thread::Builder::new()
        .name("aterm-fleet-listener".to_owned())
        .spawn(supervise_fleet_listener);
    if let Err(error) = spawned {
        FLEET_DESK.lock_or_recover().standing = Standing::Refused("thread");
        note_fleet(
            &format!("fleet listener could not start its thread: {error}"),
            true,
        );
    }
}

/// Follow a reloaded config's `[fleet]` table.
pub(crate) fn configure_fleet_listener(fleet: Option<&FleetConfig>) {
    let wanted = fleet_wanted(fleet);
    let mut desk = FLEET_DESK.lock_or_recover();
    desk.configured = true;
    if desk.wanted == wanted {
        return;
    }
    move_fleet(desk, wanted);
}

/// Serve `drive_from` alone while `generation` is current: aterm.toml names no more of its
/// grants, whether or not the reload that read it was taken. Whether the listener moved.
#[cfg(unix)]
fn narrow_fleet_listener(generation: u64, drive_from: Vec<aterm_net::fleet::DriveGrant>) -> bool {
    let desk = FLEET_DESK.lock_or_recover();
    let Some(wanted) = desk
        .wanted
        .clone()
        .filter(|_| desk.generation == generation)
    else {
        return false;
    };
    move_fleet(
        desk,
        Some(Wanted {
            drive_from,
            ..wanted
        }),
    );
    true
}

/// Move the listener to a new generation serving `wanted`, the current one's connections cut.
fn move_fleet(mut desk: std::sync::MutexGuard<'_, FleetDesk>, wanted: Option<Wanted>) {
    desk.wanted = wanted;
    desk.generation = desk.generation.wrapping_add(1);
    if let Some(stop) = desk.stop.take() {
        stop.store(false, Ordering::Release);
    }
    drop(desk);
    FLEET_CHANGED.notify_all();
}

/// `status`'s `fleet=` word: `off`, `listening`, `retrying` or `refused:<why>`.
pub(crate) fn fleet_status_word() -> String {
    FLEET_DESK.lock_or_recover().standing.word()
}

fn supervise_fleet_listener() {
    crate::qos::set_self(crate::qos::Role::Background);
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
    #[cfg(unix)]
    lift_undriving_holds(&grants_of(wanted.as_ref()));
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
    let Some((scope, drive)) = level_of(wanted.allow.as_deref()) else {
        stand_fleet(
            generation,
            Standing::Refused("allow"),
            "fleet listener not started: [fleet] allow is none of \"facts\", \"content\" and \
             \"drive\"",
        );
        return;
    };
    let drivers = if drive { wanted.drive_from } else { Vec::new() };
    #[cfg(unix)]
    serve_fleet_port(generation, addr, scope, drivers);
    #[cfg(not(unix))]
    {
        let _ = (addr, scope, drivers);
        stand_fleet(
            generation,
            Standing::Refused("unsupported"),
            "fleet listener not started: this platform serves no fleet connection",
        );
    }
}

/// Bind `addr` and serve it at `scope` until `generation` is no longer current.
#[cfg(unix)]
fn serve_fleet_port(
    generation: u64,
    addr: SocketAddr,
    scope: FleetScope,
    drivers: Vec<aterm_net::fleet::DriveGrant>,
) {
    let drivers = Arc::new(drivers);
    loop {
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
        let listener = match TcpListener::bind(addr) {
            Ok(listener) => {
                let _ = aterm_uds::keep_out_of_children(&listener);
                listener
            }
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
                let (running, rekeyed, secret, drivers) = (
                    Arc::clone(&running),
                    Arc::clone(&rekeyed),
                    secret.clone(),
                    Arc::clone(&drivers),
                );
                let proved = node
                    .as_ref()
                    .map(|node| (node.node().to_owned(), node.key()));
                move || {
                    watch_fleet_generation(
                        generation,
                        (&running, &rekeyed),
                        (&secret, proved),
                        &drivers,
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
        stand_fleet(
            generation,
            Standing::Listening,
            &if drivers.is_empty() {
                format!("fleet listening on {addr} (serving {})", scope.word())
            } else {
                format!(
                    "fleet listening on {addr} (serving {}; {} node(s) may drive it)",
                    scope.word(),
                    drivers.len()
                )
            },
        );
        let recheck = Arc::new((secret.clone(), node.clone()));
        let listening = aterm_net::fleet::Listening {
            secret,
            scope,
            node,
            serves: if drivers.is_empty() {
                SERVES
            } else {
                SERVES_DRIVE
            },
            slots: fleet_slots(),
            per_source: aterm_net::fleet::per_source(fleet_slots()),
            lifetime: aterm_net::fleet::SERVED_WITHIN,
        };
        let content = scope == FleetScope::Content;
        let port = addr.port();
        let drivers = Arc::clone(&drivers);
        aterm_net::fleet::serve(
            &listener,
            &listening,
            &running,
            move |served, op| {
                let peer = served.peer_addr();
                if op == FleetOp::Join {
                    let proof = served.peer_node().cloned();
                    return served.answer(|stream| {
                        stream.set_read_timeout(Some(aterm_net::fleet::HANDSHAKE_WITHIN))?;
                        register_member(stream, (peer, proof.as_ref()), (port, &recheck.0))
                    });
                }
                if op == FleetOp::Book {
                    return served.answer(|stream| {
                        stream.set_write_timeout(Some(aterm_net::fleet::HANDSHAKE_WITHIN))?;
                        share_book(stream, peer)
                    });
                }
                if op == FleetOp::Drive {
                    let (secret, me) = &*recheck;
                    let driver = match admit_driver(&served, &drivers, (secret, me.as_ref())) {
                        Ok(driver) => driver,
                        Err(why) => return refuse_driver(served, peer, &why),
                    };
                    let (near, far) = aterm_uds::CtlStream::pair()?;
                    if !crate::control::attach_fleet_drive(near, peer, driver) {
                        return Err(std::io::Error::other(
                            "this aterm's control server is not serving",
                        ));
                    }
                    return served.relay_answers(far);
                }
                let drive = !drivers.is_empty() && lists_driver(&served, &drivers);
                let (near, far) = aterm_uds::CtlStream::pair()?;
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

/// THE DRIVE GRANT (the step-in, `docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5.1).
/// # Errors
/// Why the dialer may not drive: it proved no node bound here, a node no grant names, one whose
/// grant holds another key, or one an old copy alone binds while the issuer's book cannot be read.
#[cfg(unix)]
fn admit_driver(
    served: &aterm_net::fleet::Served,
    drivers: &[aterm_net::fleet::DriveGrant],
    recheck: (
        &aterm_net::fleet::FleetSecret,
        Option<&aterm_net::fleet::NodeIdentity>,
    ),
) -> Result<crate::fabric::Driver, String> {
    use aterm_net::fleet_member::{peer_of, proven_driver};
    let state = aterm_types::dirs::state_dir().ok_or("this Mac has no state directory")?;
    let drivers = granted_now(drivers, config_now().as_deref());
    let node = proven_driver(served, &state, &drivers, recheck).map_err(|why| why.to_string())?;
    let host = driver_name(
        &node,
        peer_of(&state, &node).map(|peer| peer.host).as_deref(),
    );
    Ok(crate::fabric::Driver { node, host })
}

/// Whether this Mac lets the peer of `served` drive it, by the grants a drive is admitted by.
#[cfg(unix)]
fn lists_driver(
    served: &aterm_net::fleet::Served,
    drivers: &[aterm_net::fleet::DriveGrant],
) -> bool {
    let drivers = granted_now(drivers, config_now().as_deref());
    aterm_types::dirs::state_dir().is_some_and(|state| {
        aterm_net::fleet_member::listed_driver(served, &state, &drivers).is_ok()
    })
}

/// The proven `node` of a drive as its credit, its hold and its records name it.
#[cfg(unix)]
fn driver_name(node: &str, host: Option<&str>) -> String {
    let short = node.get(..8).unwrap_or(node);
    match host.filter(|host| aterm_net::fleet_member::is_host(host)) {
        Some(host) => format!("{host} (node {short})"),
        None => format!("node {short}"),
    }
}

/// A drive this Mac refuses ([`admit_driver`]).
#[cfg(unix)]
fn refuse_driver(
    served: aterm_net::fleet::Served,
    peer: Option<SocketAddr>,
    why: &str,
) -> std::io::Result<()> {
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
    served.answer(|stream| {
        stream.set_write_timeout(Some(aterm_net::fleet::HANDSHAKE_WITHIN))?;
        std::io::Write::write_all(stream, format!("ERR denied: {why}\n").as_bytes())
    })
}

/// A member's `join` (`aterm fleet join` on another machine), proved under `secret`.
#[cfg(unix)]
fn register_member<S: std::io::Read + std::io::Write>(
    stream: &mut S,
    (peer, proof): (Option<SocketAddr>, Option<&aterm_net::fleet::NodeProof>),
    (port, secret): (u16, &aterm_net::fleet::FleetSecret),
) -> std::io::Result<()> {
    use aterm_net::fleet_member::{Issuer, is_host, read_join_code, read_node_id, serve_join};
    let (Some(peer), Some(state)) = (peer, aterm_types::dirs::state_dir()) else {
        return Err(std::io::Error::other(
            "a join with no peer address or no state directory",
        ));
    };
    let issuer = || {
        let node = read_node_id(&state)
            .map_err(|e| e.to_string())?
            .ok_or_else(no_node_id)?;
        let host = read_join_code(&state)
            .map(|code| code.host)
            .or_else(|why| match aterm_uds::process::lan_host_name() {
                Some(name) if is_host(&name) => Ok(name),
                _ => Err(format!(
                    "its join code cannot be read ({why}), and its LAN name is no host"
                )),
            })?;
        Ok(Issuer {
            node,
            host,
            port,
            version: aterm_types::version::app_version().to_owned(),
        })
    };
    let now_ms = aterm_time::unix_millis().unwrap_or(0);
    match serve_join(stream, (peer.ip(), proof), (&state, secret), issuer, now_ms)? {
        Ok((recorded, member)) => aterm_log::info!("{}", registration_note(recorded, &member)),
        Err(why) => aterm_log::warn!(
            "fleet: the join from {peer} was refused: {}",
            aterm_log::sanitize_record(&why)
        ),
    }
    Ok(())
}

/// Why this machine, an issuer, cannot name itself: its node file is gone.
#[cfg(unix)]
fn no_node_id() -> String {
    format!(
        "it has no node id (the {} file in its state directory is gone): restore that file, \
         or run `aterm fleet init --rotate` on the issuer, after which every member joins again",
        aterm_net::fleet_member::NODE_FILE
    )
}

/// A member's read of this machine's book of peers.
#[cfg(unix)]
fn share_book<S: std::io::Write>(stream: &mut S, peer: Option<SocketAddr>) -> std::io::Result<()> {
    use aterm_net::fleet_member::{read_node_id, serve_book};
    let Some(state) = aterm_types::dirs::state_dir() else {
        return Err(std::io::Error::other("a book read with no state directory"));
    };
    let node = || {
        read_node_id(&state)
            .map_err(|e| e.to_string())?
            .ok_or_else(no_node_id)
    };
    let peer = peer.map_or_else(|| "a peer".to_owned(), |peer| peer.to_string());
    match serve_book(stream, &state, node)? {
        Ok(shared) => aterm_log::debug!("fleet: {peer} read this machine's book ({shared} peers)"),
        Err(why) => aterm_log::info!(
            "fleet: {peer}'s read of the book was refused: {}",
            aterm_log::sanitize_record(&why)
        ),
    }
    Ok(())
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

/// While `generation` serves `secret`, proves the node `proved` (its id and key) and lets
/// `drivers` drive, read them again every [`RETRY`]: a grant aterm.toml names no more moves the
/// listener to those it names ([`narrow_fleet_listener`]), and a node that may drive no more
/// keeps no hold ([`lift_undriving_holds`]).
#[cfg(unix)]
fn watch_fleet_generation(
    generation: u64,
    (running, rekeyed): (&AtomicBool, &AtomicBool),
    (secret, proved): (
        &aterm_net::fleet::FleetSecret,
        Option<(String, aterm_net::fleet::NodeKey)>,
    ),
    drivers: &[aterm_net::fleet::DriveGrant],
) {
    use aterm_net::fleet::KEY_FILE;
    use aterm_net::fleet_member::{NODE_FILE, NODE_KEY_FILE, read_node_identity};
    let node_now = || {
        aterm_types::dirs::state_dir()
            .and_then(|dir| read_node_identity(&dir).ok().flatten())
            .map(|node| (node.node().to_owned(), node.key()))
    };
    while wait_fleet_unchanged(generation) && running.load(Ordering::Acquire) {
        let why = match read_fleet_key() {
            Ok(now) if now == *secret && node_now() == proved => {
                if follow_grants(generation, drivers) {
                    return;
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

/// Follow aterm.toml's grants while `generation` serves `drivers`: move the listener to those it
/// still names ([`narrowed`]), or lift each hold of a node that may drive no more. Whether it
/// moved.
#[cfg(unix)]
fn follow_grants(generation: u64, drivers: &[aterm_net::fleet::DriveGrant]) -> bool {
    if drivers.is_empty() {
        return false;
    }
    if let Some(named) = narrowed(drivers, config_now().as_deref())
        && narrow_fleet_listener(generation, named)
    {
        note_fleet(
            "fleet listener: aterm.toml lets fewer nodes drive this Mac; every fleet connection \
             is cut",
            false,
        );
        return true;
    }
    lift_undriving_holds(drivers);
    false
}

fn wait_fleet_unchanged(generation: u64) -> bool {
    let desk = FLEET_DESK.lock_or_recover();
    let (desk, _) = FLEET_CHANGED
        .wait_timeout_while(desk, retry(), |desk| desk.generation == generation)
        .unwrap_or_else(PoisonError::into_inner);
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

    /// A DRIVE IS NAMED BY ITS PROVEN NODE.
    #[cfg(unix)]
    #[test]
    fn a_drive_is_named_by_the_node_it_proved() {
        let (n, m) = ("1a2b3c4d".repeat(4), "9f8e7d6c".repeat(4));
        assert_eq!(
            driver_name(&n, Some("mac-11.local")),
            "mac-11.local (node 1a2b3c4d)"
        );
        assert_ne!(
            driver_name(&n, Some("mac-11.local")),
            driver_name(&m, Some("mac-11.local")),
            "a node that names itself as another Mac reads as itself"
        );
        assert_eq!(
            driver_name(&n, Some("mac-11.local")),
            driver_name(&n, Some("mac-11.local"))
        );
        for unshown in [None, Some("the message bus"), Some("")] {
            assert_eq!(driver_name(&n, unshown), "node 1a2b3c4d", "{unshown:?}");
        }
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
            configured: false,
            started: false,
            standing: Standing::Off,
            said: String::new(),
            stop: None,
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
    fn a_grant_taken_back_admits_no_drive_and_narrows_the_listener() {
        let grant = |n: u8| {
            aterm_net::fleet::DriveGrant::from_word(&format!(
                "{}:{}",
                format!("{n:02x}").repeat(16),
                "cd".repeat(32)
            ))
            .expect("a grant")
        };
        let (a, b) = (grant(0x0a), grant(0x0b));
        let serving = vec![a.clone(), b.clone()];
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
        let both = toml("127.0.0.1:7100", "drive", &[&a, &b]);
        assert_eq!(granted_now(&serving, Some(&both)), serving);
        assert_eq!(
            granted_now(&serving, Some(&toml("127.0.0.1:7100", "DRIVE", &[&b]))),
            vec![b.clone()],
            "peers rm of a: only b drives"
        );
        let unrelated = format!(
            "font_px = \"not a number\"\n{}",
            toml("127.0.0.1:7100", "content", &[])
        );
        for gone in [
            toml("127.0.0.1:7100", "content", &[]),
            toml("127.0.0.1:7100", "facts", &[&a, &b]),
            toml("", "drive", &[&a, &b]),
            "theme = \"Nord\"\n".to_owned(),
            String::new(),
            unrelated,
        ] {
            assert!(granted_now(&serving, Some(&gone)).is_empty(), "{gone}");
        }
        for broken in [
            "[fleet\n".to_owned(),
            "theme = Nord\n".to_owned(),
            "[fleet]\nallow = 5\n".to_owned(),
        ] {
            assert_eq!(
                granted_now(&serving, Some(&broken)),
                serving,
                "a [fleet] that does not read leaves the running grants: {broken}"
            );
        }
        assert_eq!(
            granted_now(&serving, None),
            serving,
            "so does an aterm.toml that cannot be read"
        );
        assert!(
            granted_now(
                std::slice::from_ref(&b),
                Some(&toml("127.0.0.1:7100", "drive", &[&a]))
            )
            .is_empty(),
            "a grant added waits for the listener to serve it"
        );

        assert_eq!(narrowed(&serving, Some(&both)), None);
        assert_eq!(narrowed(&serving, Some("[fleet\n")), None);
        assert_eq!(narrowed(&serving, None), None);
        assert_eq!(
            narrowed(&serving, Some(&toml("127.0.0.1:7100", "drive", &[&b]))),
            Some(vec![b.clone()])
        );
        assert_eq!(
            narrowed(&serving, Some(&toml("127.0.0.1:7100", "facts", &[&a, &b]))),
            Some(Vec::new())
        );
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
        assert!(still_drives(&grants, || Some(key), &a.node));
        assert!(
            !still_drives(&grants, || Some(other), &a.node),
            "its issuer's book binds it to another key"
        );
        assert!(
            !still_drives(&grants, || None, &a.node),
            "its issuer's book names it no more"
        );
        assert!(
            !still_drives(
                &grants,
                || unreachable!("no grant names it"),
                &"0c".repeat(16)
            ),
            "no grant names it"
        );
        assert!(!still_drives(&[], || Some(key), &a.node));
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
}
