// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The keyless HTTP vault server ([`serve`]): it holds each vault's latest signed envelope,
//! takes a PUT only when its signer and handoff chain check out, holds long-polls, and queues
//! join requests, under per-server and per-address caps.

use crate::envelope::{self, MAX_ENVELOPE};
use crate::transport::{JoinRequest, parse_join_line};
use crate::wake::{Bell, Wake};
use crate::{Error, fsx};
use aterm_uds::power::Power;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{self, Read, Write};
use std::net::{
    IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How to serve.
#[derive(Debug, Clone)]
pub struct Options {
    /// Where to listen.
    pub listen: SocketAddr,
    /// The store directory (made 0700).
    pub store: PathBuf,
    /// Optional `<name>.local` mDNS registration on the LAN's IPv4 address (macOS only).
    pub advertise: Option<String>,
    /// Pins for names the store holds nothing for: name → signing key.
    pub pins: Vec<(String, [u8; 32])>,
    /// How long a long-poll is held.
    pub hold: Duration,
    /// macOS: hold `caffeinate -i -w <pid>` so the Mac does not idle-sleep under the server.
    pub keep_awake: bool,
}

/// Default hold duration for a long-poll: a held request that a sleeping Mac lost is asked
/// again within about this long of its waking ([`crate::client`]'s long-poll waits as long).
pub const DEFAULT_HOLD: Duration = Duration::from_secs(25);
/// The most connections served at once.
pub const MAX_CONNECTIONS: usize = 1024;
/// The most connections served at once from one address ([`MAX_CONNECTIONS`]).
pub const MAX_PEER_CONNECTIONS: usize = 16;
/// The most PUT bodies read at once (each up to [`MAX_ENVELOPE`]).
pub const MAX_PUTS: usize = 4;
/// The most PUT bodies read at once from one address of another machine.
pub const MAX_PEER_PUTS: usize = 1;
/// A connection thread's stack.
const CONNECTION_STACK: usize = 256 * 1024;
/// The largest request head, its blank line included.
pub const MAX_HEAD: usize = 16 * 1024;
/// How long a client has to send its whole head.
pub const HEAD_DEADLINE: Duration = Duration::from_secs(10);
/// How long a client has to send a body, and the longest it may stall within it.
const BODY_DEADLINE: Duration = Duration::from_secs(120);
const BODY_STALL: Duration = Duration::from_secs(10);
/// The longest one write of an answer may block, and the longest the whole answer may take.
const WRITE_STALL: Duration = Duration::from_secs(60);
const ANSWER_DEADLINE: Duration = Duration::from_secs(300);
/// The most an answer is handed to the socket in one write.
const WRITE_CHUNK: usize = 64 * 1024;
/// How long the accept loop gives a 503 over a cap (a fresh socket takes one line at once).
const REFUSAL_DEADLINE: Duration = Duration::from_secs(1);
/// How many answered refusals may wait to be closed gently.
const REFUSALS_QUEUED: usize = 64;
/// How often a held long-poll looks whether its client went away.
const LIVENESS_EVERY: Duration = Duration::from_secs(5);
/// The largest join request body.
pub const MAX_JOIN_BODY: usize = 1024;
/// The most queued join requests per vault.
pub const MAX_JOINS: usize = 256;
/// The most queued join requests from one address per vault.
pub const MAX_PEER_JOINS: usize = 4;
/// The largest `.joins` file read ([`MAX_JOINS`]).
const MAX_JOINS_FILE: usize = MAX_JOINS * (MAX_JOIN_BODY + 128);
/// How long a join request stays queued.
pub const JOIN_TTL_SECS: u64 = 7 * 24 * 60 * 60;
/// How long a queued join request stands before its key may change it.
pub const JOIN_CHANGE_GAP: Duration = Duration::from_secs(10);
/// How often the log notes changes to one queued join request.
const JOIN_NOTE_EVERY_SECS: u64 = 60 * 60;
/// How long before an unchanged join re-post refreshes its queue time ([`JOIN_TTL_SECS`]).
pub(crate) const JOIN_REFRESH_EVERY_SECS: u64 = 60 * 60;
/// The most subscriber last-heard records kept per vault (`<name>.heard`).
pub const MAX_HEARD: usize = 512;
/// The largest `.heard` file read ([`MAX_HEARD`]).
const MAX_HEARD_FILE: usize = MAX_HEARD * 128;
/// How long a subscriber's last-heard record is kept without a newer poll (90 days).
pub const HEARD_TTL_SECS: u64 = 90 * 24 * 60 * 60;
/// Minimum interval between disk flushes of `<name>.heard` while running.
const HEARD_FLUSH_SECS: u64 = 60;
/// The most vaults one server holds.
pub const MAX_VAULTS: usize = 64;
/// The mDNS service type advertised.
pub const SERVICE_TYPE: &str = "_alab-vault._tcp";
/// macOS's mDNS command.
pub const DNS_SD: &str = aterm_uds::os_path!("dns-sd");
/// macOS's sleep assertion command.
pub const CAFFEINATE: &str = aterm_uds::os_path!("caffeinate");
/// How often the advertised address is checked.
const ADVERTISE_EVERY: Duration = Duration::from_secs(30);
/// Continuous `/v1/alab` probe outage on `<holder>.local` before a standby `always_on` steward
/// advertises `alab-secrets` as a fallback (`docs/DESIGN-alab-swarm-2026-10-27.md` §8.3).
pub const ALAB_FALLBACK_AFTER: Duration = Duration::from_secs(90);
/// What a closing connection drains so its answer is not lost to a reset.
const LINGER_BYTES: usize = 1 << 20;
const LINGER_DEADLINE: Duration = Duration::from_secs(2);
/// How often [`serve`] looks whether its program was replaced on disk.
const PROGRAM_EVERY: Duration = Duration::from_secs(5);
/// How long a server stopping for a new program waits for the answers in flight.
const DRAIN_DEADLINE: Duration = Duration::from_secs(10);

/// Canonical `(m_number_or_max, label)` sort key for F7's lower-label tie-break (`m3` < `m19` <
/// non-`m<N>` labels).
pub(crate) fn label_rank(raw: &str) -> Option<(u32, String)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case(crate::alab_advertised()) {
        return None;
    }
    if let Some((n, canonical)) = crate::swarm_roster::leading_m_label(trimmed) {
        return Some((n, canonical));
    }
    let lower = trimmed.to_ascii_lowercase();
    crate::text::valid_machine_name(&lower).then_some((u32::MAX, lower))
}

/// Input snapshot for one tick of [`AlabNameWatcher::decide`] (`DESIGN-fluid-swarm` §4.1 F7).
#[derive(Debug, Clone)]
pub struct AlabNameInput<'a> {
    /// This server's own canonical `m<N>` (or host) label.
    pub own_label: &'a str,
    /// Sequence of the verified ALab vault envelope currently held in this server's store (`None`
    /// while empty).
    pub own_seq: Option<u64>,
    /// All `alab-secrets` / `alab-secrets (<N>)` `_alab-vault._tcp` registrations currently
    /// observed on the LAN.
    pub holders: &'a [crate::discover::DiscoveredVault],
    /// Monotone wall-clock instant for fallback duration measurement.
    pub now: Instant,
}

/// Lease-free controller deciding whether this server should advertise `alab-secrets.local`
/// alongside its own `<label>.local` (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1 F7).
///
/// - Every server probes `alab-secrets.local` itself and takes the name only after
///   [`ALAB_FALLBACK_AFTER`] (90 s) in which nothing answered there with a verified ALab envelope,
///   so a returning server leaves the name to any holder that answers.
/// - When two `>= 0.146` holders collide after a partition heal, every server settles them by one
///   strict order read from `_alab-vault._tcp` TXT (`seq` descending, then `label` ascending); the
///   loser withdraws and serves under its own label.
/// - mDNS lets only one of two holders keep the name, and hides the other's instance from the
///   order above: a holder whose registration ends that way lets the name go
///   ([`Self::lost_name`]).
/// - Because a `0.145` server (`--advertise alab-secrets`, no `label=` in TXT) has no watcher and
///   never withdraws, a `>= 0.146` server yields whenever a `0.145` holder answers with a verified
///   ALab envelope at least as new as its own.
#[derive(Debug, Default)]
pub struct AlabNameWatcher {
    holding: bool,
    failing_since: Option<Instant>,
}

impl AlabNameWatcher {
    /// Create a fresh watcher.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether this server currently holds `alab-secrets`.
    #[must_use]
    pub const fn is_holding(&self) -> bool {
        self.holding
    }

    /// This server's `alab-secrets` registration ended by itself while it held the name. mDNS
    /// keeps one host per name, and `dns-sd -P` gives a name up to a device that already answers
    /// to it. That device's registration also hides this server's `_alab-vault._tcp` instance, so
    /// neither holder hears the other and the (`seq`, `label`) order in [`Self::decide`] never
    /// meets them: the name stays with the one mDNS kept. This lets it go and listens again. The
    /// next [`Self::decide`] probes `alab-secrets.local`, stays off while a holder answers there,
    /// and takes the name again only after [`ALAB_FALLBACK_AFTER`] in which none does. The heads
    /// still meet: the holder pulls a newer one from this server's own-label instance (F6).
    pub fn lost_name(&mut self) {
        self.holding = false;
        self.failing_since = None;
    }

    /// This Mac went to sleep, and this server ended its registrations before it let the Mac
    /// sleep, so mDNSResponder hands a Bonjour Sleep Proxy no `alab-secrets` record to answer
    /// for while nothing serves it. This lets the name go, as [`Self::lost_name`] does, and
    /// forgets how long nothing answered there (a sleep stops that clock): on wake the next
    /// [`Self::decide`] competes for the name by the same probe and [`ALAB_FALLBACK_AFTER`] as
    /// any server that does not hold it.
    pub fn slept(&mut self) {
        self.holding = false;
        self.failing_since = None;
    }

    /// Decide whether this server should advertise `alab-secrets` on this tick.
    ///
    /// `probe_alab` probes a host label (`"alab-secrets"`) or resolved holder URL and returns the
    /// verified ALab envelope's `seq` when it answers with a valid ALab envelope.
    pub fn decide(
        &mut self,
        input: &AlabNameInput<'_>,
        mut probe_alab: impl FnMut(&str) -> Option<u64>,
    ) -> bool {
        let Some(own_seq) = input.own_seq.filter(|&seq| seq >= 1) else {
            self.holding = false;
            self.failing_since = None;
            return false;
        };
        let Some(own_rank) = label_rank(input.own_label) else {
            self.holding = false;
            self.failing_since = None;
            return false;
        };

        let peer_outranks = |holders: &[crate::discover::DiscoveredVault],
                             probe: &mut dyn FnMut(&str) -> Option<u64>|
         -> bool {
            for holder in holders {
                match &holder.txt {
                    Some(txt) if txt.vault == crate::ALAB_VAULT => {
                        if let Some(peer_label) = txt.label.as_deref()
                            && let Some(peer_rank) = label_rank(peer_label)
                        {
                            if peer_rank.1 == own_rank.1 {
                                continue;
                            }
                            if txt.seq > own_seq || (txt.seq == own_seq && peer_rank < own_rank) {
                                return true;
                            }
                        } else if probe(&holder.url).is_some_and(|seq| seq >= own_seq) {
                            return true;
                        }
                    }
                    Some(_) => {}
                    None => {
                        // A 0.145 holder registers `path=/v1` alone (`txt == None`) and never
                        // withdraws; yield whenever it answers with `seq >= own_seq`.
                        if probe(&holder.url).is_some_and(|seq| seq >= own_seq) {
                            return true;
                        }
                    }
                }
            }
            false
        };

        if self.holding {
            if peer_outranks(input.holders, &mut probe_alab) {
                self.holding = false;
                self.failing_since = None;
                return false;
            }
            self.failing_since = None;
            return true;
        }

        let holder_answers = probe_alab(crate::alab_advertised()).is_some()
            || input.holders.iter().any(|holder| {
                !holder.url.contains("alab-secrets.local") && probe_alab(&holder.url).is_some()
            });
        if holder_answers {
            self.failing_since = None;
            return false;
        }
        let since = *self.failing_since.get_or_insert(input.now);
        if input.now.saturating_duration_since(since) >= ALAB_FALLBACK_AFTER {
            self.holding = true;
            self.failing_since = None;
            return true;
        }
        false
    }
}

/// Serve until the process ends, or until this program is replaced on disk (then `Ok`).
pub fn serve(options: Options) -> crate::Result<()> {
    serve_with_paths(options, None)
}

/// Per-tick callback deciding which host label `serve` advertises (and updating its pins when
/// policy or the store changes).
pub type RoleDecider = Box<dyn FnMut(&mut Vec<(String, [u8; 32])>) -> Option<String> + Send>;

/// [`serve`], also following `aterm-swarm-lease` from `client_paths` when advertising a steward's
/// own `<label>.local` on the ALab network (`DESIGN-alab-swarm` §8.3).
pub fn serve_with_paths(
    options: Options,
    client_paths: Option<crate::client::Paths>,
) -> crate::Result<()> {
    serve_with_role(options, client_paths, None)
}

/// [`serve_with_paths`], re-evaluating the advertised mDNS role on every advert tick when
/// `role_decide` is provided (`DESIGN-fluid-swarm` §4.1 F1).
pub fn serve_with_role(
    options: Options,
    client_paths: Option<crate::client::Paths>,
    role_decide: Option<RoleDecider>,
) -> crate::Result<()> {
    if let Some(name) = &options.advertise
        && !envelope::valid_name(name)
    {
        return Err(Error::new(
            "--advertise takes a host label: lowercase letters, digits and -",
        ));
    }
    let signalled = Arc::new(AtomicBool::new(false));
    {
        let signalled = Arc::clone(&signalled);
        let main = std::thread::current();
        aterm_uds::process::take_stop_signals("vault-stop", move |_| {
            signalled.store(true, Ordering::SeqCst);
            main.unpark();
        });
    }
    let listener = TcpListener::bind(options.listen)
        .map_err(|e| Error::new(format!("listening on {}: {e}", options.listen)))?;
    let running = start(listener, &options)?;
    note(&format!(
        "serving the vaults in {} on {}",
        options.store.display(),
        running.addr
    ));
    let unheld = unheld_note(&running.shared.lock_server_state(), &options.pins);
    if let Some(line) = unheld {
        note(&line);
    }
    let own_names: Vec<String> = options.advertise.iter().cloned().collect();
    let advertise = match (role_decide, options.advertise) {
        (Some(decide), _) => Some((
            decide,
            own_names,
            running.addr.port(),
            Arc::clone(&running.shared),
        )),
        (None, Some(name)) => {
            let decide: RoleDecider = Box::new(move |_| Some(name.clone()));
            Some((
                decide,
                own_names,
                running.addr.port(),
                Arc::clone(&running.shared),
            ))
        }
        (None, None) => None,
    };
    let children = Children::default();
    children.tend_with_role(options.keep_awake, advertise, client_paths);
    let program = std::env::current_exe().ok();
    let result = supervise(
        running,
        PROGRAM_EVERY,
        &|| program.as_deref().and_then(stamp_of),
        &signalled,
    );
    drop(children);
    result
}

/// An executable as it is on disk.
pub(crate) type Stamp = (u64, u64, u64, i64);

/// Whether the program a service process started from (stamped `was`) was replaced on disk
/// (`now`): another file at its path, which the service starts in its place once this process
/// exits. A path that is gone (an `aterm.app` moved or deleted) is no replacement: nothing new
/// waits there and launchd could not start the gone path, so the process keeps running from the
/// file it holds open, and a repair moves the job (`crate::service::repair_server`, the GUI's
/// subscriber mend).
pub(crate) fn replaced(was: Option<Stamp>, now: Option<Stamp>) -> bool {
    matches!((was, now), (Some(was), Some(now)) if was != now)
}

/// The file at `path` as it is on disk now. A service stamps the path its program started from,
/// read once as it starts (client.rs's subscriber does the same): on Linux `current_exe` reads
/// `/proc/self/exe`, which names `<path> (deleted)` once an update renames a new file over the
/// path, so a read then would take the update for a program gone.
pub(crate) fn stamp_of(path: &Path) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.dev(), meta.ino(), meta.size(), meta.mtime()))
}

/// Run until the accept loop ends ([`Running::stop`], [`DRAIN_DEADLINE`]).
fn supervise(
    running: Running,
    every: Duration,
    stamp: &dyn Fn() -> Option<Stamp>,
    signalled: &AtomicBool,
) -> crate::Result<()> {
    let program = stamp();
    loop {
        if running.accept.as_ref().is_none_or(JoinHandle::is_finished) {
            return running.wait();
        }
        let stopping = if signalled.load(Ordering::SeqCst) {
            Some("stopped by a signal")
        } else if replaced(program, stamp()) {
            Some("this program was replaced on disk; exiting so the service starts the new one")
        } else {
            None
        };
        if let Some(why) = stopping {
            note(why);
            let shared = Arc::clone(&running.shared);
            running.stop();
            let started = Instant::now();
            while shared.active.load(Ordering::SeqCst) > 0 && started.elapsed() < DRAIN_DEADLINE {
                std::thread::sleep(Duration::from_millis(20));
            }
            return Ok(());
        }
        std::thread::park_timeout(every);
    }
}

/// A server running on an already bound listener (tests bind `127.0.0.1:0`).
pub struct Running {
    /// Where it listens.
    pub addr: SocketAddr,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    shared: Arc<Shared>,
    accept: Option<JoinHandle<io::Result<()>>>,
}

impl Running {
    /// Stop accepting and return once the accept loop has ended.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.shared.stopping.store(true, Ordering::SeqCst);
        {
            let mut state = self.shared.lock_server_state();
            for slot in state.vaults.values() {
                slot.wake.notify_all();
            }
            flush_all_heard(&self.shared, &mut state);
        }
        let wake = wake_addr(self.addr);
        if TcpStream::connect_timeout(&wake, Duration::from_secs(5)).is_ok()
            && let Some(accept) = self.accept.take()
        {
            let _ = accept.join();
        }
    }

    /// Wait for the accept loop to end (it ends only on a listener error).
    fn wait(mut self) -> crate::Result<()> {
        match self.accept.take().map(JoinHandle::join) {
            Some(Ok(Ok(()))) | None => Ok(()),
            Some(Ok(Err(e))) => Err(Error::new(format!("accepting connections: {e}"))),
            Some(Err(_)) => Err(Error::new("the accept loop panicked")),
        }
    }

    /// Pull newer verified envelopes from `peers` (`?after=<held seq>`) and store those accepted
    /// by this server's admission rules (`DESIGN-fluid-swarm` §4.1 F6). Returns how many vaults
    /// advanced.
    pub fn pull_from_peers(&self, peers: &[crate::discover::DiscoveredVault]) -> usize {
        pull_from_peers_shared(&self.shared, peers, self.addr.port(), None, &[])
    }

    /// Probe `target` (a host label or URL) for a verified ALab vault envelope accepted under this
    /// server's ALab pin (`DESIGN-fluid-swarm` §4.1 F7).
    pub fn probe_alab(&self, target: &str) -> Option<u64> {
        probe_alab_endpoint(target, self.addr.port(), None, &[], false, &self.shared)
    }

    /// Sequence number currently held for `vault` (`None` while unheld).
    #[must_use]
    pub fn held_seq(&self, vault: &str) -> Option<u64> {
        let mut state = self.shared.lock_server_state();
        take_from_store(&self.shared, &mut state, vault);
        state.vaults.get(vault).map(|slot| slot.held.seq)
    }

    /// Verified membership records currently held for `vault` (empty while none are stored).
    #[must_use]
    pub fn stored_records(&self, vault: &str) -> crate::membership_log::Store {
        let mut state = self.shared.lock_server_state();
        take_records_from_store(&self.shared, &mut state, vault);
        state
            .records
            .get(vault)
            .map(|slot| slot.store.clone())
            .or_else(|| crate::membership_log::Store::for_vault(vault).ok())
            .unwrap_or_default()
    }

    /// How many long-polls it holds now.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.shared.waiting.load(Ordering::SeqCst)
    }

    /// How many GETs of a vault's envelope it has answered.
    #[cfg(test)]
    pub(crate) fn envelope_gets(&self) -> usize {
        self.shared.envelope_gets.load(Ordering::SeqCst)
    }

    /// How many GETs of a vault's join queue it has answered.
    #[cfg(test)]
    pub(crate) fn join_gets(&self) -> usize {
        self.shared.join_gets.load(Ordering::SeqCst)
    }
}

/// The address that reaches a listener bound to `addr` from this machine.
fn wake_addr(addr: SocketAddr) -> SocketAddr {
    match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => {
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), addr.port())
        }
        IpAddr::V6(ip) if ip.is_unspecified() => {
            SocketAddr::new(Ipv6Addr::LOCALHOST.into(), addr.port())
        }
        _ => addr,
    }
}

/// Start serving on `listener` in background threads (no mDNS, no caffeinate).
pub fn start(listener: std::net::TcpListener, options: &Options) -> crate::Result<Running> {
    launch(listener, options, Tuning::default())
}

/// [`start`] with the join queue's change gap `gap` in place of [`JOIN_CHANGE_GAP`].
#[cfg(any(test, debug_assertions))]
pub fn start_with_join_change_gap(
    listener: std::net::TcpListener,
    options: &Options,
    gap: Duration,
) -> crate::Result<Running> {
    launch(
        listener,
        options,
        Tuning {
            join_change_gap: gap,
            ..Tuning::default()
        },
    )
}

/// [`start_with_join_change_gap`] with no gap, its join queue kept by the clock `now`.
#[cfg(test)]
pub(crate) fn start_with_clock(
    listener: std::net::TcpListener,
    options: &Options,
    now: fn() -> u64,
) -> crate::Result<Running> {
    launch(
        listener,
        options,
        Tuning {
            join_change_gap: Duration::ZERO,
            now,
            ..Tuning::default()
        },
    )
}

/// Post the join line `line` to vault `name` of `running` as from `peer`, as one over the network
/// is posted: its reply's status.
#[cfg(test)]
pub(crate) fn post_join_as(running: &Running, name: &str, line: &str, peer: IpAddr) -> u16 {
    post_join(&running.shared, name, line.as_bytes(), peer).status
}

/// [`start`] as a server on another Mac than its clients' ([`refuse_unheld`]).
#[cfg(test)]
pub(crate) fn start_apart(
    listener: std::net::TcpListener,
    options: &Options,
) -> crate::Result<Running> {
    launch(
        listener,
        options,
        Tuning {
            is_local: |_, _| false,
            ..Tuning::default()
        },
    )
}

/// Server deadlines, clock, and local-connection predicate ([`this_machine`]).
#[derive(Clone, Copy)]
struct Tuning {
    head_deadline: Duration,
    body_deadline: Duration,
    body_stall: Duration,
    write_stall: Duration,
    answer_deadline: Duration,
    liveness_every: Duration,
    join_change_gap: Duration,
    /// How long a closing connection drains what its client still sends ([`linger`]).
    linger: Duration,
    /// How long a pull from a `>= 0.146` peer may take ([`PEER_PULL_BUDGET`]).
    peer_pull: Duration,
    /// The wall clock the join queue is kept by, in unix seconds ([`now`]).
    now: fn() -> u64,
    /// Whether a connection from a peer address, arriving on one of this machine's addresses
    /// (`None`: unknown), comes from this machine.
    is_local: fn(IpAddr, Option<IpAddr>) -> bool,
    /// The keys no PUT is taken signed by, and the key a held signer among them gives way to.
    retired: fn() -> envelope::Retired,
    /// Run by a PUT between its first judgment and the publish lock: the vault's store file
    /// and the PUT's seq.
    #[cfg(test)]
    before_lock: fn(&Path, u64),
}

impl Default for Tuning {
    fn default() -> Tuning {
        Tuning {
            head_deadline: HEAD_DEADLINE,
            body_deadline: BODY_DEADLINE,
            body_stall: BODY_STALL,
            write_stall: WRITE_STALL,
            answer_deadline: ANSWER_DEADLINE,
            liveness_every: LIVENESS_EVERY,
            join_change_gap: JOIN_CHANGE_GAP,
            linger: LINGER_DEADLINE,
            peer_pull: PEER_PULL_BUDGET,
            now,
            is_local: this_machine,
            retired: envelope::Retired::compiled,
            #[cfg(test)]
            before_lock: |_, _| {},
        }
    }
}

fn launch(listener: TcpListener, options: &Options, tuning: Tuning) -> crate::Result<Running> {
    for (name, _) in &options.pins {
        if !envelope::valid_name(name) {
            return Err(Error::new(
                "a pin names a vault that is not a vault name ([a-z0-9][a-z0-9-]{0,62})",
            ));
        }
    }
    fsx::ensure_private_dir(&options.store)?;
    let state = load(&options.store, (tuning.now)())?;
    let _ = aterm_uds::keep_out_of_children(&listener);
    let addr = listener.local_addr()?;
    let shared = Arc::new(Shared {
        store: options.store.clone(),
        pins: Mutex::new(options.pins.clone()),
        retired: (tuning.retired)(),
        hold: options.hold,
        tuning,
        server_state: Mutex::new(state),
        stopping: AtomicBool::new(false),
        active: AtomicUsize::new(0),
        peers: Mutex::new(HashMap::new()),
        puts: Mutex::new(Puts::default()),
        join_writes: Mutex::new(()),
        record_writes: Mutex::new(()),
        records_pulls: PullSlots::default(),
        advertiser: Mutex::new(None),
        #[cfg(test)]
        waiting: AtomicUsize::new(0),
        #[cfg(test)]
        served: AtomicUsize::new(0),
        #[cfg(test)]
        envelope_gets: AtomicUsize::new(0),
        #[cfg(test)]
        join_gets: AtomicUsize::new(0),
    });
    let (refusals, refused) = sync_channel::<TcpStream>(REFUSALS_QUEUED);
    std::thread::Builder::new()
        .name("vault-refuse".to_owned())
        .stack_size(CONNECTION_STACK)
        .spawn(move || {
            for mut stream in refused {
                linger(&mut stream, tuning.linger);
            }
        })?;
    let stop = Arc::new(AtomicBool::new(false));
    let accept = {
        let shared = Arc::clone(&shared);
        let stop = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("vault-accept".to_owned())
            .spawn(move || accept_loop(&listener, &shared, &stop, &refusals))?
    };
    Ok(Running {
        addr,
        stop,
        shared,
        accept: Some(accept),
    })
}

/// Whether a connection from `peer` comes from this machine.
fn this_machine(peer: IpAddr, arrived_on: Option<IpAddr>) -> bool {
    let peer = peer.to_canonical();
    peer.is_loopback()
        || arrived_on.is_some_and(|local| local.to_canonical() == peer)
        || primary_ipv4().is_some_and(|ip| IpAddr::V4(ip) == peer)
        || primary_ipv6().is_some_and(|ip| IpAddr::V6(ip) == peer)
}

/// What the server holds for one vault.
struct Held {
    seq: u64,
    signer: [u8; 32],
    head: [u8; 32],
    /// Whether this held head or an ancestor of it carried non-empty `parents` (parent-CAS active).
    parented: bool,
    /// The held envelope's handoff chain ([`envelope::Retired::gives_way`]).
    handoff: Vec<([u8; 32], [u8; 64])>,
    bytes: Arc<Vec<u8>>,
}

/// One vault and the condition its long-polls wait on.
struct Slot {
    held: Held,
    wake: Arc<Condvar>,
    /// Verdicts kept on this vault's join requests since the start: each one ends the
    /// long-polls held then, so a refused Mac waiting on one reads it at once.
    verdicts: u64,
    /// The version of this vault's join queue, moved by each change to it (a request queued,
    /// changed or expired): the queue's `ETag`, and each move ends the join long-polls held then
    /// ([`list_joins`]). It starts at [`queue_epoch`], so a version an approver heard from an
    /// earlier run of this server names no version of this run's queue.
    joins: u64,
}

impl Slot {
    fn new(held: Held) -> Slot {
        Slot {
            held,
            wake: Arc::new(Condvar::new()),
            verdicts: 0,
            joins: queue_epoch(),
        }
    }
}

/// Where a join queue's version starts in this run of the server: random (else the clock's
/// nanoseconds), below 2^62 so its moves never wrap. A server put back with a fresh count would
/// otherwise reach a version an approver holds from before it, and hold that approver's
/// long-poll while a request it never heard waits in the queue.
fn queue_epoch() -> u64 {
    let mut bytes = [0u8; 8];
    let random = aterm_uds::rand::fill(&mut bytes)
        .map(|()| u64::from_le_bytes(bytes))
        .unwrap_or_else(|_| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos() as u64)
        });
    random >> 2
}

/// A queued join request and the address it came from (`None`: no `peer` line in the store).
#[derive(Clone)]
struct Queued {
    request: JoinRequest,
    peer: Option<IpAddr>,
    /// A real POST of this recipient was observed on the LAN. Kept separately from the
    /// first peer, which still accounts for rate limits, and persisted in the listed line.
    observed_lan: bool,
    /// The vault's seq when the request was last asked (queued, changed or asked again).
    asked: u64,
    /// When its address last queued or changed it.
    changed: u64,
    /// When the log last noted it (unix secs; 0: not since the server started).
    noted: u64,
    /// The tag of its proof an admin Mac's verdict refused (`POST …/join/verdict`), kept in
    /// memory only.
    refused: Option<String>,
}

/// One vault's membership-record log and the condition its long-polls wait on.
struct RecordSlot {
    store: crate::membership_log::Store,
    /// The records file's bytes as this server last read or wrote them, so an unchanged file is
    /// not decoded again ([`take_records_from_store`]).
    raw: Vec<u8>,
    wake: Arc<Condvar>,
}

impl RecordSlot {
    fn new(store: crate::membership_log::Store, raw: Vec<u8>) -> Self {
        Self {
            store,
            raw,
            wake: Arc::new(Condvar::new()),
        }
    }
}

#[derive(Default)]
struct State {
    vaults: BTreeMap<String, Slot>,
    records: BTreeMap<String, RecordSlot>,
    joins: BTreeMap<String, Vec<Queued>>,
    heard: BTreeMap<String, BTreeMap<String, u64>>,
    heard_flushed: BTreeMap<String, u64>,
    heard_dirty: BTreeSet<String>,
}

struct Shared {
    store: PathBuf,
    pins: Mutex<Vec<(String, [u8; 32])>>,
    /// The keys the ALab vault rotated away from ([`crate::ALAB_RETIRED`]).
    retired: envelope::Retired,
    hold: Duration,
    tuning: Tuning,
    server_state: Mutex<State>,
    stopping: AtomicBool,
    active: AtomicUsize,
    /// Connections open now, by the address they came from.
    peers: Mutex<HashMap<IpAddr, usize>>,
    /// PUT bodies being read now.
    puts: Mutex<Puts>,
    /// Held while a join request is decided and its store written, taken before `server_state`.
    join_writes: Mutex<()>,
    /// Held while membership records are verified and persisted, taken before `server_state`.
    record_writes: Mutex<()>,
    /// The records pulls from peers under way now ([`RECORDS_PULLS_AT_ONCE`]).
    records_pulls: PullSlots,
    /// The bell the `vault-tend` thread naps on when advertising, rung so a new version updates
    /// its TXT at once.
    advertiser: Mutex<Option<Arc<Bell>>>,
    #[cfg(test)]
    waiting: AtomicUsize,
    /// Connections admitted since the start.
    #[cfg(test)]
    served: AtomicUsize,
    /// GETs of a vault's envelope (`/v1/<name>`, long-polls among them) since the start.
    #[cfg(test)]
    envelope_gets: AtomicUsize,
    /// GETs of a vault's join queue (`/v1/<name>/join`, long-polls among them) since the start.
    #[cfg(test)]
    join_gets: AtomicUsize,
}

impl Shared {
    fn lock_server_state(&self) -> MutexGuard<'_, State> {
        self.server_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn pinned_key(&self, name: &str) -> Option<[u8; 32]> {
        self.pins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(pinned, _)| pinned == name)
            .map(|(_, key)| *key)
    }

    fn pins_snapshot(&self) -> Vec<(String, [u8; 32])> {
        self.pins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn merge_pins(&self, pins: &mut Vec<(String, [u8; 32])>) {
        let mut held = self.pins.lock().unwrap_or_else(PoisonError::into_inner);
        for (vault_name, key) in pins.iter() {
            if !held.iter().any(|(existing, _)| existing == vault_name) {
                held.push((vault_name.clone(), *key));
            }
        }
        pins.clone_from(&held);
    }

    fn lock_peers(&self) -> MutexGuard<'_, HashMap<IpAddr, usize>> {
        self.peers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_puts(&self) -> MutexGuard<'_, Puts> {
        self.puts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn join_writes(&self) -> MutexGuard<'_, ()> {
        self.join_writes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn vault_path(&self, name: &str) -> PathBuf {
        self.store.join(format!("{name}.vault"))
    }

    fn joins_path(&self, name: &str) -> PathBuf {
        self.store.join(format!("{name}.joins"))
    }

    fn heard_path(&self, name: &str) -> PathBuf {
        self.store.join(format!("{name}.heard"))
    }

    fn records_path(&self, name: &str) -> PathBuf {
        self.store.join(format!("{name}.records"))
    }

    fn wake_advertiser(&self) {
        if let Some(bell) = self
            .advertiser
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            bell.ring(Wake::Put);
        }
    }
}

fn now() -> u64 {
    aterm_time::unix_secs().unwrap_or(0)
}

/// A line for the server's log (stderr). A log that cannot take it (a full disk) loses the line,
/// not the server: `eprintln!` panics when the write fails.
fn note(line: &str) {
    crate::log_line(&format!("aterm secrets serve: {line}"));
}

/// What [`serve`] logs at start when its store holds no vault yet ([`refuse_unheld`]).
fn unheld_note(state: &State, pins: &[(String, [u8; 32])]) -> Option<String> {
    if !state.vaults.is_empty() {
        return None;
    }
    let from = if pins.is_empty() {
        "only from this Mac, since no --pin names a vault".to_string()
    } else {
        let names: Vec<&str> = pins.iter().map(|(name, _)| name.as_str()).collect();
        format!(
            "from this Mac, or from another for a vault this server pins ({})",
            names.join(", ")
        )
    };
    Some(format!(
        "the store holds no vault yet: a Mac in the vault puts its copy here (its subscriber does, and any aterm secrets admin command there), and until then a machine that asks to join finds none; a first version is taken {from}"
    ))
}

/// Read the store: every `<name>.vault` that parses and verifies, and every `<name>.joins` line
/// that parses and has not expired.
fn load(store: &Path, now: u64) -> crate::Result<State> {
    let mut state = State::default();
    let entries =
        std::fs::read_dir(store).map_err(|e| Error::new(format!("{}: {e}", store.display())))?;
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    // Vaults first: a record store follows the key of the envelope held beside it.
    names.sort_by(|a, b| (!a.ends_with(".vault"), a).cmp(&(!b.ends_with(".vault"), b)));
    for file in names {
        let path = store.join(&file);
        if let Some(name) = file
            .strip_suffix(".vault")
            .filter(|n| envelope::valid_name(n))
        {
            if vault_capacity_exceeded(&state, name) {
                note(&format!(
                    "{file}: skipped; the server holds at most {MAX_VAULTS} vaults"
                ));
                continue;
            }
            match load_vault(&path, name) {
                Ok(held) => {
                    state.vaults.insert(name.to_owned(), Slot::new(held));
                }
                Err(e) => note(&format!("{file}: skipped: {e}")),
            }
        } else if let Some(name) = file
            .strip_suffix(".records")
            .filter(|n| envelope::valid_name(n))
        {
            if vault_capacity_exceeded(&state, name) {
                note(&format!(
                    "{file}: skipped; the server holds at most {MAX_VAULTS} vaults"
                ));
                continue;
            }
            match fsx::read_owned(&path, crate::membership_log::MAX_RECORDS_STREAM) {
                Ok(Some(bytes)) => match decode_records_held(&state, name, &bytes) {
                    Ok(decoded) => {
                        state
                            .records
                            .insert(name.to_owned(), RecordSlot::new(decoded, bytes));
                    }
                    Err(e) => note(&format!("{file}: skipped: {e}")),
                },
                Ok(None) => {}
                Err(e) => note(&format!("{file}: skipped: {e}")),
            }
        } else if let Some(name) = file
            .strip_suffix(".joins")
            .filter(|n| envelope::valid_name(n))
        {
            match fsx::read_owned(&path, MAX_JOINS_FILE) {
                Ok(Some(bytes)) => {
                    let mut queue = parse_stored(&String::from_utf8_lossy(&bytes));
                    expire(&mut queue, now);
                    queue.truncate(MAX_JOINS);
                    state.joins.insert(name.to_owned(), queue);
                }
                Ok(None) => {}
                Err(e) => note(&format!("{file}: skipped: {e}")),
            }
        } else if let Some(name) = file
            .strip_suffix(".heard")
            .filter(|n| envelope::valid_name(n))
        {
            match fsx::read_owned(&path, MAX_HEARD_FILE) {
                Ok(Some(bytes)) => {
                    let map = parse_heard(&String::from_utf8_lossy(&bytes), Some(now));
                    if !map.is_empty() {
                        state.heard.insert(name.to_owned(), map);
                        state.heard_flushed.insert(name.to_owned(), now);
                    }
                }
                Ok(None) => {}
                Err(e) => note(&format!("{file}: skipped: {e}")),
            }
        }
    }
    Ok(state)
}

/// Read every `<name>.heard` last-heard record persisted in `store` (`(vault, recipient, at_secs)`).
pub fn read_last_heard(store: &Path) -> Vec<(String, String, u64)> {
    let Ok(entries) = std::fs::read_dir(store) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    names.sort();
    let mut out = Vec::new();
    for file in names {
        let Some(name) = file
            .strip_suffix(".heard")
            .filter(|n| envelope::valid_name(n))
        else {
            continue;
        };
        let Ok(Some(bytes)) = fsx::read_owned(&store.join(&file), MAX_HEARD_FILE) else {
            continue;
        };
        for (recipient, at) in parse_heard(&String::from_utf8_lossy(&bytes), None) {
            out.push((name.to_owned(), recipient, at));
        }
    }
    out
}

fn parse_heard(text: &str, now: Option<u64>) -> BTreeMap<String, u64> {
    let mut rows: Vec<(String, u64)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_ascii_whitespace();
        let (Some(recipient), Some(at), None) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if crate::age::Recipient::parse(recipient).is_err() {
            continue;
        }
        let Ok(at) = at.parse::<u64>() else {
            continue;
        };
        if let Some(now) = now
            && now.saturating_sub(at) > HEARD_TTL_SECS
        {
            continue;
        }
        rows.push((recipient.to_owned(), at));
    }
    let mut map = BTreeMap::new();
    for (recipient, at) in rows {
        let entry = map.entry(recipient).or_insert(0);
        *entry = (*entry).max(at);
    }
    while map.len() > MAX_HEARD {
        let Some(oldest) = map
            .iter()
            .min_by_key(|&(_, &at)| at)
            .map(|(recipient, _)| recipient.clone())
        else {
            break;
        };
        map.remove(&oldest);
    }
    map
}

fn format_heard(map: &BTreeMap<String, u64>) -> String {
    let mut text = String::new();
    for (recipient, at) in map {
        text.push_str(recipient);
        text.push(' ');
        text.push_str(&at.to_string());
        text.push('\n');
    }
    text
}

fn record_heard(shared: &Shared, state: &mut State, name: &str, subscriber: &str, now: u64) {
    let map = state.heard.entry(name.to_owned()).or_default();
    map.retain(|_, &mut at| now.saturating_sub(at) <= HEARD_TTL_SECS);
    if !map.contains_key(subscriber)
        && map.len() >= MAX_HEARD
        && let Some(oldest) = map
            .iter()
            .min_by_key(|&(_, &at)| at)
            .map(|(recipient, _)| recipient.clone())
    {
        map.remove(&oldest);
    }
    map.insert(subscriber.to_owned(), now);
    state.heard_dirty.insert(name.to_owned());
    let flush_due = match state.heard_flushed.get(name) {
        None => true,
        Some(&last) => now.saturating_sub(last) >= HEARD_FLUSH_SECS,
    };
    if flush_due {
        let body = format_heard(map);
        if fsx::write_atomic(&shared.heard_path(name), body.as_bytes(), 0o600).is_ok() {
            state.heard_flushed.insert(name.to_owned(), now);
            state.heard_dirty.remove(name);
        }
    }
}

fn flush_all_heard(shared: &Shared, state: &mut State) {
    let dirty: Vec<String> = state.heard_dirty.iter().cloned().collect();
    let now = (shared.tuning.now)();
    for name in dirty {
        if let Some(map) = state.heard.get(&name) {
            let body = format_heard(map);
            if fsx::write_atomic(&shared.heard_path(&name), body.as_bytes(), 0o600).is_ok() {
                state.heard_flushed.insert(name.clone(), now);
                state.heard_dirty.remove(&name);
            }
        }
    }
}

fn load_vault(path: &Path, name: &str) -> crate::Result<Held> {
    let bytes = fsx::read_owned(path, MAX_ENVELOPE)?.ok_or_else(|| Error::new("vanished"))?;
    let envelope = envelope::parse(&bytes)?;
    if envelope.name != name {
        return Err(Error::new(format!(
            "holds the vault `{}`, not `{name}`",
            envelope.name
        )));
    }
    envelope.verify_signature()?;
    let head = envelope.digest();
    Ok(Held {
        seq: envelope.seq,
        signer: envelope.signer,
        head,
        parented: !envelope.parents.is_empty(),
        handoff: envelope.handoff,
        bytes: Arc::new(bytes),
    })
}

use crate::ALAB_VAULT;

/// Whether `store` holds the ALab vault: an `alab.vault` that verifies, signed by a key this
/// build names for it or one reached from such a key by verified handoffs
/// ([`envelope::Retired::lineage`]), a retired one included: until the version that replaces it
/// arrives, that version is still the ALab vault this server holds.
pub(crate) fn holds_alab(store: &Path) -> bool {
    let Ok(Some(bytes)) = fsx::read_owned(&store.join(format!("{ALAB_VAULT}.vault")), MAX_ENVELOPE)
    else {
        return false;
    };
    is_alab_vault(&bytes)
}

/// Whether `bytes` are a version of the ALab vault by [`holds_alab`]'s rule: one that verifies,
/// signed by a key this build names for it or one reached from such a key by verified handoffs.
pub(crate) fn is_alab_vault(bytes: &[u8]) -> bool {
    envelope::parse(bytes).is_ok_and(|envelope| {
        envelope.name == ALAB_VAULT
            && envelope.verify_signature().is_ok()
            && envelope::Retired::compiled().lineage(&envelope.signer, &envelope.reached_from())
    })
}

/// Drop requests queued more than [`JOIN_TTL_SECS`] from now either way.
fn expire(queue: &mut Vec<Queued>, now: u64) {
    queue.retain(|queued| {
        queued
            .request
            .time
            .is_some_and(|time| time.abs_diff(now) <= JOIN_TTL_SECS)
    });
}

/// Whether a join came directly from this machine or a private/link-local LAN address.
fn lan_join_peer(peer: IpAddr) -> bool {
    match peer {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or_else(
            || ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
            |ip| lan_join_peer(IpAddr::V4(ip)),
        ),
    }
}

/// The listing carries LAN eligibility without disclosing the request's address.
fn join_lines(queue: &[Queued]) -> String {
    let mut text = String::new();
    for queued in queue {
        let mut request = queued.request.clone();
        request.lan = queued.observed_lan || queued.peer.is_some_and(lan_join_peer);
        text.push_str(&request.listed());
        text.push('\n');
    }
    text
}

/// The store's form of a queue ([`Queued::asked`]).
fn stored_lines(queue: &[Queued]) -> String {
    let mut text = String::new();
    for queued in queue {
        if let Some(peer) = queued.peer {
            text.push_str(&format!("peer {peer} {}\n", queued.asked));
        }
        let mut request = queued.request.clone();
        request.lan = queued.observed_lan;
        text.push_str(&request.listed());
        text.push('\n');
    }
    text
}

/// Read [`stored_lines`].
fn parse_stored(text: &str) -> Vec<Queued> {
    let mut queue = Vec::new();
    let mut peer = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("peer ") {
            let (address, asked) = rest.split_once(' ').unwrap_or((rest, "0"));
            peer = address
                .parse::<IpAddr>()
                .ok()
                .map(|ip| (ip, asked.parse::<u64>().unwrap_or(0)));
            continue;
        }
        let (from, asked) = peer.take().unzip();
        if let Ok(mut request) = parse_join_line(line)
            && request.time.is_some()
        {
            let observed_lan = request.lan || from.is_some_and(lan_join_peer);
            request.lan = false;
            queue.push(Queued {
                request,
                peer: from,
                observed_lan,
                asked: asked.unwrap_or(0),
                changed: 0,
                noted: 0,
                refused: None,
            });
        }
    }
    queue
}

fn accept_loop(
    listener: &TcpListener,
    shared: &Arc<Shared>,
    stop: &AtomicBool,
    refusals: &SyncSender<TcpStream>,
) -> io::Result<()> {
    loop {
        let accepted = listener.accept();
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        match accepted {
            Ok((stream, peer)) => {
                let _ = aterm_uds::keep_out_of_children(&stream);
                dispatch(shared, stream, peer.ip(), refusals);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if is_transient(&e) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e),
        }
    }
}

fn is_transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::TimedOut
            | io::ErrorKind::OutOfMemory
    ) || matches!(
        error.raw_os_error(),
        Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM)
    )
}

/// One connection's slot under [`MAX_CONNECTIONS`] and [`MAX_PEER_CONNECTIONS`].
struct Active {
    shared: Arc<Shared>,
    peer: IpAddr,
}

impl Drop for Active {
    fn drop(&mut self) {
        {
            let mut peers = self.shared.lock_peers();
            if let Some(open) = peers.get_mut(&self.peer) {
                *open = open.saturating_sub(1);
                if *open == 0 {
                    peers.remove(&self.peer);
                }
            }
        }
        self.shared.active.fetch_sub(1, Ordering::SeqCst);
    }
}

fn dispatch(
    shared: &Arc<Shared>,
    stream: TcpStream,
    peer: IpAddr,
    refusals: &SyncSender<TcpStream>,
) {
    let peer = peer.to_canonical();
    if shared.active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
        shared.active.fetch_sub(1, Ordering::SeqCst);
        refuse(
            stream,
            format!("the server is at its {MAX_CONNECTIONS} connections; try again"),
            refusals,
        );
        return;
    }
    let admitted = {
        let mut peers = shared.lock_peers();
        let open = peers.entry(peer).or_insert(0);
        let admitted = *open < MAX_PEER_CONNECTIONS;
        if admitted {
            *open += 1;
        }
        admitted
    };
    if !admitted {
        shared.active.fetch_sub(1, Ordering::SeqCst);
        refuse(
            stream,
            format!(
                "too many connections from your address (at most {MAX_PEER_CONNECTIONS}); try again"
            ),
            refusals,
        );
        return;
    }
    #[cfg(test)]
    shared.served.fetch_add(1, Ordering::SeqCst);
    let active = Active {
        shared: Arc::clone(shared),
        peer,
    };
    let spawned = std::thread::Builder::new()
        .name("vault-conn".to_owned())
        .stack_size(CONNECTION_STACK)
        .spawn(move || {
            handle(&active.shared, stream, active.peer);
            drop(active);
        });
    drop(spawned);
}

/// Answer a connection over a cap with 503 from the accept loop ([`linger`]).
fn refuse(mut stream: TcpStream, why: String, refusals: &SyncSender<TcpStream>) {
    let sent = write_reply(
        &mut stream,
        &Reply::text(503, why),
        REFUSAL_DEADLINE,
        REFUSAL_DEADLINE,
    );
    if sent.is_ok() {
        let _ = refusals.try_send(stream);
    }
}

/// The PUT bodies being read now.
#[derive(Default)]
struct Puts {
    reading: usize,
    by_peer: HashMap<IpAddr, usize>,
}

/// One PUT body being read under [`MAX_PUTS`] and [`MAX_PEER_PUTS`], released when dropped.
struct Reading<'a> {
    shared: &'a Shared,
    peer: IpAddr,
    /// From this machine.
    local: bool,
}

impl<'a> Reading<'a> {
    /// A slot to read a PUT body from `peer`, or the 503 that says which cap it met.
    fn take(shared: &'a Shared, peer: IpAddr, local: bool) -> Result<Reading<'a>, Reply> {
        let busy = |why: String| Err(Reply::text(503, why));
        let mut puts = shared.lock_puts();
        if puts.reading >= MAX_PUTS {
            return busy(format!(
                "the server is reading {MAX_PUTS} envelopes already; try again"
            ));
        }
        if !local {
            if puts.reading >= MAX_PUTS - 1 {
                return busy(format!(
                    "the server is reading {} envelopes from other machines already (the last slot is kept for its own); try again",
                    MAX_PUTS - 1
                ));
            }
            let from_peer = puts.by_peer.entry(peer).or_insert(0);
            if *from_peer >= MAX_PEER_PUTS {
                return busy(
                    "your address is sending an envelope already; send one at a time".to_owned(),
                );
            }
            *from_peer += 1;
        }
        puts.reading += 1;
        Ok(Reading {
            shared,
            peer,
            local,
        })
    }
}

impl Drop for Reading<'_> {
    fn drop(&mut self) {
        let mut puts = self.shared.lock_puts();
        puts.reading = puts.reading.saturating_sub(1);
        if !self.local
            && let Some(from_peer) = puts.by_peer.get_mut(&self.peer)
        {
            *from_peer = from_peer.saturating_sub(1);
            if *from_peer == 0 {
                puts.by_peer.remove(&self.peer);
            }
        }
    }
}

/// Response header naming the SHA-256 digest of the envelope the server currently holds.
pub const VAULT_HEAD_HEADER: &str = "X-Aterm-Vault-Head";

/// An answer.
struct Reply {
    status: u16,
    body: Body,
    etag: Option<u64>,
    vault_head: Option<[u8; 32]>,
    allow: Option<&'static str>,
    /// `Retry-After`, in seconds: when a refused request may be sent again and be taken.
    retry_after: Option<u64>,
    records_need: bool,
}

enum Body {
    Empty,
    Text(String),
    Envelope(Arc<Vec<u8>>),
    Records(Vec<u8>),
}

impl Reply {
    fn text(status: u16, why: impl Into<String>) -> Reply {
        let mut text: String = why.into();
        text.push('\n');
        Reply {
            status,
            body: Body::Text(text),
            etag: None,
            vault_head: None,
            allow: None,
            retry_after: None,
            records_need: false,
        }
    }

    fn conflict_with_head(held: &Held, why: impl Into<String>) -> Reply {
        let mut reply = Self::text(409, why);
        reply.etag = Some(held.seq);
        reply.vault_head = Some(held.head);
        reply
    }

    const fn empty(status: u16) -> Reply {
        Reply {
            status,
            body: Body::Empty,
            etag: None,
            vault_head: None,
            allow: None,
            retry_after: None,
            records_need: false,
        }
    }
}

const fn reason_phrase(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Content Too Large",
        417 => "Expectation Failed",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        505 => "HTTP Version Not Supported",
        507 => "Insufficient Storage",
        _ => "Unknown",
    }
}

/// Send `reply`, no write blocking longer than `stall` and the whole within `within`.
fn write_reply(
    stream: &mut TcpStream,
    reply: &Reply,
    stall: Duration,
    within: Duration,
) -> io::Result<()> {
    let deadline = Instant::now().checked_add(within);
    let (body, content_type): (&[u8], &str) = match &reply.body {
        Body::Empty => (&[], "text/plain; charset=utf-8"),
        Body::Text(text) => (text.as_bytes(), "text/plain; charset=utf-8"),
        Body::Envelope(bytes) => (bytes, "text/plain; charset=utf-8"),
        Body::Records(bytes) => (bytes, "application/octet-stream"),
    };
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nConnection: close\r\nCache-Control: no-store\r\n",
        reply.status,
        reason_phrase(reply.status)
    );
    if let Some(seq) = reply.etag {
        head.push_str(&format!("ETag: \"{seq}\"\r\n"));
    }
    if let Some(digest) = reply.vault_head {
        head.push_str(&format!(
            "{VAULT_HEAD_HEADER}: {}\r\n",
            hex32_encode(&digest)
        ));
    }
    if let Some(allow) = reply.allow {
        head.push_str(&format!("Allow: {allow}\r\n"));
    }
    if reply.records_need {
        head.push_str(&format!("{}: 1\r\n", crate::transport::RECORDS_NEED_HEADER));
    }
    if let Some(secs) = reply.retry_after {
        head.push_str(&format!("Retry-After: {secs}\r\n"));
    }
    if reply.status != 204 {
        head.push_str(&format!("Content-Type: {content_type}\r\n"));
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    send(stream, head.as_bytes(), stall, deadline)?;
    if reply.status != 204 {
        send(stream, body, stall, deadline)?;
    }
    stream.flush()
}

/// Write all of `bytes`, no write blocking longer than `stall` and none past `deadline`.
fn send(
    stream: &mut TcpStream,
    mut bytes: &[u8],
    stall: Duration,
    deadline: Option<Instant>,
) -> io::Result<()> {
    while !bytes.is_empty() {
        let wait = match deadline {
            Some(deadline) => deadline
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())
                .ok_or(io::ErrorKind::TimedOut)?
                .min(stall),
            None => stall,
        };
        stream.set_write_timeout(Some(wait))?;
        match stream.write(&bytes[..bytes.len().min(WRITE_CHUNK)]) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// One request's head.
struct Request {
    method: String,
    path: String,
    query: Option<String>,
    length: Option<usize>,
    expect_continue: bool,
    subscriber: Option<String>,
    no_wait: bool,
    /// Body bytes that arrived with the head.
    early: Vec<u8>,
}

fn handle(shared: &Shared, mut stream: TcpStream, peer: IpAddr) {
    let _ = stream.set_nodelay(true);
    let tuning = shared.tuning;
    let reply = match read_request(&mut stream, tuning.head_deadline) {
        Ok(Some(request)) => respond(shared, &mut stream, peer, request),
        Ok(None) => return,
        Err(reply) => reply,
    };
    if write_reply(
        &mut stream,
        &reply,
        tuning.write_stall,
        tuning.answer_deadline,
    )
    .is_ok()
    {
        linger(&mut stream, tuning.linger);
    }
}

/// Close our half and drain what the client still sends, for at most `deadline`, so a client
/// still writing (an oversize body) reads the answer instead of a reset.
fn linger(stream: &mut TcpStream, deadline: Duration) {
    if stream.shutdown(Shutdown::Write).is_err() {
        return;
    }
    let started = Instant::now();
    let mut drained = 0usize;
    let mut sink = [0u8; 8192];
    while drained < LINGER_BYTES {
        let Some(left) = deadline
            .checked_sub(started.elapsed())
            .filter(|left| !left.is_zero())
        else {
            return;
        };
        if stream.set_read_timeout(Some(left)).is_err() {
            return;
        }
        match stream.read(&mut sink) {
            Ok(0) | Err(_) => return,
            Ok(n) => drained += n,
        }
    }
}

fn find_blank_line(buf: &[u8], from: usize) -> Option<usize> {
    buf.get(from..)?
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| from + at + 4)
}

/// Read and parse one head within `deadline` ([`HEAD_DEADLINE`]) and [`MAX_HEAD`].
fn read_request(stream: &mut TcpStream, deadline: Duration) -> Result<Option<Request>, Reply> {
    let late = || {
        Reply::text(
            408,
            format!("the request head took longer than {deadline:?}"),
        )
    };
    let started = Instant::now();
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(end) = find_blank_line(&buf, buf.len().saturating_sub(chunk.len() + 3)) {
            break end;
        }
        if buf.len() >= MAX_HEAD {
            return Err(Reply::text(
                431,
                format!("the request head is larger than {MAX_HEAD} bytes"),
            ));
        }
        let Some(left) = deadline
            .checked_sub(started.elapsed())
            .filter(|left| !left.is_zero())
        else {
            return Err(late());
        };
        if stream.set_read_timeout(Some(left)).is_err() {
            return Ok(None);
        }
        match stream.read(&mut chunk) {
            Ok(0) if buf.is_empty() => return Ok(None),
            Ok(0) => return Err(Reply::text(400, "the request ended inside its head")),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(late());
            }
            Err(_) => return Ok(None),
        }
    };
    if end > MAX_HEAD {
        return Err(Reply::text(
            431,
            format!("the request head is larger than {MAX_HEAD} bytes"),
        ));
    }
    let early = buf.split_off(end);
    parse_head(&buf[..end - 4], early).map(Some)
}

fn parse_head(head: &[u8], early: Vec<u8>) -> Result<Request, Reply> {
    let bad = |why: &str| Reply::text(400, why);
    let head = std::str::from_utf8(head).map_err(|_| bad("the request head is not text"))?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let parts: Vec<&str> = request_line.split(' ').collect();
    let [method, target, version] = parts.as_slice() else {
        return Err(bad("the request line is not `METHOD /path HTTP/1.1`"));
    };
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(bad("the request method is not a method"));
    }
    if !target.starts_with('/') || !target.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(bad("the request target is not an absolute path"));
    }
    if !matches!(*version, "HTTP/1.1" | "HTTP/1.0") {
        return Err(Reply::text(505, "this server speaks HTTP/1.1"));
    }
    let mut length: Option<usize> = None;
    let mut expect_continue = false;
    let mut subscriber: Option<String> = None;
    let mut no_wait = false;
    for line in lines {
        if line.starts_with([' ', '\t']) {
            return Err(bad("folded header lines are not accepted"));
        }
        if line.contains(['\r', '\n']) {
            return Err(bad("header lines end in CRLF"));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| bad("a header line has no colon"))?;
        if name.is_empty() || !name.bytes().all(token_char) {
            return Err(bad("a header name is not a token"));
        }
        if !value
            .bytes()
            .all(|b| b == b'\t' || (0x20..0x7f).contains(&b) || b >= 0x80)
        {
            return Err(bad("a header value holds a control character"));
        }
        let value = value.trim_matches([' ', '\t']);
        if browser_metadata_header(name) {
            return Err(Reply::text(403, BROWSER_REFUSED));
        }
        if name.eq_ignore_ascii_case("content-length") {
            let parsed = (!value.is_empty()
                && value.len() <= 12
                && value.bytes().all(|b| b.is_ascii_digit()))
            .then(|| value.parse::<usize>().ok())
            .flatten()
            .ok_or_else(|| bad("Content-Length is not a byte count"))?;
            if length.is_some_and(|first| first != parsed) {
                return Err(bad("conflicting Content-Length headers"));
            }
            length = Some(parsed);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(Reply::text(
                501,
                "chunked bodies are not accepted; send Content-Length",
            ));
        } else if name.eq_ignore_ascii_case("expect") {
            if value.eq_ignore_ascii_case("100-continue") {
                expect_continue = true;
            } else {
                return Err(Reply::text(417, "the only expectation met is 100-continue"));
            }
        } else if name.eq_ignore_ascii_case(crate::transport::SUBSCRIBER_HEADER)
            && crate::age::Recipient::parse(value).is_ok()
        {
            subscriber = Some(value.to_owned());
        } else if name.eq_ignore_ascii_case(crate::transport::NO_WAIT_HEADER) && value == "1" {
            no_wait = true;
        }
    }
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, Some(query.to_owned())),
        None => (*target, None),
    };
    Ok(Request {
        method: (*method).to_owned(),
        path: path.to_owned(),
        query,
        length,
        expect_continue,
        subscriber,
        no_wait,
        early,
    })
}

/// Why a request carrying browser metadata is refused.
const BROWSER_REFUSED: &str =
    "browser-origin or fetch-metadata headers are not accepted (Origin or Sec-Fetch-*)";

/// Reject browser request metadata before processing a body or admitting a join.
fn browser_metadata_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("origin")
        || name
            .get(..10)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("sec-fetch-"))
}

fn token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// A path this server answers.
enum Route<'a> {
    Vault(&'a str),
    Records(&'a str),
    Join(&'a str),
    Verdict(&'a str),
}

/// `/v1/<name>`, `/v1/<name>/records`, `/v1/<name>/join` or `/v1/<name>/join/verdict`, with
/// `<name>` a vault name.
fn route(path: &str) -> Option<Route<'_>> {
    let rest = path.strip_prefix("/v1/")?;
    let route = match rest.split_once('/') {
        None => Route::Vault(rest),
        Some((name, "records")) => Route::Records(name),
        Some((name, "join")) => Route::Join(name),
        Some((name, "join/verdict")) => Route::Verdict(name),
        Some(_) => return None,
    };
    let (Route::Vault(name) | Route::Records(name) | Route::Join(name) | Route::Verdict(name)) =
        route;
    envelope::valid_name(name).then_some(route)
}

fn respond(shared: &Shared, stream: &mut TcpStream, peer: IpAddr, mut request: Request) -> Reply {
    let path = std::mem::take(&mut request.path);
    let Some(route) = route(&path) else {
        return Reply::text(
            404,
            "no such path: the vault paths are /v1/<name>, /v1/<name>/records, /v1/<name>/join and /v1/<name>/join/verdict",
        );
    };
    let method = request.method.as_str();
    if !matches!(
        (&route, method),
        (Route::Vault(_), "GET" | "PUT")
            | (
                Route::Records(_) | Route::Join(_) | Route::Verdict(_),
                "GET" | "POST"
            )
    ) {
        let allow = match route {
            Route::Vault(_) => "GET, PUT",
            Route::Records(_) | Route::Join(_) | Route::Verdict(_) => "GET, POST",
        };
        let mut reply = Reply::text(405, format!("{method} is not answered here; use {allow}"));
        reply.allow = Some(allow);
        return reply;
    }
    if method == "GET" && (request.length.unwrap_or(0) > 0 || !request.early.is_empty()) {
        return Reply::text(400, "a GET carries no body");
    }
    let waits = matches!(
        (&route, method),
        (Route::Vault(_) | Route::Records(_) | Route::Join(_), "GET")
    );
    if request.query.is_some() && !waits {
        return Reply::text(400, "this path takes no query");
    }
    match (route, method) {
        (Route::Vault(name), "GET") => {
            #[cfg(test)]
            shared.envelope_gets.fetch_add(1, Ordering::SeqCst);
            get_vault(
                shared,
                stream,
                name,
                request.query.as_deref(),
                request.subscriber.as_deref(),
                request.no_wait,
            )
        }
        (Route::Vault(name), _) => {
            let length = match body_length(&request, MAX_ENVELOPE) {
                Ok(length) => length,
                Err(reply) => return reply,
            };
            let arrived_on = stream.local_addr().ok().map(|addr| addr.ip());
            let local = (shared.tuning.is_local)(peer, arrived_on);
            let refused = refuse_unheld(shared, &shared.lock_server_state(), name, local);
            if let Some(reply) = refused {
                return reply;
            }
            let _reading = match Reading::take(shared, peer, local) {
                Ok(reading) => reading,
                Err(reply) => return reply,
            };
            match read_body(stream, request, length, &shared.tuning) {
                Ok(body) => put_vault(shared, name, &body, local),
                Err(reply) => reply,
            }
        }
        (Route::Records(name), "GET") => {
            let arrived_on = stream.local_addr().ok().map(|addr| addr.ip());
            let local = (shared.tuning.is_local)(peer, arrived_on);
            get_records(
                shared,
                stream,
                name,
                request.query.as_deref(),
                request.subscriber.as_deref(),
                request.no_wait,
                local,
            )
        }
        (Route::Records(name), _) => {
            let length = match body_length(&request, crate::membership_log::MAX_RECORDS_STREAM) {
                Ok(length) => length,
                Err(reply) => return reply,
            };
            let arrived_on = stream.local_addr().ok().map(|addr| addr.ip());
            let local = (shared.tuning.is_local)(peer, arrived_on);
            let refused = refuse_unheld_records(shared, &shared.lock_server_state(), name, local);
            if let Some(reply) = refused {
                return reply;
            }
            let _reading = match Reading::take(shared, peer, local) {
                Ok(reading) => reading,
                Err(reply) => return reply,
            };
            match read_body(stream, request, length, &shared.tuning) {
                Ok(body) => post_records(shared, name, &body, local),
                Err(reply) => reply,
            }
        }
        (Route::Join(name), "GET") => {
            #[cfg(test)]
            shared.join_gets.fetch_add(1, Ordering::SeqCst);
            list_joins(shared, stream, name, request.query.as_deref())
        }
        (Route::Join(name), _) => match body_length(&request, MAX_JOIN_BODY)
            .and_then(|length| read_body(stream, request, length, &shared.tuning))
        {
            Ok(body) => post_join(shared, name, &body, peer),
            Err(reply) => reply,
        },
        (Route::Verdict(name), "GET") => list_verdicts(shared, name),
        (Route::Verdict(name), _) => match body_length(&request, MAX_JOIN_BODY)
            .and_then(|length| read_body(stream, request, length, &shared.tuning))
        {
            Ok(body) => post_verdict(shared, name, &body),
            Err(reply) => reply,
        },
    }
}

fn vault_capacity_exceeded(state: &State, name: &str) -> bool {
    if state.vaults.contains_key(name) || state.records.contains_key(name) {
        return false;
    }
    let distinct = state
        .vaults
        .keys()
        .chain(state.records.keys())
        .collect::<BTreeSet<_>>()
        .len();
    distinct >= MAX_VAULTS
}

/// Why a PUT for `name` is refused whatever it carries, when the store holds nothing for `name`.
fn refuse_unheld(shared: &Shared, state: &State, name: &str, local: bool) -> Option<Reply> {
    if state.vaults.contains_key(name) {
        return None;
    }
    if !local && shared.pinned_key(name).is_none() {
        return Some(Reply::text(
            403,
            "the first version of a vault is accepted only from the server's own Mac, or under aterm secrets serve --pin <vault>=<key>",
        ));
    }
    if vault_capacity_exceeded(state, name) {
        return Some(Reply::text(
            507,
            format!("this server holds its limit of {MAX_VAULTS} vaults"),
        ));
    }
    None
}

/// Whether `state` holds at least one membership record for `name` (not an empty slot a
/// long-poll parked on before any record arrived).
fn holds_records(state: &State, name: &str) -> bool {
    state
        .records
        .get(name)
        .is_some_and(|slot| !slot.store.is_empty())
}

/// Why a `POST /v1/<name>/records` is refused before reading its body.
fn refuse_unheld_records(shared: &Shared, state: &State, name: &str, local: bool) -> Option<Reply> {
    if holds_records(state, name) || state.vaults.contains_key(name) {
        return None;
    }
    if !local && name != crate::ALAB_VAULT && shared.pinned_key(name).is_none() {
        return Some(Reply::text(
            403,
            "the first records of a non-ALab vault are accepted only from the server's own Mac, or under aterm secrets serve --pin <vault>=<key>",
        ));
    }
    if vault_capacity_exceeded(state, name) {
        return Some(Reply::text(
            507,
            format!("this server holds its limit of {MAX_VAULTS} vaults"),
        ));
    }
    None
}

/// A body's `Content-Length`.
fn body_length(request: &Request, max: usize) -> Result<usize, Reply> {
    let Some(length) = request.length else {
        return Err(Reply::text(
            411,
            "a body needs Content-Length (chunked is not accepted)",
        ));
    };
    if length > max {
        return Err(Reply::text(
            413,
            format!("the body is larger than {max} bytes"),
        ));
    }
    if request.early.len() > length {
        return Err(Reply::text(
            400,
            "bytes follow the body (one request per connection)",
        ));
    }
    Ok(length)
}

/// Read the `length` bytes of body [`body_length`] admitted, within the body deadline and stall.
fn read_body(
    stream: &mut TcpStream,
    request: Request,
    length: usize,
    tuning: &Tuning,
) -> Result<Vec<u8>, Reply> {
    let mut body = request.early;
    if request.expect_continue && body.len() < length {
        let continued = stream
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .and_then(|()| stream.flush());
        if continued.is_err() {
            return Err(Reply::text(400, "the connection closed"));
        }
    }
    let started = Instant::now();
    let mut chunk = vec![0u8; 64 * 1024];
    while body.len() < length {
        let Some(left) = tuning
            .body_deadline
            .checked_sub(started.elapsed())
            .filter(|left| !left.is_zero())
        else {
            return Err(Reply::text(408, "the body took too long"));
        };
        let wait = left.min(tuning.body_stall);
        if stream.set_read_timeout(Some(wait)).is_err() {
            return Err(Reply::text(400, "the connection closed"));
        }
        let want = (length - body.len()).min(chunk.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) => return Err(Reply::text(400, "the body ended before its Content-Length")),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                let why = if wait == left {
                    "the body took too long"
                } else {
                    "the body stalled"
                };
                return Err(Reply::text(408, why));
            }
            Err(_) => return Err(Reply::text(400, "the connection closed")),
        }
    }
    Ok(body)
}

fn envelope_reply(held: &Held) -> Reply {
    Reply {
        status: 200,
        body: Body::Envelope(Arc::clone(&held.bytes)),
        etag: Some(held.seq),
        vault_head: Some(held.head),
        allow: None,
        retry_after: None,
        records_need: false,
    }
}

/// The `n` of a long-poll's query, `after=<n>` (`None`: no query).
fn after_query(query: Option<&str>) -> Result<Option<u64>, Reply> {
    let Some(query) = query else {
        return Ok(None);
    };
    match query.strip_prefix("after=") {
        Some(digits)
            if !digits.is_empty()
                && digits.len() <= 20
                && digits.bytes().all(|b| b.is_ascii_digit()) =>
        {
            digits
                .parse::<u64>()
                .map(Some)
                .map_err(|_| Reply::text(400, "after= is not a number"))
        }
        _ => Err(Reply::text(400, "the only query is after=<n>")),
    }
}

/// Hold a long-poll on `wake` until `done` says so, the server stops, [`Shared::hold`] runs
/// out or the client goes away ([`gone`]), and return the state then. `done` is asked first,
/// and again after each wake, under `server_state`.
fn park_until_news<'s>(
    shared: &'s Shared,
    stream: &TcpStream,
    mut state: MutexGuard<'s, State>,
    wake: &Condvar,
    mut done: impl FnMut(&mut State) -> bool,
) -> MutexGuard<'s, State> {
    let deadline = Instant::now().checked_add(shared.hold);
    let every = shared.tuning.liveness_every;
    #[cfg(test)]
    shared.waiting.fetch_add(1, Ordering::SeqCst);
    loop {
        if done(&mut state) || shared.stopping.load(Ordering::SeqCst) {
            break;
        }
        let slice = match deadline {
            Some(deadline) => match deadline
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())
            {
                Some(left) => left.min(every),
                None => break,
            },
            None => every,
        };
        let (next, waited) = wake
            .wait_timeout(state, slice)
            .unwrap_or_else(PoisonError::into_inner);
        state = next;
        if waited.timed_out() && gone(stream) {
            break;
        }
    }
    #[cfg(test)]
    shared.waiting.fetch_sub(1, Ordering::SeqCst);
    state
}

fn get_vault(
    shared: &Shared,
    stream: &TcpStream,
    name: &str,
    query: Option<&str>,
    subscriber: Option<&str>,
    no_wait: bool,
) -> Reply {
    let after = match after_query(query) {
        Ok(after) => after,
        Err(reply) => return reply,
    };
    let mut state = shared.lock_server_state();
    take_from_store(shared, &mut state, name);
    if !state.vaults.contains_key(name) {
        return Reply::text(404, format!("no vault named {name} here"));
    }
    if let Some(subscriber) = subscriber {
        let now = (shared.tuning.now)();
        record_heard(shared, &mut state, name, subscriber, now);
    }
    let Some(slot) = state.vaults.get(name) else {
        return Reply::text(404, format!("no vault named {name} here"));
    };
    let Some(after) = after else {
        return envelope_reply(&slot.held);
    };
    if slot.held.seq > after {
        return envelope_reply(&slot.held);
    }
    if no_wait {
        let mut reply = Reply::empty(204);
        reply.etag = Some(slot.held.seq);
        return reply;
    }
    let wake = Arc::clone(&slot.wake);
    let verdicts = slot.verdicts;
    let state = park_until_news(shared, stream, state, &wake, |state| {
        take_from_store(shared, state, name);
        state
            .vaults
            .get(name)
            .is_some_and(|slot| slot.held.seq > after || slot.verdicts != verdicts)
    });
    match state.vaults.get(name) {
        Some(slot) if slot.held.seq > after => envelope_reply(&slot.held),
        Some(slot) => {
            let mut reply = Reply::empty(204);
            reply.etag = Some(slot.held.seq);
            reply
        }
        None => Reply::empty(204),
    }
}

/// Whether a held long-poll's client went away.
fn gone(stream: &TcpStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return true;
    }
    let mut byte = [0u8; 1];
    let gone = match stream.peek(&mut byte) {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) => !matches!(
            e.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
        ),
    };
    stream.set_nonblocking(false).is_err() || gone
}

fn put_vault(shared: &Shared, name: &str, body: &[u8], local: bool) -> Reply {
    let envelope = match envelope::parse(body) {
        Ok(envelope) => envelope,
        Err(e) => return Reply::text(400, e.0),
    };
    if envelope.name != name {
        return Reply::text(
            400,
            format!(
                "the envelope names the vault `{}`, not `{name}`",
                envelope.name
            ),
        );
    }
    if let Err(e) = envelope.verify_signature() {
        return Reply::text(403, e.0);
    }
    if let Err(e) = envelope.check_signer(&shared.retired) {
        return Reply::text(403, e.0);
    }
    let mut state = shared.lock_server_state();
    // Judged before the publish lock is taken, so a PUT this server refuses never holds it
    // against one it would take.
    match admit(shared, &mut state, name, &envelope, body, local) {
        Ok(Some(_)) => {}
        Ok(None) => return Reply::empty(204),
        Err(reply) => return reply,
    }
    #[cfg(test)]
    (shared.tuning.before_lock)(&shared.vault_path(name), envelope.seq);
    // Held from here to the rename, as a publish at the store path holds it, so neither
    // writer renames over a version the other just wrote; judged again under it, since such
    // a publish may have landed since.
    #[cfg(unix)]
    let _publishing =
        match crate::transport::PublishLock::take_clearing_dead(&shared.vault_path(name)) {
            Ok(lock) => lock,
            // Only a sender of a version the server would take gets here (an admin, or a
            // machine putting its copy back), so the answer names the lock by its file name,
            // which fits a client's quote; the log has the whole path.
            Err(crate::transport::NotTaken::Held { lock, holder }) => {
                let held_by = crate::transport::held_by(&holder);
                note(&format!(
                    "{name}: refused seq {}: {} is held{held_by}",
                    envelope.seq,
                    lock.display()
                ));
                let file = lock.file_name().unwrap_or_default().to_string_lossy();
                return Reply::text(
                    409,
                    format!(
                        "a publish holds {file} in the server's store; if none is running there, \
                         remove it and publish again{held_by}"
                    ),
                );
            }
            Err(crate::transport::NotTaken::Failed(e)) => {
                note(&format!(
                    "{name}: could not store seq {}: {e}",
                    envelope.seq
                ));
                return Reply::text(500, "the server could not store the envelope");
            }
        };
    let pin = match admit(shared, &mut state, name, &envelope, body, local) {
        Ok(Some(pin)) => pin,
        Ok(None) => return Reply::empty(204),
        Err(reply) => return reply,
    };
    if !state.vaults.contains_key(name) {
        // A queue the store kept with no vault of that name (its file removed by hand) asked an
        // earlier vault: no request reaches a name the server does not hold, and the new vault
        // cannot tell the Macs that one let go from the rest. It goes before the vault is
        // stored, so a restart never reads it back beside the new one; each Mac still waiting
        // asks again within the hour.
        if let Err(e) = std::fs::remove_file(shared.joins_path(name))
            && e.kind() != std::io::ErrorKind::NotFound
        {
            note(&format!(
                "{name}: could not store seq {}: the join queue an earlier vault of that name \
                 left could not be removed: {e}",
                envelope.seq
            ));
            return Reply::text(500, "the server could not store the envelope");
        }
        if state
            .joins
            .remove(name)
            .is_some_and(|queue| !queue.is_empty())
        {
            note(&format!(
                "{name}: dropped the join queue an earlier vault of that name left"
            ));
        }
    }
    if let Err(e) = fsx::write_atomic(&shared.vault_path(name), body, 0o600) {
        note(&format!(
            "{name}: could not store seq {}: {e}",
            envelope.seq
        ));
        return Reply::text(500, "the server could not store the envelope");
    }
    let held = Held {
        seq: envelope.seq,
        signer: envelope.signer,
        head: envelope.digest(),
        parented: !envelope.parents.is_empty(),
        handoff: envelope.handoff.clone(),
        bytes: Arc::new(body.to_vec()),
    };
    let first = !state.vaults.contains_key(name);
    let slot = state.vaults.entry(name.to_owned()).or_insert_with(|| {
        Slot::new(Held {
            seq: 0,
            signer: envelope.signer,
            head: [0u8; 32],
            parented: false,
            handoff: Vec::new(),
            bytes: Arc::new(Vec::new()),
        })
    });
    slot.held = held;
    slot.wake.notify_all();
    shared.wake_advertiser();
    note(&format!(
        "{name}: accepted seq {} signed by {}{}",
        envelope.seq,
        envelope::fingerprint(&envelope.signer),
        match pin {
            envelope::Pin::Rotated(_) => " (a handoff from the pinned key)",
            envelope::Pin::Replaced(_) =>
                " (the pinned key gave way to the key this build names, and what it held with it)",
            envelope::Pin::Same if first => " (its first envelope here)",
            envelope::Pin::Same => "",
        }
    ));
    Reply::empty(204)
}

/// Judge a PUT of `envelope` (its bytes `body`) against what the server holds, after the store
/// file's newer version ([`take_from_store`]): the pin it is taken under, `None` when it is the
/// version held, or the refusal.
fn admit(
    shared: &Shared,
    state: &mut State,
    name: &str,
    envelope: &envelope::Envelope,
    body: &[u8],
    local: bool,
) -> Result<Option<envelope::Pin>, Reply> {
    take_from_store(shared, state, name);
    if let Some(slot) = state.vaults.get(name) {
        return takes_over(envelope, body, &slot.held, &shared.retired);
    }
    let mut pin = envelope::Pin::Same;
    if let Some(configured) = shared.pinned_key(name) {
        pin = envelope
            .check_pin_with(&configured, &[], &shared.retired)
            .map_err(|e| Reply::text(403, e.0))?;
    }
    match refuse_unheld(shared, state, name, local) {
        Some(reply) => Err(reply),
        None => Ok(Some(pin)),
    }
}

/// Whether `envelope` (its bytes `body`) takes over `held`: the rule a PUT and a version at the
/// store file keep alike. The pin it is taken under, `None` when it is `held`, or the refusal.
fn takes_over(
    envelope: &envelope::Envelope,
    body: &[u8],
    held: &Held,
    retired: &envelope::Retired,
) -> Result<Option<envelope::Pin>, Reply> {
    let followed: Vec<[u8; 32]> = held.handoff.iter().map(|(key, _)| *key).collect();
    let pin = envelope
        .check_pin_with(&held.signer, &followed, retired)
        .map_err(|e| Reply::text(403, e.0))?;
    if !matches!(pin, envelope::Pin::Replaced(_)) {
        if envelope.seq <= held.seq {
            if envelope.seq == held.seq && held.bytes.as_slice() == body {
                return Ok(None);
            }
            return Err(Reply::conflict_with_head(
                held,
                format!(
                    "seq {} is not newer than seq {}, which this server holds",
                    envelope.seq, held.seq
                ),
            ));
        }
        envelope
            .check_extends(&held.signer, &held.handoff)
            .map_err(|e| Reply::text(403, e.0))?;
        if (!envelope.parents.is_empty() || held.parented) && !envelope.parents.contains(&held.head)
        {
            return Err(Reply::conflict_with_head(
                held,
                format!(
                    "head {} at seq {} is not among the envelope's parents",
                    hex32_encode(&held.head),
                    held.seq
                ),
            ));
        }
    }
    Ok(Some(pin))
}

/// Hold a newer version an admin published at the vault's store file itself (an admin whose
/// vault is that path), checked at each read of the vault, each slice of a held long-poll and
/// each PUT. Without it the server serves what it read at launch, and the next publish built
/// on that overwrites the path's version, a revoke included. A version there is held only as a
/// PUT of it would be ([`takes_over`]); any other file changes nothing.
fn take_from_store(shared: &Shared, state: &mut State, name: &str) {
    let Some(slot) = state.vaults.get_mut(name) else {
        return;
    };
    let Ok(Some(bytes)) = fsx::read_owned(&shared.vault_path(name), MAX_ENVELOPE) else {
        return;
    };
    if bytes.as_slice() == slot.held.bytes.as_slice() {
        return;
    }
    let Ok(envelope) = envelope::parse(&bytes) else {
        return;
    };
    let publishable = envelope.name == name
        && envelope.verify_signature().is_ok()
        && envelope.check_signer(&shared.retired).is_ok()
        && matches!(
            takes_over(&envelope, &bytes, &slot.held, &shared.retired),
            Ok(Some(_))
        );
    if !publishable {
        return;
    }
    note(&format!(
        "{name}: holds seq {}, which an admin published at the store file",
        envelope.seq
    ));
    slot.held = Held {
        seq: envelope.seq,
        signer: envelope.signer,
        head: envelope.digest(),
        parented: slot.held.parented || !envelope.parents.is_empty(),
        handoff: envelope.handoff.clone(),
        bytes: Arc::new(bytes),
    };
    slot.wake.notify_all();
    shared.wake_advertiser();
}

/// The keys of the envelope this server holds for `name`: its signer and every key its handoff
/// chain names. An uncompiled record store ([`crate::membership_log::Store::under`]), ALab's
/// dormant one or another vault's, takes its genesis from one of them. The genesis outlives the
/// key that signed it: after a rotation the held envelope is the new key's, and a store that
/// followed its signer alone refused the vault's genesis at every restart and on every new
/// server.
fn held_keys(state: &State, name: &str) -> Option<Vec<[u8; 32]>> {
    state.vaults.get(name).map(|slot| {
        std::iter::once(slot.held.signer)
            .chain(slot.held.handoff.iter().map(|(key, _)| *key))
            .collect()
    })
}

/// [`held_keys`], else the key this server pins for `name`.
fn record_keys(shared: &Shared, state: &State, name: &str) -> Option<Vec<[u8; 32]>> {
    held_keys(state, name).or_else(|| shared.pinned_key(name).map(|key| vec![key]))
}

/// `store`, following `keys` ([`held_keys`]).
fn follow_held(
    store: crate::membership_log::Store,
    keys: Option<&[[u8; 32]]>,
) -> crate::membership_log::Store {
    match keys {
        Some(keys) => store.under(keys),
        None => store,
    }
}

/// [`crate::membership_log::Store::decode`] for `name` on this server ([`follow_held`],
/// [`kept_records`]).
fn decode_records_held(
    state: &State,
    name: &str,
    bytes: &[u8],
) -> crate::Result<crate::membership_log::Store> {
    Ok(kept_records(
        follow_held(
            crate::membership_log::Store::for_vault(name)?,
            held_keys(state, name).as_deref(),
        ),
        bytes,
    ))
}

/// What this server keeps of a records file's `bytes`, read into `empty`: every record that
/// verifies and that the store takes, each after its parents, as
/// [`crate::membership_log::Store::recovering`] reads them, so the records a POST appended
/// ([`post_records`]) in any order read back, and a file whose last append was cut short keeps
/// what came before it. A store with no genesis keeps none of its records ([`post_records`]),
/// so a file an earlier build let any LAN host fill reads as the empty store it would be now,
/// and the next write replaces it.
fn kept_records(empty: crate::membership_log::Store, bytes: &[u8]) -> crate::membership_log::Store {
    let decoded = empty.clone().recovering(bytes);
    if decoded.log().is_some() {
        decoded
    } else {
        empty
    }
}

/// Merge any verified records written directly at `<name>.records` on disk into memory.
fn take_records_from_store(shared: &Shared, state: &mut State, name: &str) {
    if vault_capacity_exceeded(state, name) {
        return;
    }
    let Ok(Some(bytes)) = fsx::read_owned(
        &shared.records_path(name),
        crate::membership_log::MAX_RECORDS_STREAM,
    ) else {
        return;
    };
    let keys = record_keys(shared, state, name);
    if let Some(slot) = state.records.get_mut(name) {
        if slot.raw == bytes {
            return;
        }
        let mut candidate = follow_held(slot.store.clone(), keys.as_deref());
        if let Ok(added) = candidate.ingest(&bytes) {
            slot.raw = bytes;
            // A store with no genesis keeps none of a file's records ([`kept_records`]).
            if added > 0 && candidate.log().is_some() {
                slot.store = candidate;
                slot.wake.notify_all();
            }
        }
        return;
    }
    let Ok(empty) = crate::membership_log::Store::for_vault(name) else {
        return;
    };
    // Kept even when it holds nothing: the slot remembers the bytes it read (`RecordSlot::raw`),
    // so an unchanged file is not decoded again.
    let kept = kept_records(follow_held(empty, keys.as_deref()), &bytes);
    state
        .records
        .insert(name.to_owned(), RecordSlot::new(kept, bytes));
}

fn have_query(query: Option<&str>) -> Result<Vec<crate::membership_log::RecordId>, Reply> {
    let Some(query) = query else {
        return Ok(Vec::new());
    };
    let Some(rest) = query.strip_prefix("have=") else {
        return Err(Reply::text(400, "the only query is have=<digest>,..."));
    };
    if rest.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for part in rest.split(',') {
        if out.len() >= crate::membership_log::MAX_RECORDS {
            return Err(Reply::text(400, "have= names too many digests"));
        }
        let mut digest = [0u8; 32];
        if !aterm_codec::hex::decode_into(part.as_bytes(), &mut digest) {
            return Err(Reply::text(
                400,
                "have= is not comma-separated 64-hex digests",
            ));
        }
        out.push(digest);
    }
    Ok(out)
}

fn records_reply(records: &[crate::membership_log::Record], records_need: bool) -> Reply {
    Reply {
        status: 200,
        body: Body::Records(crate::membership_log::encode_records(records)),
        etag: None,
        vault_head: None,
        allow: None,
        retry_after: None,
        records_need,
    }
}

fn server_lacks_have(state: &State, name: &str, have: &[crate::membership_log::RecordId]) -> bool {
    !have.is_empty()
        && !state
            .records
            .get(name)
            .and_then(|slot| slot.store.log())
            .is_some_and(|log| have.iter().all(|id| log.get(id).is_some()))
}

fn get_records(
    shared: &Shared,
    stream: &TcpStream,
    name: &str,
    query: Option<&str>,
    subscriber: Option<&str>,
    no_wait: bool,
    local: bool,
) -> Reply {
    let have = match have_query(query) {
        Ok(have) => have,
        Err(reply) => return reply,
    };
    let mut state = shared.lock_server_state();
    take_records_from_store(shared, &mut state, name);
    let known_vault = holds_records(&state, name)
        || state.vaults.contains_key(name)
        || name == crate::ALAB_VAULT
        || shared.pinned_key(name).is_some()
        || local;
    if !known_vault {
        return Reply::text(404, format!("no vault named {name} here"));
    }
    if let Some(subscriber) = subscriber
        && (holds_records(&state, name) || state.vaults.contains_key(name))
    {
        let now = (shared.tuning.now)();
        record_heard(shared, &mut state, name, subscriber, now);
    }
    if let Some(slot) = state.records.get(name) {
        let missing = slot.store.missing(&have);
        if !missing.is_empty() {
            return records_reply(&missing, server_lacks_have(&state, name, &have));
        }
    }
    if no_wait {
        let mut reply = Reply::empty(204);
        reply.records_need = server_lacks_have(&state, name, &have);
        return reply;
    }
    if !state.records.contains_key(name) {
        if vault_capacity_exceeded(&state, name) {
            return Reply::text(
                507,
                format!("this server holds its limit of {MAX_VAULTS} vaults"),
            );
        }
        let store = match crate::membership_log::Store::for_vault(name) {
            Ok(store) => store,
            Err(e) => return Reply::text(500, e.to_string()),
        };
        state
            .records
            .insert(name.to_owned(), RecordSlot::new(store, Vec::new()));
    }
    let Some(slot) = state.records.get(name) else {
        return Reply::empty(204);
    };
    let wake = Arc::clone(&slot.wake);
    let mut state = park_until_news(shared, stream, state, &wake, |state| {
        take_records_from_store(shared, state, name);
        state
            .records
            .get(name)
            .is_some_and(|slot| !slot.store.missing(&have).is_empty())
    });
    let lacks = server_lacks_have(&state, name, &have);
    let reply = match state
        .records
        .get(name)
        .map(|slot| slot.store.missing(&have))
    {
        Some(missing) if !missing.is_empty() => records_reply(&missing, lacks),
        _ => {
            let mut reply = Reply::empty(204);
            reply.records_need = lacks;
            reply
        }
    };
    drop(wake);
    // A slot that read a records file stays though it keeps nothing ([`kept_records`]): taken
    // out, the next look would decode the whole file again.
    if state.records.get(name).is_some_and(|slot| {
        slot.store.is_empty() && slot.raw.is_empty() && Arc::strong_count(&slot.wake) == 1
    }) {
        state.records.remove(name);
    }
    reply
}

/// How a records POST reached the file: its records on the end of it, or the file written whole.
enum RecordsWrite {
    Appended(Vec<u8>),
    Whole(Vec<u8>),
}

/// Take the records `body` brings into `name`'s store and its file. The store is tried on a copy,
/// which shares every record's bytes with the slot's ([`crate::membership_log::Record`]), so a
/// POST copies no record's bytes, however large the store. What a POST adds goes on the end of
/// the file as the slot last read or wrote it ([`fsx::append_at`]), and a reader orders it
/// ([`kept_records`]); the file is written whole when the store let a waiting record go to make
/// room, or the file holds other than the store's records or is not as the slot left it.
fn post_records(shared: &Shared, name: &str, body: &[u8], local: bool) -> Reply {
    if body.is_empty() {
        return Reply::text(400, "a POST to /records needs at least one record");
    }
    let _writing = shared
        .record_writes
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let (mut candidate, on_disk) = {
        let mut state = shared.lock_server_state();
        take_records_from_store(shared, &mut state, name);
        if let Some(reply) = refuse_unheld_records(shared, &state, name, local) {
            return reply;
        }
        let keys = record_keys(shared, &state, name);
        match state.records.get(name) {
            Some(slot) => (
                follow_held(slot.store.clone(), keys.as_deref()),
                slot.raw.len(),
            ),
            None => match crate::membership_log::Store::for_vault(name) {
                Ok(store) => (follow_held(store, keys.as_deref()), 0),
                Err(e) => return Reply::text(500, e.to_string()),
            },
        }
    };
    let before = candidate.inventory();
    let before_bytes: usize = before
        .iter()
        .filter_map(|id| candidate.get_bytes(id))
        .map(<[u8]>::len)
        .sum();
    let added = match candidate.ingest(body) {
        Ok(added) => added,
        Err(e) => return Reply::text(400, e.to_string()),
    };
    if added == 0 {
        return Reply::empty(204);
    }
    // A store with no genesis keeps nothing but the genesis that roots it, with whatever
    // comes beside it: no member is ever served a record before a genesis
    // (`Store::missing`), so what such a store took could be anyone's (any LAN host may POST
    // to ALab's dormant store), and the server would hold and rewrite it to its bound. Such a
    // server pulls its peers' log from the genesis (its store's `have` is empty), and a member
    // that hears it lacks its frontier (`server_lacks_have`) posts it the whole store when it
    // has no post to it on record, or `REPOST_AGAIN` after its last whole one
    // (`client::Reposted`); a post of only its new records gets this 422.
    if candidate.log().is_none() {
        return Reply::text(
            422,
            "a store with no genesis takes records only with the genesis that roots them",
        );
    }
    let path = shared.records_path(name);
    let after = candidate.inventory();
    // Onto a file that holds exactly the store's records (not one an earlier build filled with
    // records it no longer keeps), and only when none of them went.
    let tail =
        (before_bytes > 0 && on_disk == before_bytes && before.is_subset(&after)).then(|| {
            after
                .difference(&before)
                .filter_map(|id| candidate.get_bytes(id))
                .flatten()
                .copied()
                .collect::<Vec<u8>>()
        });
    let written = match tail {
        Some(tail)
            if fsx::append_at(&path, u64::try_from(on_disk).unwrap_or(u64::MAX), &tail).is_ok() =>
        {
            RecordsWrite::Appended(tail)
        }
        _ => {
            let raw = candidate.encode();
            if let Err(e) = fsx::write_atomic(&path, &raw, 0o600) {
                note(&format!("{name}: could not store records: {e}"));
                return Reply::text(500, "the server could not write to its store");
            }
            RecordsWrite::Whole(raw)
        }
    };
    let mut state = shared.lock_server_state();
    match state.records.get_mut(name) {
        Some(slot) => {
            slot.store = candidate;
            match written {
                // A look between the append and here may have read the file already.
                RecordsWrite::Appended(tail) => {
                    slot.raw.truncate(on_disk);
                    slot.raw.extend_from_slice(&tail);
                }
                RecordsWrite::Whole(raw) => slot.raw = raw,
            }
            slot.wake.notify_all();
        }
        None => {
            // The slot went since its copy: the next look reads the file this POST wrote.
            let raw = match written {
                RecordsWrite::Appended(_) => Vec::new(),
                RecordsWrite::Whole(raw) => raw,
            };
            state
                .records
                .insert(name.to_owned(), RecordSlot::new(candidate, raw));
        }
    }
    Reply::empty(204)
}

fn post_join(shared: &Shared, name: &str, body: &[u8], peer: IpAddr) -> Reply {
    let Ok(text) = std::str::from_utf8(body) else {
        return Reply::text(400, "a join request is one line of text");
    };
    let line = text.strip_suffix('\n').unwrap_or(text);
    if line.contains(['\n', '\r']) {
        return Reply::text(400, "a join request is one line: `age1… <host> <code>`");
    }
    let request = match parse_join_line(line) {
        Ok(request) if request.time.is_none() && !request.lan => request,
        Ok(_) => return Reply::text(400, "a join request is `age1… <host> <code>`"),
        Err(e) => return Reply::text(400, e.0),
    };
    let code = crate::age::Recipient::parse(&request.recipient)
        .ok()
        .map(|key| crate::client::join_code(&key));
    if code.as_deref() != Some(request.code.as_str()) {
        return Reply::text(400, "a join request's code is not its key's");
    }
    let now = (shared.tuning.now)();
    let _writing = shared.join_writes();
    let (queue, noted) = match decide_join(shared, name, request, peer, now) {
        Ok(Decided::Next(queue, noted)) => (queue, noted),
        Ok(Decided::Queued) => return Reply::empty(204),
        Err(reply) => return reply,
    };
    if let Err(e) = fsx::write_atomic(
        &shared.joins_path(name),
        stored_lines(&queue).as_bytes(),
        0o600,
    ) {
        note(&format!("{name}: could not store a join request: {e}"));
        return Reply::text(500, "the server could not store the request");
    }
    let mut state = shared.lock_server_state();
    let queue = keep_verdicts(queue, state.joins.get(name).map(Vec::as_slice));
    state.joins.insert(name.to_owned(), queue);
    if let Some(slot) = state.vaults.get_mut(name) {
        slot.joins += 1;
        slot.wake.notify_all();
    }
    drop(state);
    if let Some(line) = noted {
        note(&format!("{name}: {line}"));
    }
    Reply::empty(204)
}

/// `queue`, decided from a copy of the queue held then, with each verdict the held queue took
/// since: [`post_verdict`] takes only `server_state`, so one can land while a join's store is
/// written. A verdict carries over only to the request of the same key and proof.
fn keep_verdicts(mut queue: Vec<Queued>, held: Option<&[Queued]>) -> Vec<Queued> {
    for queued in queue.iter_mut().filter(|queued| queued.refused.is_none()) {
        let since = held.into_iter().flatten().find(|now| {
            now.request.recipient == queued.request.recipient
                && now.request.proof == queued.request.proof
        });
        if let Some(now) = since {
            queued.refused.clone_from(&now.refused);
        }
    }
    queue
}

/// What [`decide_join`] decided.
enum Decided {
    /// The request is queued as it is already ([`Queued::asked`]).
    Queued,
    /// The queue to store, and the line to log.
    Next(Vec<Queued>, Option<String>),
}

/// Decide a join request from `peer` under `server_state`, or refuse it ([`Shared::join_writes`]).
fn decide_join(
    shared: &Shared,
    name: &str,
    request: JoinRequest,
    peer: IpAddr,
    now: u64,
) -> Result<Decided, Reply> {
    let mut state = shared.lock_server_state();
    let Some(seq) = state.vaults.get(name).map(|slot| slot.held.seq) else {
        return Err(Reply::text(404, format!("no vault named {name} here")));
    };
    let mut queue = state.joins.get(name).cloned().unwrap_or_default();
    expire(&mut queue, now);
    let same = |queued: &Queued| {
        let asked = &queued.request;
        asked.recipient == request.recipient
            && asked.host == request.host
            && asked.code == request.code
            && asked.allowed == request.allowed
            && asked.proof == request.proof
    };
    if let Some(at) = queue.iter().position(same) {
        // Preserve the first address for queue accounting, but let a machine that has
        // moved onto the LAN become eligible without a changed request or a manual Allow.
        if lan_join_peer(peer) && !queue[at].observed_lan {
            queue[at].observed_lan = true;
            queue[at].asked = seq;
            if now.saturating_sub(queue[at].request.time.unwrap_or(0)) >= JOIN_REFRESH_EVERY_SECS {
                queue[at].request.time = Some(now);
            }
            return Ok(Decided::Next(queue, None));
        }
        if now.saturating_sub(queue[at].request.time.unwrap_or(0)) < JOIN_REFRESH_EVERY_SECS {
            if let Some(held) = state
                .joins
                .get_mut(name)
                .and_then(|queue| queue.iter_mut().find(|queued| same(queued)))
            {
                held.asked = seq;
            }
            return Ok(Decided::Queued);
        }
        queue[at].request.time = Some(now);
        queue[at].asked = seq;
        return Ok(Decided::Next(queue, None));
    }
    let ours = |queued: &Queued| {
        queued.request.recipient == request.recipient && queued.peer.is_none_or(|at| at == peer)
    };
    let previous = queue.iter().position(ours).map(|at| queue.remove(at));
    if let Some(previous) = &previous {
        let gap = shared.tuning.join_change_gap;
        if let Some(since) = now
            .checked_sub(previous.changed)
            .filter(|since| *since < gap.as_secs())
        {
            // The seconds left of the gap: a client that waits them is taken (its person typed
            // the join password again), where the queue's caps name no wait.
            let mut reply = Reply::text(
                429,
                format!("this key's join request changed less than {gap:?} ago; ask again shortly"),
            );
            reply.retry_after = Some(gap.as_secs() - since);
            return Err(reply);
        }
    }
    if !make_room(
        &mut queue,
        MAX_PEER_JOINS,
        |queued| queued.peer == Some(peer),
        seq,
    ) {
        return Err(Reply::text(
            429,
            format!("your address already has {MAX_PEER_JOINS} join requests queued here"),
        ));
    }
    if !make_room(&mut queue, MAX_JOINS, |_| true, seq) {
        return Err(Reply::text(
            429,
            format!("the join queue already holds {MAX_JOINS} requests"),
        ));
    }
    let host = &request.host;
    let (noted, line) = match &previous {
        None => (now, Some(format!("join request queued for host {host}"))),
        Some(previous) if now.saturating_sub(previous.noted) >= JOIN_NOTE_EVERY_SECS => (
            now,
            Some(format!(
                "join request changed for host {host} (changes to one are noted at most hourly)"
            )),
        ),
        Some(previous) => (previous.noted, None),
    };
    queue.push(Queued {
        request: JoinRequest {
            time: Some(now),
            ..request
        },
        peer: Some(peer),
        observed_lan: lan_join_peer(peer) || previous.is_some_and(|old| old.observed_lan),
        asked: seq,
        changed: now,
        noted,
        refused: None,
    });
    Ok(Decided::Next(queue, line))
}

/// Make room in `queue` for one more request among those `counted` picks, once they number `cap`.
fn make_room(
    queue: &mut Vec<Queued>,
    cap: usize,
    counted: impl Fn(&Queued) -> bool,
    seq: u64,
) -> bool {
    if queue.iter().filter(|queued| counted(queued)).count() < cap {
        return true;
    }
    match queue
        .iter()
        .position(|queued| counted(queued) && queued.asked != seq)
    {
        Some(at) => {
            queue.remove(at);
            true
        }
        None => false,
    }
}

/// `GET /v1/<name>/join[?after=<n>]`: the queued requests, one line each, with the queue's
/// [`Slot::joins`] as the `ETag`. With `after=<n>` while the queue is still at `n`, held until
/// a request is queued or changed or the vault takes a new version (`200`, the lines), or the
/// hold runs out (`204`): an admin Mac's approver hears a new request at once, with one
/// request per hold while none comes.
fn list_joins(shared: &Shared, stream: &TcpStream, name: &str, query: Option<&str>) -> Reply {
    let after = match after_query(query) {
        Ok(after) => after,
        Err(reply) => return reply,
    };
    let mut state = shared.lock_server_state();
    let now = (shared.tuning.now)();
    let State { vaults, joins, .. } = &mut *state;
    let Some(slot) = vaults.get_mut(name) else {
        return Reply::text(404, format!("no vault named {name} here"));
    };
    // A request that expired since the last listing moves the queue, so an approver that holds
    // that listing reads it again rather than judging a request the queue dropped.
    if let Some(queue) = joins.get_mut(name) {
        let held = queue.len();
        expire(queue, now);
        if queue.len() != held {
            slot.joins += 1;
            slot.wake.notify_all();
        }
    }
    if let Some(after) = after.filter(|after| *after == slot.joins) {
        let (wake, seq) = (Arc::clone(&slot.wake), slot.held.seq);
        state = park_until_news(shared, stream, state, &wake, |state| {
            state
                .vaults
                .get(name)
                .is_none_or(|slot| slot.joins != after || slot.held.seq != seq)
        });
        if let Some(slot) = state
            .vaults
            .get(name)
            .filter(|slot| slot.joins == after && slot.held.seq == seq)
        {
            let mut reply = Reply::empty(204);
            reply.etag = Some(slot.joins);
            return reply;
        }
    }
    let generation = state.vaults.get(name).map(|slot| slot.joins);
    let queue = state.joins.entry(name.to_owned()).or_default();
    Reply {
        status: 200,
        body: Body::Text(join_lines(queue)),
        etag: generation,
        vault_head: None,
        allow: None,
        retry_after: None,
        records_need: false,
    }
}

/// `POST /v1/<name>/join/verdict`: an admin Mac's `age1… refused <tag>` on the request of that
/// key whose proof `tag` names ([`crate::join::proof_tag`]). Unauthenticated, it can only make
/// that Mac ask its person for the join password again: it is kept beside that request alone,
/// in memory, until the request changes, and a new one ends the vault's long-polls.
fn post_verdict(shared: &Shared, name: &str, body: &[u8]) -> Reply {
    let line = std::str::from_utf8(body)
        .ok()
        .map(|text| text.strip_suffix('\n').unwrap_or(text));
    let Some((recipient, tag)) = line.and_then(crate::transport::parse_verdict_line) else {
        return Reply::text(400, "a verdict is one line: `age1… refused <tag>`");
    };
    let mut state = shared.lock_server_state();
    let State { vaults, joins, .. } = &mut *state;
    let Some(slot) = vaults.get_mut(name) else {
        return Reply::text(404, format!("no vault named {name} here"));
    };
    let Some(queued) = joins.get_mut(name).and_then(|queue| {
        queue.iter_mut().find(|queued| {
            queued.request.recipient == recipient
                && queued
                    .request
                    .proof
                    .as_deref()
                    .is_some_and(|proof| crate::join::proof_tag(proof) == tag)
        })
    }) else {
        return Reply::text(404, "no join request of that key carries that proof");
    };
    if queued.refused.as_deref() != Some(tag.as_str()) {
        queued.refused = Some(tag);
        slot.verdicts += 1;
        slot.wake.notify_all();
    }
    Reply::empty(204)
}

/// `GET /v1/<name>/join/verdict`: one `age1… refused <tag>` line per request a verdict refused.
fn list_verdicts(shared: &Shared, name: &str) -> Reply {
    let state = shared.lock_server_state();
    if !state.vaults.contains_key(name) {
        return Reply::text(404, format!("no vault named {name} here"));
    }
    let mut text = String::new();
    for queued in state.joins.get(name).into_iter().flatten() {
        if let Some(tag) = &queued.refused {
            text.push_str(&format!("{} refused {tag}\n", queued.request.recipient));
        }
    }
    Reply {
        status: 200,
        body: Body::Text(text),
        etag: None,
        vault_head: None,
        allow: None,
        retry_after: None,
        records_need: false,
    }
}

/// What a server's `_alab-vault._tcp` TXT record says of the vault it advertises: hints for
/// choosing which server to ask first, never a trust grant (envelopes still verify against the
/// pinned signer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultTxt {
    /// Vault name (`[a-z0-9][a-z0-9-]{0,62}`).
    pub vault: String,
    /// Signer fingerprint (`xxxx-xxxx-xxxx-xxxx-xxxx` lowercase hex, [`envelope::fingerprint`]).
    pub signer: String,
    /// Held sequence number (`>= 1`).
    pub seq: u64,
    /// SHA-256 digest of the held envelope bytes ([`envelope::Envelope::digest`]).
    pub head: [u8; 32],
    /// Optional canonical label of the server advertising this instance (`label=<mN>`, F7).
    pub label: Option<String>,
}

impl VaultTxt {
    /// The `key=value` TXT attributes registered after `path=/v1`.
    #[must_use]
    pub fn txt_pairs(&self) -> Vec<String> {
        let mut pairs = vec![
            format!("vault={}", self.vault),
            format!("signer={}", self.signer),
            format!("seq={}", self.seq),
            format!("head={}", hex32_encode(&self.head)),
        ];
        if let Some(label) = &self.label {
            pairs.push(format!("label={label}"));
        }
        pairs
    }

    /// Parse a `_alab-vault._tcp` TXT record's `key=value` strings (RFC 6763 §6.4: keys are
    /// ASCII case-insensitive and the first occurrence of a key wins). Requires `path=/v1` and a
    /// well-formed `vault`, `signer`, `seq` (`>= 1`) and `head`; an older server's `path=/v1`
    /// alone yields `None`.
    pub fn from_txt<'a>(pairs: impl IntoIterator<Item = &'a str>) -> Option<Self> {
        let mut path = None;
        let mut vault = None;
        let mut signer = None;
        let mut seq = None;
        let mut head = None;
        let mut label = None;
        for pair in pairs {
            let Some((raw_key, value)) = pair.split_once('=') else {
                continue;
            };
            if raw_key.eq_ignore_ascii_case("path") {
                path.get_or_insert(value);
            } else if raw_key.eq_ignore_ascii_case("vault") {
                vault.get_or_insert(value);
            } else if raw_key.eq_ignore_ascii_case("signer") {
                signer.get_or_insert(value);
            } else if raw_key.eq_ignore_ascii_case("seq") {
                seq.get_or_insert(value);
            } else if raw_key.eq_ignore_ascii_case("head") {
                head.get_or_insert(value);
            } else if raw_key.eq_ignore_ascii_case("label") {
                label.get_or_insert(value);
            }
        }
        if path != Some("/v1") {
            return None;
        }
        let vault = vault.filter(|name| envelope::valid_name(name))?;
        let signer = signer.filter(|fp| valid_fingerprint(fp))?;
        let seq = parse_positive_seq(seq?)?;
        let head = hex32_decode(head?)?;
        let label = label
            .filter(|name| {
                *name != crate::alab_advertised() && crate::text::valid_machine_name(name)
            })
            .map(str::to_owned);
        Some(Self {
            vault: vault.to_owned(),
            signer: signer.to_owned(),
            seq,
            head,
            label,
        })
    }

    /// Parse a `dns-sd -L` TXT line (whitespace-separated `key=value` attributes).
    #[must_use]
    pub fn from_resolve_line(line: &str) -> Option<Self> {
        Self::from_txt(line.split_whitespace())
    }
}

fn valid_fingerprint(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 24
        && bytes.iter().enumerate().all(|(idx, &b)| {
            if matches!(idx, 4 | 9 | 14 | 19) {
                b == b'-'
            } else {
                matches!(b, b'0'..=b'9' | b'a'..=b'f')
            }
        })
}

fn parse_positive_seq(text: &str) -> Option<u64> {
    if text.is_empty() || text.starts_with('0') || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<u64>().ok().filter(|&n| n >= 1)
}

fn hex32_encode(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for &byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn hex32_decode(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let nibble = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    let bytes = text.as_bytes();
    let mut out = [0u8; 32];
    for (idx, slot) in out.iter_mut().enumerate() {
        let hi = nibble(bytes[idx * 2])?;
        let lo = nibble(bytes[idx * 2 + 1])?;
        *slot = (hi << 4) | lo;
    }
    Some(out)
}

/// Which held vault's TXT record an advertisement named `advertised` carries (`None` while the
/// store holds none or holds several unrelated vaults none of which `advertised` or a single pin
/// singles out).
fn advertised_vault(
    state: &State,
    advertised: &str,
    own_label: Option<&str>,
    pins: &[(String, [u8; 32])],
) -> Option<VaultTxt> {
    let chosen = if advertised == crate::alab_advertised() && state.vaults.contains_key(ALAB_VAULT)
    {
        Some(ALAB_VAULT)
    } else if state.vaults.contains_key(advertised) {
        Some(advertised)
    } else if state.vaults.contains_key(ALAB_VAULT) {
        Some(ALAB_VAULT)
    } else if state.vaults.len() == 1 {
        state.vaults.keys().next().map(String::as_str)
    } else if let [(pinned, _)] = pins
        && state.vaults.contains_key(pinned.as_str())
    {
        Some(pinned.as_str())
    } else {
        None
    }?;
    let held = &state.vaults.get(chosen)?.held;
    let label = own_label
        .filter(|name| *name != crate::alab_advertised() && crate::text::valid_machine_name(name))
        .map(str::to_owned);
    (held.seq >= 1).then(|| VaultTxt {
        vault: chosen.to_owned(),
        signer: envelope::fingerprint(&held.signer),
        seq: held.seq,
        head: held.head,
        label,
    })
}

/// Arguments to `dns-sd` registering `<name>.local` at `ip` and the vault service on `port`, with
/// `txt`'s vault name, signer fingerprint, `seq` and `head` digest beside `path=/v1` once a vault
/// is held (`path=/v1` alone while the store holds none).
pub fn dns_sd_args(name: &str, port: u16, ip: Ipv4Addr, txt: Option<&VaultTxt>) -> Vec<String> {
    let mut args = vec![
        "-P".to_owned(),
        name.to_owned(),
        SERVICE_TYPE.to_owned(),
        "local".to_owned(),
        port.to_string(),
        format!("{name}.local"),
        ip.to_string(),
        "path=/v1".to_owned(),
    ];
    if let Some(txt) = txt {
        args.extend(txt.txt_pairs());
    }
    args
}

/// Where one `dns-sd -P` registration is made: the address it names and, on a Mac on several
/// networks at once, the interface it is scoped to ([`advert_targets`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The interface (`en0`) the registration is made on alone, or `None` for every interface.
    pub scope: Option<String>,
    /// The address `<name>.local` names.
    pub ip: Ipv4Addr,
}

/// [`dns_sd_args`] for a registration at `target`: `-i <interface>` first where it is scoped.
pub fn registration_args(
    target: &Target,
    name: &str,
    port: u16,
    txt: Option<&VaultTxt>,
) -> Vec<String> {
    let mut args = match &target.scope {
        Some(interface) => vec!["-i".to_owned(), interface.clone()],
        None => Vec::new(),
    };
    args.extend(dns_sd_args(name, port, target.ip, txt));
    args
}

/// Where this Mac advertises its vault now ([`advert_targets`] over its interfaces and
/// [`lan_ipv4`]).
fn lan_targets() -> Vec<Target> {
    advert_targets(
        &crate::network::interfaces().unwrap_or_default(),
        lan_ipv4(),
    )
}

/// Where a Mac with `interfaces`, whose route to mDNS's group leaves from `routed`, advertises
/// its vault: one registration per network it is on, each at an address its peers on that
/// network reach.
///
/// - **One network** (the usual case): one registration on every interface at `routed`, else at
///   that network's address.
/// - **Several networks:** one registration per network, scoped to one of its interfaces and
///   naming that interface's own address. `routed`'s interface goes first, and serves its own
///   network. An example is a Mac on a phone hotspot over Wi-Fi and on the LAN over a wire, as
///   m22 was when it came back on 2026-10-28. Each link then hears an address valid on it,
///   whichever link the default route takes. One address named on every link sends one side to
///   an address it cannot reach.
///
/// A network is the IPv4 subnet of an interface that is up and running and can broadcast and
/// multicast, so no tunnel or loopback counts. Link-local addresses are left out. An interface
/// counts once: an interface holding addresses on several subnets is counted at its lowest
/// network's address.
#[must_use]
pub fn advert_targets(
    interfaces: &[crate::network::InterfaceAddress],
    routed: Option<Ipv4Addr>,
) -> Vec<Target> {
    const WANTED: u32 =
        (libc::IFF_UP | libc::IFF_RUNNING | libc::IFF_BROADCAST | libc::IFF_MULTICAST) as u32;
    const REFUSED: u32 = (libc::IFF_LOOPBACK | libc::IFF_POINTOPOINT) as u32;
    let mut lan: Vec<(Ipv4Addr, Ipv4Addr, &str)> = interfaces
        .iter()
        .filter(|at| at.flags & WANTED == WANTED && at.flags & REFUSED == 0)
        .filter_map(|at| match (at.address, at.netmask) {
            (IpAddr::V4(ip), Some(IpAddr::V4(mask)))
                if !ip.is_link_local() && !ip.is_loopback() && !ip.is_unspecified() =>
            {
                Some((ip & mask, ip, at.name.as_str()))
            }
            _ => None,
        })
        .collect();
    // `routed`'s own address first, then by network, address and name: the one each network
    // keeps is `routed` on its own network, else its lowest address.
    lan.sort_by_key(|&(network, ip, name)| (Some(ip) != routed, network, ip, name));
    let mut chosen: Vec<(Ipv4Addr, Target)> = Vec::new();
    for (network, ip, name) in lan {
        let taken = chosen
            .iter()
            .any(|(held, target)| *held == network || target.scope.as_deref() == Some(name));
        if !taken {
            chosen.push((
                network,
                Target {
                    scope: Some(name.to_owned()),
                    ip,
                },
            ));
        }
    }
    if chosen.len() < 2 {
        return routed
            .or_else(|| chosen.first().map(|(_, target)| target.ip))
            .map(|ip| Target { scope: None, ip })
            .into_iter()
            .collect();
    }
    chosen.into_iter().map(|(_, target)| target).collect()
}

/// The argv (after the program) that keeps the Mac from idle sleep while `pid` lives.
pub fn caffeinate_args(pid: u32) -> Vec<String> {
    vec!["-i".to_owned(), "-w".to_owned(), pid.to_string()]
}

/// The addresses this Mac's traffic leaves by: the default route's over IPv4, the LAN's and the
/// default route's over IPv6, each `None` without a route.
pub fn route_sources() -> [Option<IpAddr>; 3] {
    [
        primary_ipv4().map(IpAddr::V4),
        lan_ipv4().map(IpAddr::V4),
        primary_ipv6().map(IpAddr::V6),
    ]
}

/// Primary IPv4 address of the default-route interface.
pub fn primary_ipv4() -> Option<Ipv4Addr> {
    source_ipv4(Ipv4Addr::new(192, 0, 2, 1), 9)
}

/// LAN IPv4 address at which `<name>.local` is advertised ([`primary_ipv4`]).
///
/// On macOS it is read from the route to mDNS's group in the routing table
/// ([`aterm_uds::network::route_source_ipv4`]), never by a UDP connect to the group: that connect
/// is a use of the local network, which puts macOS's Local Network question on screen before
/// System State's step asks it (every converge pass and vault subscriber reads these routes).
fn lan_ipv4() -> Option<Ipv4Addr> {
    #[cfg(target_os = "macos")]
    let routed = aterm_uds::network::route_source_ipv4(MDNS_GROUP);
    #[cfg(not(target_os = "macos"))]
    let routed = source_ipv4(MDNS_GROUP, 5353);
    routed.or_else(primary_ipv4)
}

/// mDNS's IPv4 multicast group, whose route names the LAN `<name>.local` is advertised on.
const MDNS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);

/// The local address a UDP socket connected to `to` gets (no packet is sent).
fn source_ipv4(to: Ipv4Addr, port: u16) -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((to, port)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() && !ip.is_loopback() => Some(ip),
        _ => None,
    }
}

/// The IPv6 address the default route leaves by, found as [`primary_ipv4`] finds its own.
fn primary_ipv6() -> Option<Ipv6Addr> {
    let socket = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).ok()?;
    socket
        .connect((Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1), 9))
        .ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V6(ip) if !ip.is_unspecified() && !ip.is_loopback() => Some(ip),
        _ => None,
    }
}

/// The children [`serve`] holds, killed when it is dropped.
#[derive(Default)]
struct Children {
    set: Arc<Mutex<ChildSet>>,
    /// The bell the `vault-tend` thread naps on, rung as this is dropped.
    bell: Mutex<Option<Arc<Bell>>>,
}

/// The `dns-sd -P` registrations of one name ([`reregister_slot`]).
#[derive(Default)]
struct DnsSdSlot {
    /// Each registration running: where, its TXT, and the `dns-sd -P` that holds it.
    held: Vec<(Target, Option<VaultTxt>, aterm_uds::spawnfd::Child)>,
    /// The instance label currently held by [`Self::held`].
    held_name: Option<String>,
    /// The addresses last logged as advertised, so TXT refreshes on new seqs stay quiet.
    logged: Vec<Ipv4Addr>,
    /// A registration ended by itself (`dns-sd -P` gives up a name another device on the
    /// network answers to), so the log said so, until every one stays up.
    dns_sd_lost: bool,
    /// What woke the round that last started a registration (`None`: the first round, as the
    /// server started), so a test can tell which source moved the advert.
    started_by: Option<Option<Wake>>,
}

#[derive(Default)]
struct ChildSet {
    closed: bool,
    caffeinate: Option<aterm_uds::spawnfd::Child>,
    /// The last start of [`CAFFEINATE`] failed, so the log said so ([`tend_awake`]).
    awake_failed: bool,
    /// The registration of `--advertise`'s own name.
    dns: DnsSdSlot,
    /// Secondary lease-followed registration for `alab-secrets.local` (§8.3).
    alab_dns: DnsSdSlot,
    /// The log said a held `alab-secrets` registration lost the name ([`reregister_roles`]),
    /// so it says so again only after a later one stays up for a round.
    alab_lost_said: bool,
    /// A will-sleep took this server's registrations ([`take_for_sleep`]), so the next round
    /// lets the `alab-secrets` name go before anything is registered again
    /// ([`reregister_awake`]).
    slept: bool,
}

fn quiet(program: &str, args: &[String]) -> io::Result<aterm_uds::spawnfd::Child> {
    use aterm_uds::spawnfd::Stdio;
    aterm_uds::spawnfd::command(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

fn end(child: &mut aterm_uds::spawnfd::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Budget for resolving `_alab-vault._tcp` instances heard on the standing browse.
const PEER_RESOLVE_BUDGET: Duration = Duration::from_millis(750);
/// Budget for an immediate `?after=<seq>` pull from a `>= 0.146` peer (`X-Aterm-No-Wait: 1`).
const PEER_PULL_BUDGET: Duration = Duration::from_secs(2);
/// Budget for a pull from a `0.145` peer (`txt == None`), which ignores `X-Aterm-No-Wait` and
/// parks when it holds nothing newer.
const LEGACY_PEER_PULL_BUDGET: Duration = Duration::from_millis(750);

fn is_self_addr(addr: SocketAddr, own_port: u16, here: &[Target]) -> bool {
    addr.port() == own_port
        && (addr.ip().is_loopback() || here.iter().any(|t| IpAddr::V4(t.ip) == addr.ip()))
}

fn is_obviously_self_url(
    uri: &aterm_http::Uri,
    own_port: u16,
    own_label: Option<&str>,
    here: &[Target],
) -> bool {
    let Some(port) = uri
        .authority()
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse::<u16>().ok())
    else {
        return false;
    };
    if port != own_port {
        return false;
    }
    if let Ok(ip) = uri.host().parse::<IpAddr>() {
        return ip.is_loopback() || here.iter().any(|t| IpAddr::V4(t.ip) == ip);
    }
    if let Some(label) = own_label {
        let host = uri.host().strip_suffix(".local").unwrap_or(uri.host());
        if host.eq_ignore_ascii_case(label) {
            return true;
        }
    }
    false
}

/// How many peers' records a server reads at once ([`pull_from_peer`]): each body is up to
/// [`crate::membership_log::MAX_RECORDS_STREAM`], and is decoded again where [`post_records`]
/// stores it, one at a time. A pass asks all its peers (up to
/// [`crate::discover::MAX_CANDIDATES`]) for their envelopes at once, which finds the dark ones
/// in one budget, and holds this many records bodies: one read while another is stored.
const RECORDS_PULLS_AT_ONCE: usize = 2;

/// Slots for [`RECORDS_PULLS_AT_ONCE`] records pulls.
#[derive(Default)]
struct PullSlots {
    taken: Mutex<usize>,
    freed: Condvar,
    /// Pulls waiting for a slot now.
    #[cfg(test)]
    waiting: AtomicUsize,
}

/// One records pull's slot, given back when dropped.
struct PullSlot<'a>(&'a PullSlots);

impl PullSlots {
    /// A slot, once one is free: a slot's pull ends within its budget.
    fn take(&self) -> PullSlot<'_> {
        let mut taken = self.taken.lock().unwrap_or_else(PoisonError::into_inner);
        #[cfg(test)]
        let queued = usize::from(*taken >= RECORDS_PULLS_AT_ONCE);
        #[cfg(test)]
        self.waiting.fetch_add(queued, Ordering::SeqCst);
        while *taken >= RECORDS_PULLS_AT_ONCE {
            taken = self
                .freed
                .wait(taken)
                .unwrap_or_else(PoisonError::into_inner);
        }
        #[cfg(test)]
        self.waiting.fetch_sub(queued, Ordering::SeqCst);
        *taken += 1;
        PullSlot(self)
    }
}

impl Drop for PullSlot<'_> {
    fn drop(&mut self) {
        let mut taken = self.0.taken.lock().unwrap_or_else(PoisonError::into_inner);
        *taken = taken.saturating_sub(1);
        self.0.freed.notify_one();
    }
}

/// Pull from each of `peers` on a thread of its own ([`pull_from_peer`]): a peer that takes a
/// pull and never answers (a Mac asleep mid-answer, a server that stalls) holds only its own
/// pull, not the pulls of every peer after it on the vault-tend thread. Their records bodies
/// are read [`RECORDS_PULLS_AT_ONCE`] at a time. Returns how many vaults advanced.
fn pull_from_peers_shared(
    shared: &Shared,
    peers: &[crate::discover::DiscoveredVault],
    own_port: u16,
    own_label: Option<&str>,
    here: &[Target],
) -> usize {
    let pull = |peer: &crate::discover::DiscoveredVault| {
        pull_from_peer(shared, peer, own_port, own_label, here)
    };
    std::thread::scope(|scope| {
        let pulls: Vec<_> = peers
            .iter()
            .map(|peer| {
                std::thread::Builder::new()
                    .name("vault-peer-pull".to_owned())
                    .spawn_scoped(scope, move || pull(peer))
            })
            .collect();
        peers
            .iter()
            .zip(pulls)
            .map(|(peer, handle)| match handle {
                Ok(handle) => handle.join().unwrap_or(0),
                // No thread to spare: this one pulls once the others are under way.
                Err(_) => pull(peer),
            })
            .sum()
    })
}

/// Pull newer envelopes and missing records of each vault `peer` serves (`?after=<held seq>`,
/// `?have=<frontier>`), storing what this server's admission rules accept. A pull that fails
/// (no answer within its budget, a refused or dropped connection, an answer no server gives)
/// ends this pass's pulls from `peer`: one more would cost the same budget to hear the same
/// silence. Returns how many vaults advanced.
fn pull_from_peer(
    shared: &Shared,
    peer: &crate::discover::DiscoveredVault,
    own_port: u16,
    own_label: Option<&str>,
    here: &[Target],
) -> usize {
    let mut stored = 0usize;
    let Some(uri) = aterm_http::Uri::parse(&peer.url) else {
        return stored;
    };
    if is_obviously_self_url(&uri, own_port, own_label, here) {
        return stored;
    }
    let vault_names: Vec<String> = match &peer.txt {
        Some(txt) => vec![txt.vault.clone()],
        None => {
            let mut names: Vec<String> = shared
                .pins_snapshot()
                .into_iter()
                .map(|(name, _)| name)
                .collect();
            {
                let state = shared.lock_server_state();
                for name in state.vaults.keys().chain(
                    state
                        .records
                        .iter()
                        .filter(|(_, slot)| !slot.store.is_empty())
                        .map(|(name, _)| name),
                ) {
                    if !names.contains(name) {
                        names.push(name.clone());
                    }
                }
            }
            names
        }
    };
    let Some((base_prefix, _)) = peer.url.rsplit_once('/') else {
        return stored;
    };
    let budget = if peer.txt.is_some() {
        shared.tuning.peer_pull
    } else {
        LEGACY_PEER_PULL_BUDGET
    };
    for vault_name in vault_names {
        let vault_url = format!("{base_prefix}/{vault_name}");
        let (held_seq, held_head, accepted_envelope, accepted_records) = {
            let mut state = shared.lock_server_state();
            take_from_store(shared, &mut state, &vault_name);
            take_records_from_store(shared, &mut state, &vault_name);
            let pinned = shared.pinned_key(&vault_name).is_some();
            let (seq, head, accepted_env) = match state.vaults.get(&vault_name) {
                Some(slot) => (Some(slot.held.seq), Some(slot.held.head), true),
                None => (None, None, pinned),
            };
            let accepted_rec = accepted_env
                || holds_records(&state, &vault_name)
                || vault_name == crate::ALAB_VAULT;
            (seq, head, accepted_env, accepted_rec)
        };
        if accepted_envelope {
            match crate::transport::fetch_after_nowait(&vault_url, held_seq, budget) {
                Ok(crate::transport::Fetched::Body(bytes)) => {
                    let reply = put_vault(shared, &vault_name, &bytes, false);
                    if reply.status == 204 {
                        let advanced = {
                            let state = shared.lock_server_state();
                            state.vaults.get(&vault_name).is_some_and(|slot| {
                                Some(slot.held.seq) != held_seq || Some(slot.held.head) != held_head
                            })
                        };
                        if advanced {
                            stored += 1;
                        }
                    }
                }
                Ok(_) => {}
                Err(_) => return stored,
            }
        }
        if accepted_records {
            let _slot = shared.records_pulls.take();
            let have_records = shared
                .lock_server_state()
                .records
                .get(&vault_name)
                .map_or_else(Vec::new, |slot| slot.store.have());
            let bytes = match crate::transport::fetch_records(
                &vault_url,
                &have_records,
                None,
                budget,
                None,
            ) {
                Ok(bytes) => bytes,
                Err(_) => return stored,
            };
            if bytes.is_empty() {
                continue;
            }
            let before_len = shared
                .lock_server_state()
                .records
                .get(&vault_name)
                .map_or(0, |slot| slot.store.len());
            let reply = post_records(shared, &vault_name, &bytes, false);
            if reply.status == 204 {
                let after_len = shared
                    .lock_server_state()
                    .records
                    .get(&vault_name)
                    .map_or(0, |slot| slot.store.len());
                if after_len > before_len {
                    stored += 1;
                }
            }
        }
    }
    stored
}

fn verify_probed_alab_envelope(bytes: &[u8], shared: &Shared) -> Option<u64> {
    let env = envelope::parse(bytes).ok()?;
    if env.name != crate::ALAB_VAULT || env.seq < 1 {
        return None;
    }
    env.verify_signature().ok()?;
    env.check_signer(&shared.retired).ok()?;
    if let Some(pin) = shared.pinned_key(crate::ALAB_VAULT) {
        env.check_pin_with(&pin, &[], &shared.retired).ok()?;
    } else if !shared.retired.lineage(&env.signer, &env.reached_from()) {
        return None;
    }
    Some(env.seq)
}

fn probe_alab_endpoint(
    target: &str,
    own_port: u16,
    own_label: Option<&str>,
    here: &[Target],
    holding_alab_name: bool,
    shared: &Shared,
) -> Option<u64> {
    use std::net::ToSocketAddrs;
    let raw_url = if target.starts_with("http://") || target.starts_with("https://") {
        target.to_owned()
    } else {
        format!("http://{target}.local:{own_port}/v1/{}", crate::ALAB_VAULT)
    };
    let uri = aterm_http::Uri::parse(&raw_url)?;
    if is_obviously_self_url(&uri, own_port, own_label, here) {
        return None;
    }
    let ip_literal = uri.host().parse::<IpAddr>().is_ok();
    if holding_alab_name
        && !ip_literal
        && !here.is_empty()
        && uri.host().eq_ignore_ascii_case("alab-secrets.local")
    {
        let addrs = uri.authority().to_socket_addrs().ok()?;
        let mut best: Option<u64> = None;
        for addr in addrs {
            if is_self_addr(addr, own_port, here) {
                continue;
            }
            let direct = format!("http://{addr}/v1/{}", crate::ALAB_VAULT);
            if let Ok(crate::transport::Fetched::Body(bytes)) =
                crate::transport::fetch_after_nowait(&direct, None, Duration::from_secs(3))
                && let Some(seq) = verify_probed_alab_envelope(&bytes, shared)
            {
                best = Some(best.map_or(seq, |prev| prev.max(seq)));
            }
        }
        return best;
    }
    let Ok(crate::transport::Fetched::Body(bytes)) =
        crate::transport::fetch_after_nowait(&raw_url, None, Duration::from_secs(3))
    else {
        return None;
    };
    if !ip_literal
        && !here.is_empty()
        && let Ok(addrs) = uri.authority().to_socket_addrs()
    {
        let all_addrs: Vec<SocketAddr> = addrs.collect();
        if !all_addrs.is_empty()
            && all_addrs
                .iter()
                .all(|addr| is_self_addr(*addr, own_port, here))
        {
            return None;
        }
    }
    verify_probed_alab_envelope(&bytes, shared)
}

type AdvertiseTarget = (String, u16, Option<(Vec<Target>, Option<VaultTxt>)>);

/// What one round of [`tend_role_targets`] decided: this server's own name, and `alab-secrets`.
type RoleTargets = (Option<AdvertiseTarget>, Option<AdvertiseTarget>);

#[expect(
    clippy::too_many_arguments,
    reason = "role tender coordinates primary own-label slot, alab-secrets watcher, and peer pull"
)]
fn tend_role_targets(
    decide: &mut RoleDecider,
    port: u16,
    shared: &Shared,
    _client_paths: Option<&crate::client::Paths>,
    alab_watcher: &mut AlabNameWatcher,
    here: &[Target],
    peers: &[crate::discover::DiscoveredVault],
    holders: &[crate::discover::DiscoveredVault],
    probe_alab: impl FnMut(&str) -> Option<u64>,
) -> RoleTargets {
    let at = |txt| (!here.is_empty()).then(|| (here.to_vec(), txt));
    let mut pins = shared.pins_snapshot();
    let decided = decide(&mut pins);
    shared.merge_pins(&mut pins);
    let alab_name = crate::alab_advertised().to_owned();
    if !peers.is_empty() {
        let _ = pull_from_peers_shared(shared, peers, port, decided.as_deref(), here);
    }
    let Some(name) = decided else {
        return (None, Some((alab_name, port, None)));
    };
    let (primary_txt, own_alab_seq) = {
        let mut state = shared.lock_server_state();
        take_from_store(shared, &mut state, ALAB_VAULT);
        let txt = advertised_vault(&state, &name, Some(&name), &pins);
        let seq = shared
            .pinned_key(ALAB_VAULT)
            .and_then(|_| state.vaults.get(ALAB_VAULT).map(|slot| slot.held.seq));
        (txt, seq)
    };
    let primary = Some((name.clone(), port, at(primary_txt)));
    let secondary = if name != alab_name {
        let want_alab = alab_watcher.decide(
            &AlabNameInput {
                own_label: &name,
                own_seq: own_alab_seq,
                holders,
                now: Instant::now(),
            },
            probe_alab,
        );
        let maybe = if want_alab {
            let txt = {
                let state = shared.lock_server_state();
                advertised_vault(&state, &alab_name, Some(&name), &pins)
            };
            at(txt)
        } else {
            None
        };
        Some((alab_name, port, maybe))
    } else {
        Some((alab_name, port, None))
    };
    (primary, secondary)
}

/// Start one `dns-sd -P` of a name and port at a target with a TXT ([`Seams::spawn`]).
type SpawnAdvert =
    dyn Fn(&str, u16, &Target, Option<&VaultTxt>) -> io::Result<aterm_uds::spawnfd::Child> + Send;

/// Hear this Mac's sleep and wake until the bell given is gone, handing each to the handler
/// given; whether it hears them ([`Seams::power`]).
type ListenPower = dyn FnOnce(std::sync::Weak<Bell>, Box<dyn FnMut(Power) + Send>) -> bool + Send;

/// End one registration's `dns-sd -P` as this Mac goes to sleep ([`Seams::end_for_sleep`]).
type EndAdvert = dyn Fn(&mut aterm_uds::spawnfd::Child) + Send + Sync;

/// How long a will-sleep waits for this server's registrations to end before it lets the Mac
/// sleep anyway ([`SleepYield`]). Ending a `dns-sd` takes milliseconds; macOS itself waits at
/// most 30 s for every process it told before it sleeps.
const SLEEP_YIELD_BOUND: Duration = Duration::from_secs(2);

/// What the `vault-tend` thread reads and starts ([`Children::tend_seamed`]): this Mac's own,
/// or a test's.
struct Seams {
    /// Waits at most the time it is given for a change of this Mac's addresses or links, and
    /// says whether one came ([`crate::wake::route_changes`]). `None`: none is heard.
    routes: Option<Box<dyn FnMut(Duration) -> bool + Send>>,
    /// Whether a standing `_alab-vault._tcp` browse wakes the advert ([`crate::discover::Watch`]).
    browse: bool,
    /// Where this Mac advertises now ([`lan_targets`]).
    targets: Box<dyn Fn() -> Vec<Target> + Send>,
    /// Start one `dns-sd -P` of a name and port at a target with a TXT.
    spawn: Box<SpawnAdvert>,
    /// The longest nap between two looks.
    every: Duration,
    /// Hears this Mac's sleep and wake ([`aterm_uds::power::listen`]); a test's arm hears
    /// none, or what the test hands it.
    power: Box<ListenPower>,
    /// Ends a registration as this Mac goes to sleep ([`end`]).
    end_for_sleep: Arc<EndAdvert>,
    /// How long a will-sleep waits for that ([`SLEEP_YIELD_BOUND`]).
    sleep_bound: Duration,
}

impl Seams {
    /// This Mac's own: its route socket, its standing browse, its interfaces, `dns-sd` and its
    /// root power domain.
    fn this_mac() -> Seams {
        Seams {
            routes: crate::wake::route_changes()
                .map(|changes| Box::new(changes) as Box<dyn FnMut(Duration) -> bool + Send>),
            browse: true,
            targets: Box::new(lan_targets),
            spawn: Box::new(|name, port, target, txt| {
                quiet(DNS_SD, &registration_args(target, name, port, txt))
            }),
            every: ADVERTISE_EVERY,
            power: Box::new(aterm_uds::power::listen::<Bell>),
            end_for_sleep: Arc::new(end),
            sleep_bound: SLEEP_YIELD_BOUND,
        }
    }
}

/// What the server does as the system announces sleep and wake ([`Seams::power`],
/// `docs/DESIGN-fluid-swarm-2026-10-28.md` §4.2). A Mac about to sleep hands its mDNS records
/// to a Bonjour Sleep Proxy on the LAN (an Apple TV, a HomePod), which then answers for them
/// while the Mac sleeps. A holder's `alab-secrets` record answered that way can keep the name
/// from every other server (each probes it and stays off while it answers, and a registration
/// of the name loses to the proxy's), while nothing serves `:7484`. So a will-sleep ends every `dns-sd -P` this server holds, its own name's and
/// `alab-secrets`', before the Mac is let sleep, and a wake rings the advert's bell: its next
/// round registers its own name again and competes for `alab-secrets` as any server that does
/// not hold it does ([`AlabNameWatcher::slept`]).
struct SleepYield {
    set: Arc<Mutex<ChildSet>>,
    /// Set by a will-sleep and cleared by a wake: while set, no round registers anything.
    asleep: Arc<AtomicBool>,
    bell: std::sync::Weak<Bell>,
    end: Arc<EndAdvert>,
    bound: Duration,
}

impl SleepYield {
    /// Hear one announcement. A will-sleep returns once every registration ended, or after
    /// [`Self::bound`] (its caller lets the Mac sleep as it returns).
    fn hear(&self, power: Power) {
        match power {
            Power::WillSleep => self.yield_to_sleep(),
            Power::Woke => {
                if self.asleep.swap(false, Ordering::SeqCst)
                    && let Some(bell) = self.bell.upgrade()
                {
                    bell.ring(Wake::Resumed);
                }
            }
        }
    }

    /// End every registration this server holds, on a thread of its own, and wait for that at
    /// most [`Self::bound`]: a registration that will not end, or a round that holds the
    /// children's lock, never keeps the Mac awake longer.
    fn yield_to_sleep(&self) {
        self.asleep.store(true, Ordering::SeqCst);
        let (done, ended) = std::sync::mpsc::channel();
        let (set, asleep, end) = (
            Arc::clone(&self.set),
            Arc::clone(&self.asleep),
            Arc::clone(&self.end),
        );
        let spawned = std::thread::Builder::new()
            .name("vault-sleep".to_owned())
            .spawn(move || {
                let mut taken = take_for_sleep(&set, &asleep);
                let mut names: Vec<String> = Vec::new();
                for (name, child) in &mut taken {
                    end(child);
                    if let Some(name) = name.take()
                        && !names.contains(&name)
                    {
                        names.push(name);
                    }
                }
                let _ = done.send(names);
            });
        if let Err(e) = spawned {
            note(&format!(
                "this Mac is going to sleep, and this server could not end its registrations: {e}"
            ));
            return;
        }
        match ended.recv_timeout(self.bound) {
            Ok(names) if !names.is_empty() => {
                let names: Vec<String> = names.iter().map(|name| format!("{name}.local")).collect();
                note(&format!(
                    "this Mac is going to sleep, so this server stopped advertising {} until it \
                     wakes",
                    names.join(" and ")
                ));
            }
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => note(&format!(
                "this Mac is going to sleep, and this server's registrations did not end within \
                 {}s; letting it sleep anyway",
                self.bound.as_secs_f32()
            )),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => note(
                "this Mac is going to sleep, and ending this server's registrations failed; \
                 letting it sleep anyway",
            ),
        }
    }
}

/// Take every registration out of `set` as this Mac goes to sleep, if it still is (`asleep`):
/// the ones held now, and so every one a round started before (each round looks at `asleep`
/// under the same lock, [`reregister_awake`]). Each comes with the name it was registered
/// under. The next round lets `alab-secrets` go ([`ChildSet::slept`]).
fn take_for_sleep(
    set: &Mutex<ChildSet>,
    asleep: &AtomicBool,
) -> Vec<(Option<String>, aterm_uds::spawnfd::Child)> {
    let mut guard = set.lock().unwrap_or_else(PoisonError::into_inner);
    let set = &mut *guard;
    if set.closed || !asleep.load(Ordering::SeqCst) {
        return Vec::new();
    }
    set.slept = true;
    let mut taken = Vec::new();
    for slot in [&mut set.dns, &mut set.alab_dns] {
        let name = slot.held_name.take();
        taken.extend(
            slot.held
                .drain(..)
                .map(|(_, _, child)| (name.clone(), child)),
        );
        slot.logged.clear();
        slot.dns_sd_lost = false;
    }
    taken
}

impl Children {
    fn lock(&self) -> MutexGuard<'_, ChildSet> {
        self.set.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keep the server's children up while it runs: the keep-awake assertion when
    /// `keep_awake` ([`tend_awake`]) and the mDNS registration of `advertise`'s name, port and
    /// held vault TXT ([`reregister_slot`]), checked on each wake and every [`ADVERTISE_EVERY`].
    #[allow(dead_code)]
    fn tend(&self, keep_awake: bool, advertise: Option<(String, u16, Arc<Shared>)>) {
        self.tend_with_paths(keep_awake, advertise, None);
    }

    #[allow(dead_code)]
    fn tend_with_paths(
        &self,
        keep_awake: bool,
        advertise: Option<(String, u16, Arc<Shared>)>,
        client_paths: Option<crate::client::Paths>,
    ) {
        let advertise = advertise.map(|(name, port, shared)| {
            let own = vec![name.clone()];
            let decide: RoleDecider = Box::new(move |_| Some(name.clone()));
            (decide, own, port, shared)
        });
        self.tend_with_role(keep_awake, advertise, client_paths);
    }

    fn tend_with_role(
        &self,
        keep_awake: bool,
        advertise: Option<(RoleDecider, Vec<String>, u16, Arc<Shared>)>,
        client_paths: Option<crate::client::Paths>,
    ) {
        if !cfg!(target_os = "macos") {
            if keep_awake {
                note("keep-awake is macOS only; ignored");
            }
            if advertise.is_some() {
                note("--advertise is macOS only (dns-sd); ignored");
            }
            return;
        }
        if !keep_awake && advertise.is_none() {
            return;
        }
        self.tend_role_seamed(keep_awake, advertise, client_paths, Seams::this_mac());
    }

    /// [`Children::tend_with_paths`] on any platform, reading and starting through `seams`. Its
    /// thread naps on a bell rung by a PUT that stores a new version, by `seams`' route changes
    /// and by its standing browse's Adds (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.5.2, F3a),
    /// so a Mac back on the LAN advertises its new address at once, not at its next look.
    #[cfg_attr(not(test), allow(dead_code))]
    fn tend_seamed(
        &self,
        keep_awake: bool,
        advertise: Option<(String, u16, Arc<Shared>)>,
        client_paths: Option<crate::client::Paths>,
        seams: Seams,
    ) {
        let advertise = advertise.map(|(name, port, shared)| {
            let own = vec![name.clone()];
            let decide: RoleDecider = Box::new(move |_| Some(name.clone()));
            (decide, own, port, shared)
        });
        self.tend_role_seamed(keep_awake, advertise, client_paths, seams);
    }

    fn tend_role_seamed(
        &self,
        keep_awake: bool,
        mut advertise: Option<(RoleDecider, Vec<String>, u16, Arc<Shared>)>,
        client_paths: Option<crate::client::Paths>,
        mut seams: Seams,
    ) {
        let held = Arc::clone(&self.set);
        let pid = std::process::id();
        let bell = Arc::new(Bell::default());
        if let Some(routes) = seams.routes.take() {
            crate::wake::hear(&bell, routes, Wake::Route);
        }
        // A server that advertises ends its registrations before this Mac sleeps.
        let asleep = Arc::new(AtomicBool::new(false));
        if advertise.is_some() {
            let listen = std::mem::replace(&mut seams.power, Box::new(|_, _| false));
            let yielding = SleepYield {
                set: Arc::clone(&held),
                asleep: Arc::clone(&asleep),
                bell: Arc::downgrade(&bell),
                end: Arc::clone(&seams.end_for_sleep),
                bound: seams.sleep_bound,
            };
            let hears = listen(
                Arc::downgrade(&bell),
                Box::new(move |power| yielding.hear(power)),
            );
            if !hears && cfg!(target_os = "macos") {
                note(
                    "could not hear this Mac's sleep and wake, so this server's advertisements \
                     stay registered while it sleeps",
                );
            }
        }
        // The browse rings for servers other than this one: its own names' Adds come back
        // with each registration it starts.
        let watch = (seams.browse && advertise.is_some()).then(|| {
            let own: Vec<&str> = advertise
                .iter()
                .flat_map(|(_, names, _, _)| names.iter().map(String::as_str))
                .chain([crate::alab_advertised()])
                .collect();
            crate::discover::Watch::start(&bell, &own)
        });
        let napping = Arc::clone(&bell);
        let shared_for_wake = advertise
            .as_ref()
            .map(|(_, _, _, shared)| Arc::clone(shared));
        let spawned = std::thread::Builder::new()
            .name("vault-tend".to_owned())
            .spawn(move || {
                let watch = watch;
                let mut alab_watcher = AlabNameWatcher::new();
                let mut why = None;
                loop {
                    let here = (seams.targets)();
                    let (peers, holders) = match &watch {
                        Some(w) => (
                            w.peer_vaults(PEER_RESOLVE_BUDGET),
                            w.alab_holders(PEER_RESOLVE_BUDGET),
                        ),
                        None => (Vec::new(), Vec::new()),
                    };
                    let role_targets = advertise.as_mut().map(|(decide, _, port, shared)| {
                        after_sleep(&held, &mut alab_watcher);
                        let port = *port;
                        let holding = alab_watcher.is_holding();
                        let targets = tend_role_targets(
                            decide,
                            port,
                            shared,
                            client_paths.as_ref(),
                            &mut alab_watcher,
                            &here,
                            &peers,
                            &holders,
                            |target| {
                                probe_alab_endpoint(target, port, None, &here, holding, shared)
                            },
                        );
                        if let (Some((name, _, _)), Some(w)) = (&targets.0, &watch) {
                            w.note_own(name);
                        }
                        targets
                    });
                    {
                        let mut set = held.lock().unwrap_or_else(PoisonError::into_inner);
                        if set.closed {
                            return;
                        }
                        if keep_awake {
                            tend_awake(&mut set, &|| quiet(CAFFEINATE, &caffeinate_args(pid)));
                        }
                        if let Some(targets) = role_targets {
                            reregister_awake(
                                &mut set,
                                &asleep,
                                targets,
                                &mut alab_watcher,
                                why,
                                &*seams.spawn,
                            );
                        }
                    }
                    why = Some(napping.nap(seams.every));
                }
            });
        match spawned {
            Ok(_) => {
                if let Some(shared) = shared_for_wake {
                    *shared
                        .advertiser
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&bell));
                }
                *self.bell.lock().unwrap_or_else(PoisonError::into_inner) = Some(bell);
            }
            Err(e) => note(&format!(
                "could not start keeping awake or advertising: {e}"
            )),
        }
    }
}

/// Keep one keep-awake assertion ([`CAFFEINATE`] tied to the server's pid) running: a live one
/// stays, and one that ended (a person's `killall caffeinate`) or never started is started
/// again by `spawn`. The log says so only when that changes.
fn tend_awake(set: &mut ChildSet, spawn: &dyn Fn() -> io::Result<aterm_uds::spawnfd::Child>) {
    if let Some(child) = set.caffeinate.as_mut()
        && matches!(child.try_wait(), Ok(None))
    {
        return;
    }
    let ended = set
        .caffeinate
        .take()
        .map(|mut child| end(&mut child))
        .is_some();
    match spawn() {
        Ok(child) => {
            set.caffeinate = Some(child);
            if ended || set.awake_failed {
                note("keeping this Mac awake again");
            }
            set.awake_failed = false;
        }
        Err(e) => {
            if !set.awake_failed {
                note(&format!("could not start {CAFFEINATE}: {e}"));
            }
            set.awake_failed = true;
        }
    }
}

/// Keep `set`'s registrations at what one round of [`tend_role_targets`] decided, each started by
/// `spawn`: this server's own name in [`ChildSet::dns`] and `alab-secrets` in
/// [`ChildSet::alab_dns`] ([`reregister_slot`]). A held `alab-secrets` registration that ended
/// by itself while still wanted lost the name to another device
/// ([`AlabNameWatcher::lost_name`]): this round ends it and starts none in its place, where
/// [`reregister_slot`] alone would start one on every round, each losing the same way. The log
/// says so once, and again only after a later hold of the name stays up for a round.
fn reregister_roles(
    set: &mut ChildSet,
    (primary, secondary): RoleTargets,
    alab_watcher: &mut AlabNameWatcher,
    why: Option<Wake>,
    spawn: &SpawnAdvert,
) {
    match primary {
        Some((name, port, target)) => {
            reregister_slot(&mut set.dns, &name, target, why, &|at, txt| {
                spawn(&name, port, at, txt)
            });
        }
        None => {
            reregister_slot(&mut set.dns, "", None, why, &|_, _| {
                Err(io::Error::other("advertising disabled"))
            });
        }
    }
    let Some((alab_name, port, mut alab_target)) = secondary else {
        return;
    };
    match wanted_ended(&mut set.alab_dns, &alab_name, alab_target.as_ref()) {
        Some(true) => {
            alab_watcher.lost_name();
            alab_target = None;
            if !std::mem::replace(&mut set.alab_lost_said, true) {
                note(&format!(
                    "another device answers to {alab_name}.local, so this server leaves the name \
                     to it and serves under its own; it takes the name again after {}s in which \
                     no vault answers there",
                    ALAB_FALLBACK_AFTER.as_secs()
                ));
            }
        }
        Some(false) => set.alab_lost_said = false,
        None => {}
    }
    reregister_slot(
        &mut set.alab_dns,
        &alab_name,
        alab_target,
        why,
        &|at, txt| spawn(&alab_name, port, at, txt),
    );
}

/// Before a round decides: a will-sleep that took the registrations since the last round
/// ([`ChildSet::slept`]) lets `alab-secrets` go first ([`AlabNameWatcher::slept`]), so the
/// round after a wake decides as a server that does not hold the name, and competes for it by
/// the probe.
fn after_sleep(set: &Mutex<ChildSet>, alab_watcher: &mut AlabNameWatcher) {
    let slept = std::mem::take(&mut set.lock().unwrap_or_else(PoisonError::into_inner).slept);
    if slept {
        alab_watcher.slept();
    }
}

/// [`reregister_roles`], unless this Mac is going to sleep or asleep (`asleep`), when nothing
/// is registered ([`SleepYield`]). A round that a will-sleep overtook after it decided
/// ([`ChildSet::slept`], set since [`after_sleep`]) lets `alab-secrets` go
/// ([`AlabNameWatcher::slept`]) and registers nothing either, since its decision predates the
/// sleep: the round after the wake (which the wake rings) decides again. A round while asleep
/// (the dark wakes macOS runs between a will-sleep and the next full wake, when nothing may
/// answer at the name only because the network is down) lets it go too, so no time asleep
/// counts toward [`ALAB_FALLBACK_AFTER`] and the first round after the wake probes the name.
fn reregister_awake(
    set: &mut ChildSet,
    asleep: &AtomicBool,
    targets: RoleTargets,
    alab_watcher: &mut AlabNameWatcher,
    why: Option<Wake>,
    spawn: &SpawnAdvert,
) {
    if std::mem::take(&mut set.slept) {
        alab_watcher.slept();
        return;
    }
    if asleep.load(Ordering::SeqCst) {
        alab_watcher.slept();
        return;
    }
    reregister_roles(set, targets, alab_watcher, why, spawn);
}

/// Whether one of `slot`'s registrations that `want` still wants under `name` (its target and
/// TXT, as [`reregister_slot`] keeps one) ended by itself: `None` when it holds none so wanted.
fn wanted_ended(
    slot: &mut DnsSdSlot,
    name: &str,
    want: Option<&(Vec<Target>, Option<VaultTxt>)>,
) -> Option<bool> {
    let (targets, txt) = want.filter(|_| slot.held_name.as_deref() == Some(name))?;
    let mut ended = None;
    for (target, held_txt, child) in &mut slot.held {
        if targets.contains(target) && txt == held_txt {
            let gone = matches!(child.try_wait(), Ok(Some(_)));
            ended = Some(ended.unwrap_or(false) || gone);
        }
    }
    ended
}

/// Keep `slot`'s `dns-sd -P` registrations (each started by `spawn`) at `want`'s targets and
/// TXT: a live registration at a target still wanted with the same TXT stays, and every other
/// one is replaced cleanly. One that ended by itself gave the name up to another device on the
/// network, which new Macs may then reach in place of this vault: the log says so once, and
/// again once every registration stays up. A round that starts one names `why` it ran (`None`:
/// the first, as the server started) in its log line, and keeps it ([`DnsSdSlot::started_by`]).
fn reregister_slot(
    slot: &mut DnsSdSlot,
    name: &str,
    want: Option<(Vec<Target>, Option<VaultTxt>)>,
    why: Option<Wake>,
    spawn: &dyn Fn(&Target, Option<&VaultTxt>) -> io::Result<aterm_uds::spawnfd::Child>,
) {
    let same_name = slot.held_name.as_deref() == Some(name);
    let mut gave_up = false;
    let mut kept = Vec::new();
    for (target, held_txt, mut child) in std::mem::take(&mut slot.held) {
        let wanted = same_name
            && want
                .as_ref()
                .is_some_and(|(targets, txt)| targets.contains(&target) && *txt == held_txt);
        match child.try_wait() {
            Ok(None) if wanted => {
                kept.push((target, held_txt, child));
                continue;
            }
            Ok(Some(_)) if wanted => gave_up = true,
            _ => {}
        }
        end(&mut child);
    }
    slot.held = kept;
    if !same_name {
        slot.logged.clear();
        slot.dns_sd_lost = false;
    }
    if gave_up && !slot.dns_sd_lost {
        slot.dns_sd_lost = true;
        note(&format!(
            "{DNS_SD} gave up advertising {name}.local: another device on this network answers \
             to that name, so new Macs may reach it in place of this vault (--advertise <name> \
             picks another); trying again every {}s",
            ADVERTISE_EVERY.as_secs()
        ));
    }
    let Some((targets, txt)) = want else {
        slot.held_name = None;
        slot.logged.clear();
        slot.dns_sd_lost = false;
        return;
    };
    let missing: Vec<&Target> = targets
        .iter()
        .filter(|target| !slot.held.iter().any(|(held, _, _)| held == *target))
        .collect();
    if missing.is_empty() {
        if !gave_up && std::mem::take(&mut slot.dns_sd_lost) {
            note(&format!(
                "advertising {name}.local at {} again",
                addresses(&slot.logged)
            ));
        }
        return;
    }
    for target in missing {
        match spawn(target, txt.as_ref()) {
            Ok(child) => slot.held.push((target.clone(), txt.clone(), child)),
            Err(e) => note(&format!("could not start {DNS_SD}: {e}")),
        }
    }
    slot.held_name = (!slot.held.is_empty()).then(|| name.to_owned());
    slot.started_by = Some(why);
    let ips: Vec<Ipv4Addr> = slot.held.iter().map(|(target, _, _)| target.ip).collect();
    if !slot.dns_sd_lost && !ips.is_empty() && slot.logged != ips {
        note(&advertising_line(name, &ips, why));
    }
    slot.logged = ips;
}

/// The log line for advertising `name` at `ips`, saying what woke the round (`why`), unless it
/// was the first.
fn advertising_line(name: &str, ips: &[Ipv4Addr], why: Option<Wake>) -> String {
    let woken = why.map_or(String::new(), |why| format!(" (woken by {})", why.words()));
    format!("advertising {name}.local at {}{woken}", addresses(ips))
}

fn addresses(ips: &[Ipv4Addr]) -> String {
    ips.iter()
        .map(Ipv4Addr::to_string)
        .collect::<Vec<_>>()
        .join(" and ")
}

impl Drop for Children {
    fn drop(&mut self) {
        let mut guard = self.lock();
        let set = &mut *guard;
        set.closed = true;
        if let Some(mut child) = set.caffeinate.take() {
            end(&mut child);
        }
        for slot in [&mut set.dns, &mut set.alab_dns] {
            for (_, _, mut child) in slot.held.drain(..) {
                end(&mut child);
            }
        }
        drop(guard);
        if let Some(bell) = self
            .bell
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            bell.ring(Wake::Stop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Locker, Unsigned, endorse, sign, signer_public, test_armor};
    use crate::transport::{self, Fetched, Source};

    const HANG_DETECTOR: Duration = Duration::from_secs(60);
    const OLD: [u8; 32] = [7u8; 32];
    const NEW: [u8; 32] = [8u8; 32];
    const STRANGER: [u8; 32] = [9u8; 32];

    fn envelope(seed: &[u8; 32], seq: u64) -> Vec<u8> {
        envelope_with(seed, seq, Vec::new(), "alab")
    }

    fn unsigned(seq: u64, handoff: Vec<([u8; 32], [u8; 64])>, name: &str) -> Unsigned {
        Unsigned {
            name: name.to_owned(),
            seq,
            time: 1_792_530_000 + seq,
            handoff,
            parents: Vec::new(),
            lockers: vec![
                Locker {
                    label: "admin".to_owned(),
                    armored: test_armor(&[seq as u8; 40]),
                },
                Locker {
                    label: "1".to_owned(),
                    armored: test_armor(b"everyone"),
                },
            ],
        }
    }

    fn envelope_with(
        seed: &[u8; 32],
        seq: u64,
        handoff: Vec<([u8; 32], [u8; 64])>,
        name: &str,
    ) -> Vec<u8> {
        sign(&unsigned(seq, handoff, name), seed).expect("signed")
    }

    fn options(store: &Path, hold: Duration) -> Options {
        Options {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: store.to_path_buf(),
            advertise: None,
            pins: Vec::new(),
            hold,
            keep_awake: false,
        }
    }

    fn run(options: &Options) -> Running {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        start(listener, options).expect("started")
    }

    fn run_tuned(options: &Options, tuning: Tuning) -> Running {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        launch(listener, options, tuning).expect("started")
    }

    /// Wait (a hang detector, not a budget) until `done` holds.
    fn until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + HANG_DETECTOR;
        while !done() {
            assert!(Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// A connection that has sent `request` and reads nothing yet.
    fn sent(running: &Running, request: &[u8]) -> TcpStream {
        let mut stream = TcpStream::connect(running.addr).expect("connect");
        stream
            .set_read_timeout(Some(HANG_DETECTOR))
            .expect("timeout");
        stream.write_all(request).expect("write");
        stream
    }

    fn read_all(mut stream: TcpStream) -> String {
        let mut answer = Vec::new();
        let _ = stream.read_to_end(&mut answer);
        String::from_utf8_lossy(&answer).into_owned()
    }

    /// Key `n`'s join code.
    fn code(n: u8) -> String {
        let key = crate::transport::test_recipient(n);
        crate::client::join_code(&crate::age::Recipient::parse(&key).expect("a key"))
    }

    fn join_line(n: u8, host: &str) -> String {
        format!("{} {host} {}", crate::transport::test_recipient(n), code(n))
    }

    fn queued(request: JoinRequest, peer: Option<IpAddr>) -> Queued {
        Queued {
            request,
            peer,
            observed_lan: false,
            asked: 1,
            changed: 0,
            noted: 0,
            refused: None,
        }
    }

    /// A server's clocks with no gap between changes to one join request.
    fn changeable() -> Tuning {
        Tuning {
            join_change_gap: Duration::ZERO,
            ..Tuning::default()
        }
    }

    /// Wait (a hang detector) until no connection is open.
    fn until_closed(running: &Running) {
        until("every connection has closed", || {
            running.shared.active.load(Ordering::SeqCst) == 0
        });
    }

    /// How many PUT bodies are being read.
    fn reading(running: &Running) -> usize {
        running.shared.lock_puts().reading
    }

    fn url(running: &Running) -> String {
        format!("http://{}/v1/alab", running.addr)
    }

    fn source(running: &Running) -> Source {
        Source::Url(url(running))
    }

    fn get(running: &Running) -> crate::Result<Fetched> {
        transport::fetch(&source(running), None, HANG_DETECTOR)
    }

    fn put(running: &Running, bytes: &[u8]) -> crate::Result<()> {
        transport::publish(&source(running), bytes)
    }

    /// Send `request` as is and read the whole answer.
    fn raw(running: &Running, request: &[u8]) -> String {
        let mut stream = TcpStream::connect(running.addr).expect("connect");
        stream
            .set_read_timeout(Some(HANG_DETECTOR))
            .expect("timeout");
        stream
            .set_write_timeout(Some(HANG_DETECTOR))
            .expect("timeout");
        stream.write_all(request).expect("write");
        let mut answer = Vec::new();
        let _ = stream.read_to_end(&mut answer);
        String::from_utf8_lossy(&answer).into_owned()
    }

    fn stored(store: &Path) -> Option<Vec<u8>> {
        std::fs::read(store.join("alab.vault")).ok()
    }

    /// Wait (a hang detector, not a budget) until `n` long-polls are held.
    fn until_waiting(running: &Running, n: usize) {
        let deadline = Instant::now() + HANG_DETECTOR;
        while running.waiting() != n {
            assert!(Instant::now() < deadline, "no long-poll was held");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn the_first_envelope_is_trusted_on_first_use_and_served_with_an_etag() {
        use std::os::unix::fs::PermissionsExt;
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let store = root.path().join("served");
        let running = run(&options(&store, Duration::from_millis(200)));
        assert_eq!(get(&running).expect("answered"), Fetched::Missing);

        let first = envelope(&OLD, 1);
        put(&running, &first).expect("trusted on first use");
        assert_eq!(get(&running).expect("held"), Fetched::Body(first.clone()));
        assert_eq!(stored(&store).as_deref(), Some(first.as_slice()));
        let mode =
            |path: &Path| std::fs::metadata(path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&store.join("alab.vault")), 0o600);
        assert_eq!(mode(&store), 0o700);

        let response = aterm_http::Client::new(
            aterm_http::Trust::PlatformVerifier,
            aterm_http::ProxyMode::Direct,
            HANG_DETECTOR,
        )
        .get(&url(&running))
        .limit(MAX_ENVELOPE)
        .send(&[])
        .expect("answered");
        assert_eq!(response.status(), 200);
        assert_eq!(response.header("etag"), Some("\"1\""));
        assert_eq!(response.header("cache-control"), Some("no-store"));
        assert_eq!(
            response.header("content-type"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(response.body(), first.as_slice());

        put(&running, &first).expect("idempotent");
        running.stop();
    }

    #[test]
    fn a_store_that_holds_no_vault_says_a_mac_in_the_vault_puts_its_copy_here() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(
            &root.path().join("served"),
            Duration::from_millis(200),
        ));
        let pins = [("alab".to_string(), OLD)];
        let unheld =
            |pins: &[(String, [u8; 32])]| unheld_note(&running.shared.lock_server_state(), pins);
        let pinned = unheld(&pins).expect("an empty store says so");
        assert!(
            pinned.contains("a Mac in the vault puts its copy here"),
            "{pinned}"
        );
        assert!(
            pinned.contains("a vault this server pins (alab)"),
            "{pinned}"
        );
        let unpinned = unheld(&[]).expect("an empty store says so");
        assert!(unpinned.contains("only from this Mac"), "{unpinned}");
        put(&running, &envelope(&OLD, 1)).expect("trusted on first use");
        assert_eq!(unheld(&pins), None);
        running.stop();
    }

    #[test]
    fn refused_puts_name_why_and_leave_the_store_unchanged() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let store = root.path().to_path_buf();
        let running = run(&options(&store, Duration::from_millis(200)));
        let three = envelope(&OLD, 3);
        put(&running, &three).expect("first");

        let mut forged = envelope(&OLD, 4);
        let at = forged.len() - 10;
        forged[at] = if forged[at] == b'A' { b'B' } else { b'A' };
        let refusals = [
            (forged, "403", "signature"),
            (envelope(&OLD, 2), "409", "not newer"),
            (envelope(&OLD, 3)[..20].to_vec(), "400", "envelope"),
            (envelope(&STRANGER, 4), "403", "pinned key"),
            (
                envelope_with(&OLD, 4, Vec::new(), "other"),
                "400",
                "names the vault `other`",
            ),
        ];
        for (bytes, status, why) in refusals {
            let error = put(&running, &bytes).expect_err(why);
            assert!(error.0.contains(status), "{why}: {error}");
            assert!(error.0.contains(why), "{why}: {error}");
            assert_eq!(stored(&store).as_deref(), Some(three.as_slice()), "{why}");
            assert_eq!(get(&running).expect("held"), Fetched::Body(three.clone()));
        }
        running.stop();
    }

    #[test]
    fn a_same_seq_with_other_bytes_is_refused_as_not_newer() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 3)).expect("first");
        let other = sign(
            &Unsigned {
                name: "alab".to_owned(),
                seq: 3,
                time: 1,
                handoff: Vec::new(),
                parents: Vec::new(),
                lockers: Vec::new(),
            },
            &OLD,
        )
        .expect("signed");
        let error = put(&running, &other).expect_err("fork");
        assert!(error.0.contains("409"), "{error}");
        running.stop();
    }

    #[test]
    fn an_endorsed_handoff_moves_the_pin() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let new = signer_public(&NEW).expect("public");
        let old = signer_public(&OLD).expect("public");
        let elsewhere = endorse(&OLD, "other", &new).expect("endorsed");
        let forged =
            crate::envelope::sign_unchecked(&unsigned(2, vec![(old, elsewhere)], "alab"), &NEW)
                .expect("rendered");
        let error = put(&running, &forged).expect_err("refused");
        assert!(error.0.contains("403"), "{error}");
        assert!(error.0.contains("endorsement"), "{error}");

        let chain = vec![(old, endorse(&OLD, "alab", &new).expect("e"))];
        let handoff = envelope_with(&NEW, 2, chain.clone(), "alab");
        put(&running, &handoff).expect("an endorsed handoff");
        put(&running, &envelope_with(&NEW, 3, chain, "alab")).expect("the new key");
        let error = put(&running, &envelope(&OLD, 4)).expect_err("the old key");
        assert!(error.0.contains("403"), "{error}");
        running.stop();
    }

    #[test]
    fn a_configured_pin_holds_for_a_vault_not_yet_held() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let mut options = options(root.path(), Duration::from_millis(200));
        options.pins = vec![("alab".to_owned(), signer_public(&OLD).expect("public"))];
        let running = run(&options);
        let error = put(&running, &envelope(&STRANGER, 1)).expect_err("not the pin");
        assert!(error.0.contains("403"), "{error}");
        assert!(stored(root.path()).is_none());
        put(&running, &envelope(&OLD, 1)).expect("the pin");
        running.stop();
    }

    #[test]
    fn a_fresh_store_pinned_to_the_first_key_takes_a_vault_three_rotations_on() {
        let seeds: Vec<[u8; 32]> = (0..4u8).map(|index| [0x40 + index; 32]).collect();
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let mut options = options(root.path(), Duration::from_millis(200));
        options.pins = vec![("alab".to_owned(), signer_public(&seeds[0]).expect("public"))];
        let running = run(&options);
        let mut broken = crate::envelope::test_chain("alab", &seeds);
        broken[1].1 = endorse(
            &seeds[1],
            "alab",
            &signer_public(&STRANGER).expect("public"),
        )
        .expect("endorsed");
        let forged = crate::envelope::sign_unchecked(&unsigned(9, broken, "alab"), &seeds[3])
            .expect("rendered");
        let error = put(&running, &forged).expect_err("a broken middle link");
        assert!(error.0.contains("403"), "{error}");
        assert!(error.0.contains("endorsement of handoff 2 of 3"), "{error}");
        let error = put(&running, &envelope(&seeds[3], 9)).expect_err("no chain");
        assert!(error.0.contains("403"), "{error}");
        assert!(stored(root.path()).is_none());

        let chained = envelope_with(
            &seeds[3],
            9,
            crate::envelope::test_chain("alab", &seeds),
            "alab",
        );
        put(&running, &chained).expect("followed from the pin");
        let error = put(&running, &envelope(&seeds[3], 10)).expect_err("the chain dropped");
        assert!(error.0.contains("403"), "{error}");
        assert!(error.0.contains("handoff 1 of 3"), "{error}");
        let next = envelope_with(
            &seeds[3],
            10,
            crate::envelope::test_chain("alab", &seeds),
            "alab",
        );
        put(&running, &next).expect("the chain carried on");
        running.stop();
    }

    #[test]
    fn a_rotation_that_drops_a_link_of_the_held_chain_is_refused() {
        let seeds: Vec<[u8; 32]> = (0..3u8).map(|index| [0x50 + index; 32]).collect();
        let chain = crate::envelope::test_chain("alab", &seeds);
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&seeds[0], 1)).expect("first");
        let once = envelope_with(&seeds[1], 2, chain[..1].to_vec(), "alab");
        put(&running, &once).expect("the first rotation");

        let cut = envelope_with(&seeds[2], 3, chain[1..].to_vec(), "alab");
        let error = put(&running, &cut).expect_err("the first link dropped");
        assert!(error.0.contains("403"), "{error}");
        assert!(
            error.0.contains("drops or changes handoff 1 of 1"),
            "{error}"
        );
        let first = crate::envelope::fingerprint(&signer_public(&seeds[0]).expect("public"));
        assert!(error.0.contains(&first), "{error}");
        let error = put(&running, &envelope(&seeds[1], 3)).expect_err("dropped in place");
        assert!(error.0.contains("handoff 1 of 1"), "{error}");
        assert_eq!(stored(root.path()).as_deref(), Some(once.as_slice()));
        put(
            &running,
            &envelope_with(&seeds[2], 3, chain.clone(), "alab"),
        )
        .expect("the chain carried on");
        running.stop();

        let other = root.path().join("cut");
        let running = run(&options(&other, Duration::from_millis(200)));
        put(&running, &cut).expect("its first version, from this machine");
        put(&running, &envelope_with(&seeds[2], 4, chain, "alab")).expect("restored");
        running.stop();
    }

    #[test]
    fn a_store_the_removed_admin_fed_takes_the_vault_once_its_build_retires_the_key() {
        let old = signer_public(&OLD).expect("public");
        let to = |seed: &[u8; 32]| {
            vec![(
                old,
                endorse(&OLD, "alab", &signer_public(seed).expect("public")).expect("endorsed"),
            )]
        };
        let real = envelope_with(&NEW, 3, to(&NEW), "alab");
        let fork = envelope_with(&STRANGER, 1 << 40, to(&STRANGER), "alab");
        let high = envelope(&OLD, 1 << 40);
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let hold = Duration::from_millis(200);
        for (fed, why) in [(fork, "endorsed no handoff"), (high, "not newer")] {
            let store = root.path().join(why.replace(' ', "-"));
            let running = run(&options(&store, hold));
            put(&running, &envelope(&OLD, 1)).expect("first");
            put(&running, &fed).expect("taken before a build knew OLD retired");
            let error = put(&running, &real).expect_err("captured");
            assert!(error.0.contains(why), "{error}");
            running.stop();

            let running = run_tuned(&options(&store, hold), retiring());
            put(&running, &real).expect("the held key gave way");
            assert_eq!(stored(&store).as_deref(), Some(real.as_slice()));
            assert!(
                put(&running, &fed).is_err(),
                "what the removed admin signs is not taken again"
            );
            running.stop();
        }
    }

    #[test]
    fn joins_asked_under_a_key_that_gave_way_give_way_to_new_ones() {
        let old = signer_public(&OLD).expect("public");
        let real = envelope_with(
            &NEW,
            3,
            vec![(
                old,
                endorse(&OLD, "alab", &signer_public(&NEW).expect("public")).expect("endorsed"),
            )],
            "alab",
        );
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let hold = Duration::from_millis(200);
        let running = run(&options(root.path(), hold));
        put(&running, &envelope(&OLD, 1)).expect("first");
        put(&running, &envelope(&OLD, 1 << 40)).expect("the removed admin's");
        let ask = |running: &Running, n: u8| {
            transport::post_join(&url(running), &join_line(n, &format!("m{n}")))
        };
        for n in 1..=4 {
            ask(&running, n).expect("queued");
        }
        running.stop();

        let running = run_tuned(&options(root.path(), hold), retiring());
        put(&running, &real).expect("the held key gave way");
        ask(&running, 5).expect("m1 gave way");
        assert_eq!(
            transport::list_joins(&url(&running)).expect("listed").len(),
            4
        );
        running.stop();
        let running = run_tuned(&options(root.path(), hold), retiring());
        ask(&running, 6).expect("m2 gave way");
        running.stop();
    }

    /// A server that knows OLD retired, and NEW the key that replaced it.
    fn retiring() -> Tuning {
        Tuning {
            retired: || crate::envelope::Retired {
                keys: vec![signer_public(&OLD).expect("public")],
                current: Some(signer_public(&NEW).expect("public")),
            },
            ..Tuning::default()
        }
    }

    #[test]
    fn a_retired_key_signs_nothing_the_server_takes() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let hold = Duration::from_millis(200);
        let unaware = root.path().join("unaware");
        let running = run(&options(&unaware, hold));
        put(&running, &envelope(&OLD, 1)).expect("not retired here");
        running.stop();

        let empty = root.path().join("empty");
        let running = run_tuned(&options(&empty, hold), retiring());
        let error = put(&running, &envelope(&OLD, 1)).expect_err("retired");
        assert!(error.0.contains("403"), "{error}");
        assert!(error.0.contains("rotated away from"), "{error}");
        assert!(stored(&empty).is_none());
        running.stop();

        let running = run_tuned(&options(&unaware, hold), retiring());
        let error = put(&running, &envelope(&OLD, 2)).expect_err("retired");
        assert!(error.0.contains("rotated away from"), "{error}");
        let old = signer_public(&OLD).expect("public");
        let to = |seed: &[u8; 32]| endorse(&OLD, "alab", &signer_public(seed).expect("public"));
        let fork = envelope_with(
            &STRANGER,
            2,
            vec![(old, to(&STRANGER).expect("endorsed"))],
            "alab",
        );
        let error = put(&running, &fork).expect_err("a fork the old key endorsed");
        assert!(error.0.contains("403"), "{error}");
        let real = envelope_with(&NEW, 2, vec![(old, to(&NEW).expect("endorsed"))], "alab");
        put(&running, &real).expect("the key that replaced it");
        assert_eq!(stored(&unaware).as_deref(), Some(real.as_slice()));
        running.stop();
    }

    #[test]
    fn a_long_poll_answers_at_once_when_newer_and_wakes_on_a_put() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), HANG_DETECTOR));
        put(&running, &envelope(&OLD, 5)).expect("first");
        let poll = |after| transport::fetch(&source(&running), Some(after), HANG_DETECTOR);
        assert_eq!(poll(4).expect("newer"), Fetched::Body(envelope(&OLD, 5)));

        let target = source(&running);
        let waiter = std::thread::spawn(move || transport::fetch(&target, Some(5), HANG_DETECTOR));
        until_waiting(&running, 1);
        put(&running, &envelope(&OLD, 6)).expect("newer");
        let woke = waiter.join().expect("waiter");
        assert_eq!(woke.expect("woken"), Fetched::Body(envelope(&OLD, 6)));
        running.stop();
    }

    #[test]
    fn a_long_poll_ends_with_204_after_the_hold_or_at_stop() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 5)).expect("first");
        let fetched = transport::fetch(&source(&running), Some(5), HANG_DETECTOR).expect("held");
        assert_eq!(
            fetched,
            Fetched::NoNewer(Some(5)),
            "the 204 names the seq held"
        );
        running.stop();

        let running = run(&options(root.path(), HANG_DETECTOR));
        let target = source(&running);
        let waiter = std::thread::spawn(move || transport::fetch(&target, Some(5), HANG_DETECTOR));
        until_waiting(&running, 1);
        running.stop();
        assert_eq!(
            waiter.join().expect("waiter").expect("ended"),
            Fetched::NoNewer(Some(5))
        );
    }

    /// A join request ends the queue's long-poll at once, naming the queue's next version, and
    /// so does a new version of the vault; one held while nothing changes ends at the hold.
    /// Negative control: a server without the long-poll refuses its query.
    #[test]
    fn a_join_long_poll_wakes_on_a_request_or_a_version_and_ends_at_the_hold() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), HANG_DETECTOR));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let at = url(&running);
        let listed = transport::wait_joins(&at, None, HANG_DETECTOR).expect("listed");
        let transport::Joins::Listed(lines, Some(version)) = listed else {
            panic!("the queue names its version: {listed:?}");
        };
        assert!(lines.is_empty());
        let waiting = |after| {
            let at = at.clone();
            std::thread::spawn(move || transport::wait_joins(&at, Some(after), HANG_DETECTOR))
        };

        let waiter = waiting(version);
        until_waiting(&running, 1);
        let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let line = join_line(1, "m22");
        assert_eq!(
            post_join(&running.shared, "alab", line.as_bytes(), peer).status,
            204
        );
        let woke = waiter.join().expect("waiter").expect("woken");
        let transport::Joins::Listed(lines, Some(next)) = woke else {
            panic!("the request is listed at once: {woke:?}");
        };
        assert_eq!(next, version + 1);
        assert!(lines.len() == 1 && lines[0].starts_with(&line), "{lines:?}");

        let waiter = waiting(next);
        until_waiting(&running, 1);
        put(&running, &envelope(&OLD, 2)).expect("a new version");
        assert_eq!(
            waiter.join().expect("waiter").expect("woken"),
            transport::Joins::Listed(lines, Some(next)),
            "a new version of the vault ends the queue's long-poll"
        );
        running.stop();

        let running = run(&options(root.path(), Duration::from_millis(200)));
        let at = url(&running);
        let Ok(transport::Joins::Listed(_, Some(version))) =
            transport::wait_joins(&at, None, HANG_DETECTOR)
        else {
            panic!("listed with its version");
        };
        assert_eq!(
            transport::wait_joins(&at, Some(version), HANG_DETECTOR).expect("held"),
            transport::Joins::Unchanged
        );
        assert_eq!(running.join_gets(), 2);
        running.stop();
    }

    /// The clock [`a_held_queue_version_from_another_run_or_before_an_expiry_is_answered_at_once`]
    /// moves, in unix seconds.
    static QUEUE_CLOCK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// A queue version an approver holds is answered at once, with the queue as it is, when it
    /// names the queue of an earlier run of the server (a server put back counts its changes
    /// afresh) or when a request in the listing it names expired since. Negative controls: a
    /// count that starts at zero each run, or an expiry that leaves the version where it was,
    /// holds the approver's long-poll while the queue it holds differs from the queue.
    #[test]
    fn a_held_queue_version_from_another_run_or_before_an_expiry_is_answered_at_once() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let ask = |running: &Running, n: u8, host: &str| {
            let line = join_line(n, host);
            assert_eq!(
                post_join(&running.shared, "alab", line.as_bytes(), peer).status,
                204
            );
        };
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 1)).expect("first");
        ask(&running, 1, "m22");
        let Ok(transport::Joins::Listed(_, Some(heard))) =
            transport::wait_joins(&url(&running), None, HANG_DETECTOR)
        else {
            panic!("listed with its version");
        };
        running.stop();

        // Put back from its store, then as many changes as the first run had.
        let running = run(&options(root.path(), Duration::from_millis(200)));
        ask(&running, 2, "m23");
        let woke =
            transport::wait_joins(&url(&running), Some(heard), HANG_DETECTOR).expect("answered");
        let transport::Joins::Listed(lines, Some(version)) = woke else {
            panic!("a version from the last run named this run's queue: {woke:?}");
        };
        assert_eq!(lines.len(), 2, "{lines:?}");
        running.stop();

        QUEUE_CLOCK.store(now(), Ordering::SeqCst);
        let tuning = Tuning {
            now: || QUEUE_CLOCK.load(Ordering::SeqCst),
            ..Tuning::default()
        };
        let running = run_tuned(&options(root.path(), Duration::from_millis(200)), tuning);
        let Ok(transport::Joins::Listed(lines, Some(version))) =
            transport::wait_joins(&url(&running), Some(version), HANG_DETECTOR)
        else {
            panic!("listed with its version");
        };
        assert_eq!(lines.len(), 2, "{lines:?}");
        QUEUE_CLOCK.fetch_add(JOIN_TTL_SECS + 60, Ordering::SeqCst);
        let woke =
            transport::wait_joins(&url(&running), Some(version), HANG_DETECTOR).expect("answered");
        assert!(
            matches!(&woke, transport::Joins::Listed(lines, Some(next)) if lines.is_empty() && *next != version),
            "the requests expired, and the listing that held them is answered: {woke:?}"
        );
        running.stop();
    }

    #[test]
    fn a_browsers_request_is_refused_before_anything_is_read_or_queued() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let line = join_line(1, "m22");
        for header in [
            "Origin: https://example.com",
            "origin: null",
            "Sec-Fetch-Site: cross-site",
            "SEC-FETCH-MODE: no-cors",
        ] {
            let answer = raw(
                &running,
                format!(
                    "POST /v1/alab/join HTTP/1.1\r\nHost: alab-secrets.local:7484\r\n{header}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{line}",
                    line.len()
                )
                .as_bytes(),
            );
            assert!(answer.starts_with("HTTP/1.1 403 "), "{header}: {answer}");
        }
        for path in ["/v1/alab", "/v1/alab/join"] {
            let answer = raw(
                &running,
                format!(
                    "GET {path} HTTP/1.1\r\nHost: rebound.example:7484\r\nSec-Fetch-Site: same-origin\r\n\r\n"
                )
                .as_bytes(),
            );
            assert!(answer.starts_with("HTTP/1.1 403 "), "{path}: {answer}");
        }
        assert!(
            transport::list_joins(&url(&running))
                .expect("listed")
                .is_empty(),
            "nothing a browser sent was queued"
        );
        transport::post_join(&url(&running), &line).expect("queued");
        assert_eq!(
            transport::list_joins(&url(&running)).expect("listed").len(),
            1
        );
    }

    #[test]
    fn join_requests_queue_deduplicate_and_validate() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run_tuned(
            &options(root.path(), Duration::from_millis(200)),
            changeable(),
        );
        let key = |n: u8| crate::transport::test_recipient(n);
        let line = join_line(1, "m22");
        let error = transport::post_join(&url(&running), &line).expect_err("no vault yet");
        assert!(error.0.contains("404"), "{error}");
        put(&running, &envelope(&OLD, 1)).expect("first");

        transport::post_join(&url(&running), &line).expect("queued");
        transport::post_join(&url(&running), &join_line(2, "m3")).expect("queued");
        transport::post_join(&url(&running), &join_line(1, "m22b")).expect("again");
        let listed = transport::list_joins(&url(&running)).expect("listed");
        assert_eq!(listed.len(), 2, "{listed:?}");
        let requests: Vec<JoinRequest> = listed
            .iter()
            .map(|line| parse_join_line(line).expect("a listed line parses"))
            .collect();
        assert_eq!(requests[0].host, "m3");
        assert_eq!(requests[1].host, "m22b");
        assert!(requests.iter().all(|request| request.time.is_some()));
        assert!(requests.iter().all(|request| request.lan));

        for bad in [
            "nonsense".to_owned(),
            join_line(3, "M22"),
            format!("{} 1", join_line(3, "m22")),
            format!("{} lan=1", join_line(3, "m22")),
            format!("{} 1 lan=1", join_line(3, "m22")),
            format!("{}\nsecond line", join_line(3, "m22")),
            format!("{} m22 {}", key(3), code(4)),
        ] {
            let answer = raw(
                &running,
                format!(
                    "POST /v1/alab/join HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{bad}",
                    bad.len()
                )
                .as_bytes(),
            );
            assert!(answer.starts_with("HTTP/1.1 400 "), "{bad:?}: {answer}");
        }
        assert_eq!(
            transport::list_joins(&url(&running)).expect("listed").len(),
            2
        );
        running.stop();
    }

    #[test]
    fn the_join_queue_is_bounded() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let now = now();
        let mut queue = Vec::new();
        for n in 0..MAX_JOINS {
            let mut line = crate::transport::test_recipient(0);
            line.truncate(60);
            line.push(char::from(b"qpzry9x8gf2tvdw0s3jn54khce6mua7l"[n % 32]));
            line.push(char::from(b"qpzry9x8gf2tvdw0s3jn54khce6mua7l"[n / 32]));
            queue.push(queued(
                JoinRequest {
                    recipient: line,
                    host: format!("h{n}"),
                    code: "abcd-ef23-ghij-k234".to_owned(),
                    time: Some(now),
                    lan: false,
                    allowed: None,
                    proof: None,
                },
                None,
            ));
        }
        running
            .shared
            .lock_server_state()
            .joins
            .insert("alab".to_owned(), queue);
        let error = transport::post_join(&url(&running), &join_line(1, "m22")).expect_err("full");
        assert!(error.0.contains("429"), "{error}");
        assert!(error.0.contains("already holds 256"), "{error}");

        put(&running, &envelope(&OLD, 2)).expect("a new version");
        transport::post_join(&url(&running), &join_line(1, "m22")).expect("room made");
        let state = running.shared.lock_server_state();
        let queue = &state.joins["alab"];
        assert_eq!(queue.len(), MAX_JOINS);
        assert_eq!(
            queue[0].request.host, "h1",
            "h0, the longest queued, gave way"
        );
        assert_eq!(queue[MAX_JOINS - 1].request.host, "m22");
        drop(state);
        running.stop();
    }

    #[test]
    fn the_server_restarts_from_its_store_and_expires_old_joins() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 4)).expect("first");
        let line = join_line(1, "m22");
        transport::post_join(&url(&running), &line).expect("queued");
        running.stop();

        let old = format!(
            "{} m9 abcd-ef24-ghij-k234 {}\n",
            crate::transport::test_recipient(2),
            now() - JOIN_TTL_SECS - 86_400
        );
        let joins = root.path().join("alab.joins");
        let mut text = std::fs::read_to_string(&joins).expect("joins");
        text.push_str(&old);
        fsx::write_atomic(&joins, text.as_bytes(), 0o600).expect("write");

        let running = run(&options(root.path(), Duration::from_millis(200)));
        assert_eq!(
            get(&running).expect("held"),
            Fetched::Body(envelope(&OLD, 4))
        );
        let listed = transport::list_joins(&url(&running)).expect("listed");
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert!(listed[0].starts_with(&line), "{listed:?}");
        assert!(put(&running, &envelope(&OLD, 3)).is_err());
        assert!(put(&running, &envelope(&STRANGER, 5)).is_err());
        put(&running, &envelope(&OLD, 5)).expect("newer");
        running.stop();
    }

    #[test]
    fn a_vaults_first_version_drops_the_queue_an_earlier_one_left() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 4)).expect("first");
        transport::post_join(&url(&running), &join_line(1, "m22")).expect("queued");
        running.stop();
        // The vault's file goes by hand; the queue it left stays in the store.
        std::fs::remove_file(root.path().join("alab.vault")).expect("the vault's file");

        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 1)).expect("a first version again");
        let listed = transport::list_joins(&url(&running)).expect("listed");
        assert!(listed.is_empty(), "{listed:?}");
        running.stop();
        assert!(
            !root.path().join("alab.joins").exists(),
            "the store keeps no old queue"
        );
        let running = run(&options(root.path(), Duration::from_millis(200)));
        let listed = transport::list_joins(&url(&running)).expect("listed");
        assert!(listed.is_empty(), "a restart reads none back: {listed:?}");
        running.stop();
    }

    #[test]
    fn oversize_heads_and_bodies_are_refused() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let mut huge = b"GET /v1/alab HTTP/1.1\r\nX-Pad: ".to_vec();
        huge.resize(MAX_HEAD + 1024, b'a');
        let answer = raw(&running, &huge);
        assert!(answer.starts_with("HTTP/1.1 431 "), "{answer}");

        let answer = raw(
            &running,
            format!(
                "PUT /v1/alab HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n",
                MAX_ENVELOPE + 1
            )
            .as_bytes(),
        );
        assert!(answer.starts_with("HTTP/1.1 413 "), "{answer}");
        let answer = raw(
            &running,
            b"POST /v1/alab/join HTTP/1.1\r\nHost: x\r\nContent-Length: 4096\r\n\r\n",
        );
        assert!(answer.starts_with("HTTP/1.1 413 "), "{answer}");
        let answer = raw(&running, b"PUT /v1/alab HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(answer.starts_with("HTTP/1.1 411 "), "{answer}");
        let answer = raw(
            &running,
            b"PUT /v1/alab HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
        );
        assert!(answer.starts_with("HTTP/1.1 501 "), "{answer}");
        assert_eq!(
            stored(root.path()).as_deref(),
            Some(envelope(&OLD, 1).as_slice())
        );
        running.stop();
    }

    #[test]
    fn an_expect_continue_put_is_answered_100_then_204() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        let body = envelope(&OLD, 1);
        let mut stream = TcpStream::connect(running.addr).expect("connect");
        stream
            .set_read_timeout(Some(HANG_DETECTOR))
            .expect("timeout");
        stream
            .write_all(
                format!(
                    "PUT /v1/alab HTTP/1.1\r\nHost: x\r\nExpect: 100-continue\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .expect("head");
        let mut interim = [0u8; 25];
        stream.read_exact(&mut interim).expect("100");
        assert_eq!(&interim, b"HTTP/1.1 100 Continue\r\n\r\n");
        stream.write_all(&body).expect("body");
        let mut answer = String::new();
        let _ = stream.read_to_string(&mut answer);
        assert!(answer.starts_with("HTTP/1.1 204 "), "{answer}");
        assert!(
            !answer.to_ascii_lowercase().contains("content-length"),
            "{answer}"
        );
        running.stop();
    }

    #[test]
    fn bad_paths_methods_and_queries_are_refused() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 1)).expect("first");
        for (request, status) in [
            ("GET /v1/../alab.vault HTTP/1.1", "404"),
            ("GET /v1/..%2Falab HTTP/1.1", "404"),
            ("GET /v1/Alab HTTP/1.1", "404"),
            ("GET /v1/alab/ HTTP/1.1", "404"),
            ("GET /v1/alab/join/x HTTP/1.1", "404"),
            ("GET /v2/alab HTTP/1.1", "404"),
            ("GET / HTTP/1.1", "404"),
            ("GET http://x/v1/alab HTTP/1.1", "400"),
            ("GET /v1/alab?after=x HTTP/1.1", "400"),
            ("GET /v1/alab?after=-1 HTTP/1.1", "400"),
            ("GET /v1/alab?before=1 HTTP/1.1", "400"),
            ("GET /v1/alab/join?x=1 HTTP/1.1", "400"),
            ("DELETE /v1/alab HTTP/1.1", "405"),
            ("POST /v1/alab HTTP/1.1", "405"),
            ("PUT /v1/alab/join HTTP/1.1", "405"),
            ("HEAD /v1/alab HTTP/1.1", "405"),
            ("GET /v1/alab HTTP/2.0", "505"),
            ("get /v1/alab HTTP/1.1", "400"),
            ("GET  /v1/alab HTTP/1.1", "400"),
        ] {
            let answer = raw(&running, format!("{request}\r\nHost: x\r\n\r\n").as_bytes());
            assert!(
                answer.starts_with(&format!("HTTP/1.1 {status} ")),
                "{request}: {answer}"
            );
            assert!(answer.contains("Connection: close\r\n"), "{answer}");
        }
        let answer = raw(&running, b"DELETE /v1/alab HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(answer.contains("Allow: GET, PUT\r\n"), "{answer}");
        let answer = raw(
            &running,
            b"GET /v1/alab HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\nhi",
        );
        assert!(answer.starts_with("HTTP/1.1 400 "), "{answer}");
        let answer = raw(&running, b"GET /v1/alab HTTP/1.1\r\n folded\r\n\r\n");
        assert!(answer.starts_with("HTTP/1.1 400 "), "{answer}");
        assert_eq!(
            get(&running).expect("held"),
            Fetched::Body(envelope(&OLD, 1))
        );
        running.stop();
    }

    #[test]
    fn the_children_argv_is_exact() {
        let ip = Ipv4Addr::new(192, 168, 1, 22);
        let empty = dns_sd_args("alab-secrets", 7484, ip, None);
        assert_eq!(
            empty,
            [
                "-P",
                "alab-secrets",
                "_alab-vault._tcp",
                "local",
                "7484",
                "alab-secrets.local",
                "192.168.1.22",
                "path=/v1"
            ]
        );
        let raw = envelope(&OLD, 79);
        let parsed = crate::envelope::parse(&raw).expect("envelope");
        let txt = VaultTxt {
            vault: "alab".to_owned(),
            signer: crate::envelope::fingerprint(&parsed.signer),
            seq: parsed.seq,
            head: parsed.digest(),
            label: None,
        };
        let with_txt = dns_sd_args("alab-secrets", 7484, ip, Some(&txt));
        let head_hex = hex32_encode(&txt.head);
        assert_eq!(
            with_txt,
            [
                "-P",
                "alab-secrets",
                "_alab-vault._tcp",
                "local",
                "7484",
                "alab-secrets.local",
                "192.168.1.22",
                "path=/v1",
                "vault=alab",
                &format!("signer={}", txt.signer),
                "seq=79",
                &format!("head={head_hex}"),
            ]
        );
        assert!(
            with_txt[7..].iter().all(|pair| pair.len() <= 255),
            "every TXT string fits RFC 6763's 255-byte limit"
        );
        assert_eq!(caffeinate_args(4242), ["-i", "-w", "4242"]);
        assert_eq!(
            wake_addr("0.0.0.0:7484".parse().expect("addr")),
            "127.0.0.1:7484".parse().expect("addr")
        );
        assert_eq!(
            wake_addr("[::]:7484".parse().expect("addr")),
            "[::1]:7484".parse().expect("addr")
        );
        assert_eq!(
            wake_addr("10.0.0.2:7484".parse().expect("addr")),
            "10.0.0.2:7484".parse().expect("addr")
        );
    }

    #[test]
    fn vault_txt_round_trips_and_rejects_old_or_malformed_args() {
        let ip = Ipv4Addr::new(192, 168, 1, 22);
        let raw = envelope(&OLD, 79);
        let parsed = crate::envelope::parse(&raw).expect("envelope");
        let signer_public = parsed.signer;
        let txt = VaultTxt {
            vault: "alab".to_owned(),
            signer: crate::envelope::fingerprint(&signer_public),
            seq: 79,
            head: parsed.digest(),
            label: None,
        };
        let args = dns_sd_args("alab-secrets", 7484, ip, Some(&txt));
        assert_eq!(
            VaultTxt::from_txt(args[7..].iter().map(String::as_str)),
            Some(txt.clone())
        );
        let resolve_line = format!("  {} extra=ignored seq=999", args[7..].join(" "));
        assert_eq!(
            VaultTxt::from_resolve_line(&resolve_line),
            Some(txt.clone()),
            "RFC 6763: first occurrence of a key wins and unknown keys are ignored"
        );

        // Negative control: old 0.145 args carry only `path=/v1` and never parse as a `VaultTxt`.
        let old_args = dns_sd_args("alab-secrets", 7484, ip, None);
        assert_eq!(
            VaultTxt::from_txt(old_args[7..].iter().map(String::as_str)),
            None
        );

        let head_hex = hex32_encode(&txt.head);
        for malformed in [
            vec![
                "vault=alab".to_owned(),
                format!("signer={}", txt.signer),
                "seq=79".to_owned(),
                format!("head={head_hex}"),
            ],
            vec![
                "path=/v2".to_owned(),
                "vault=alab".to_owned(),
                format!("signer={}", txt.signer),
                "seq=79".to_owned(),
                format!("head={head_hex}"),
            ],
            vec![
                "path=/v1".to_owned(),
                "vault=Bad_Name".to_owned(),
                format!("signer={}", txt.signer),
                "seq=79".to_owned(),
                format!("head={head_hex}"),
            ],
            vec![
                "path=/v1".to_owned(),
                "vault=alab".to_owned(),
                format!("signer={}", txt.signer.to_ascii_uppercase()),
                "seq=79".to_owned(),
                format!("head={head_hex}"),
            ],
            vec![
                "path=/v1".to_owned(),
                "vault=alab".to_owned(),
                format!("signer={}", txt.signer),
                "seq=0".to_owned(),
                format!("head={head_hex}"),
            ],
            vec![
                "path=/v1".to_owned(),
                "vault=alab".to_owned(),
                format!("signer={}", txt.signer),
                "seq=079".to_owned(),
                format!("head={head_hex}"),
            ],
            vec![
                "path=/v1".to_owned(),
                "vault=alab".to_owned(),
                format!("signer={}", txt.signer),
                "seq=79".to_owned(),
                format!("head={}", &head_hex[..63]),
            ],
        ] {
            assert_eq!(
                VaultTxt::from_txt(malformed.iter().map(String::as_str)),
                None,
                "{malformed:?}"
            );
        }

        let mut state = State::default();
        assert_eq!(advertised_vault(&state, "alab-secrets", None, &[]), None);
        state.vaults.insert(
            "alab".to_owned(),
            Slot::new(Held {
                seq: 79,
                signer: signer_public,
                head: txt.head,
                parented: false,
                handoff: Vec::new(),
                bytes: Arc::new(raw),
            }),
        );
        assert_eq!(
            advertised_vault(&state, "alab-secrets", None, &[]),
            Some(txt.clone())
        );
        assert_eq!(
            advertised_vault(&state, "m22", None, &[]),
            Some(txt.clone())
        );
        let mut labelled_txt = txt;
        labelled_txt.label = Some("m22".to_owned());
        assert_eq!(
            advertised_vault(&state, "alab-secrets", Some("m22"), &[]),
            Some(labelled_txt.clone())
        );
        let labelled_args = dns_sd_args("alab-secrets", 7484, ip, Some(&labelled_txt));
        assert_eq!(
            VaultTxt::from_txt(labelled_args[7..].iter().map(String::as_str)),
            Some(labelled_txt)
        );
    }

    #[test]
    fn expiry_keeps_a_week_either_side_of_the_clock() {
        let request = |time| {
            queued(
                JoinRequest {
                    recipient: crate::transport::test_recipient(1),
                    host: "m22".to_owned(),
                    code: "abcd-ef23-ghij-k234".to_owned(),
                    time: Some(time),
                    lan: false,
                    allowed: None,
                    proof: None,
                },
                None,
            )
        };
        let now = 10 * JOIN_TTL_SECS;
        let mut queue = vec![
            request(now),
            request(now - JOIN_TTL_SECS),
            request(now - JOIN_TTL_SECS - 1),
            request(now + 2 * 86_400),
            request(now + JOIN_TTL_SECS),
            request(now + JOIN_TTL_SECS + 1),
        ];
        expire(&mut queue, now);
        let times: Vec<Option<u64>> = queue.iter().map(|queued| queued.request.time).collect();
        assert_eq!(
            times,
            [
                now,
                now - JOIN_TTL_SECS,
                now + 2 * 86_400,
                now + JOIN_TTL_SECS
            ]
            .map(Some)
        );
    }

    #[test]
    fn one_address_holds_at_most_16_connections_and_the_next_reads_503() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let tuning = Tuning {
            liveness_every: Duration::from_millis(10),
            ..Tuning::default()
        };
        let running = run_tuned(&options(root.path(), 2 * HANG_DETECTOR), tuning);
        put(&running, &envelope(&OLD, 1)).expect("first");
        until_closed(&running);
        let poll = b"GET /v1/alab?after=1 HTTP/1.1\r\nHost: x\r\n\r\n";
        let mut held: Vec<TcpStream> = (0..MAX_PEER_CONNECTIONS)
            .map(|_| sent(&running, poll))
            .collect();
        until_waiting(&running, MAX_PEER_CONNECTIONS);

        let answer = raw(&running, b"GET /v1/alab HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(answer.starts_with("HTTP/1.1 503 "), "{answer}");
        assert!(
            answer.contains("too many connections from your address"),
            "{answer}"
        );
        assert!(answer.contains("Connection: close\r\n"), "{answer}");

        drop(held.pop());
        until_waiting(&running, MAX_PEER_CONNECTIONS - 1);
        until("the departed client's slot is released", || {
            running
                .shared
                .lock_peers()
                .get(&IpAddr::V4(Ipv4Addr::LOCALHOST))
                .copied()
                == Some(MAX_PEER_CONNECTIONS - 1)
        });
        assert_eq!(
            get(&running).expect("served"),
            Fetched::Body(envelope(&OLD, 1))
        );
        running.stop();
        for stream in held {
            let answer = read_all(stream);
            assert!(answer.starts_with("HTTP/1.1 204 "), "{answer}");
        }
    }

    #[test]
    fn at_most_4_put_bodies_are_read_at_once() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let tuning = Tuning {
            body_deadline: 2 * HANG_DETECTOR,
            body_stall: 2 * HANG_DETECTOR,
            ..Tuning::default()
        };
        let running = run_tuned(&options(root.path(), Duration::from_millis(200)), tuning);
        let body = envelope(&OLD, 1);
        let head = format!(
            "PUT /v1/alab HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let mut partial = head.clone().into_bytes();
        partial.extend_from_slice(&body[..10]);
        let stalled: Vec<TcpStream> = (0..MAX_PUTS).map(|_| sent(&running, &partial)).collect();
        until("four bodies are being read", || {
            reading(&running) == MAX_PUTS
        });

        let mut whole = head.into_bytes();
        whole.extend_from_slice(&body);
        let answer = raw(&running, &whole);
        assert!(answer.starts_with("HTTP/1.1 503 "), "{answer}");
        assert!(answer.contains("4 envelopes"), "{answer}");
        assert_eq!(get(&running).expect("answered"), Fetched::Missing);

        drop(stalled);
        until("the stalled bodies are let go", || reading(&running) == 0);
        put(&running, &body).expect("read now");
        running.stop();
    }

    /// Whether [`toggled`] says a peer is this machine's.
    static LOCAL: AtomicBool = AtomicBool::new(false);

    /// An `is_local` a test flips.
    fn toggled(_: IpAddr, _: Option<IpAddr>) -> bool {
        LOCAL.load(Ordering::SeqCst)
    }

    #[test]
    fn another_machine_reads_one_put_at_a_time_and_the_last_slot_is_this_machines() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let tuning = Tuning {
            body_deadline: 2 * HANG_DETECTOR,
            body_stall: 2 * HANG_DETECTOR,
            is_local: toggled,
            ..Tuning::default()
        };
        let mut options = options(root.path(), Duration::from_millis(200));
        options.pins = vec![("alab".to_owned(), signer_public(&OLD).expect("public"))];
        let running = run_tuned(&options, tuning);
        let body = envelope(&OLD, 1);
        let head = format!(
            "PUT /v1/alab HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let mut partial = head.clone().into_bytes();
        partial.extend_from_slice(&body[..10]);
        let mut whole = head.into_bytes();
        whole.extend_from_slice(&body);

        LOCAL.store(false, Ordering::SeqCst);
        let mut stalled = vec![sent(&running, &partial)];
        until("its body is being read", || reading(&running) == 1);
        let answer = raw(&running, &whole);
        assert!(answer.starts_with("HTTP/1.1 503 "), "{answer}");
        assert!(answer.contains("your address is sending"), "{answer}");

        LOCAL.store(true, Ordering::SeqCst);
        stalled.extend((0..2).map(|_| sent(&running, &partial)));
        until("three bodies are being read", || reading(&running) == 3);
        LOCAL.store(false, Ordering::SeqCst);
        let answer = raw(&running, &whole);
        assert!(answer.starts_with("HTTP/1.1 503 "), "{answer}");
        assert!(answer.contains("from other machines"), "{answer}");
        LOCAL.store(true, Ordering::SeqCst);
        put(&running, &body).expect("this machine's PUT takes the last slot");

        drop(stalled);
        until("the stalled bodies are let go", || reading(&running) == 0);
        LOCAL.store(false, Ordering::SeqCst);
        put(&running, &envelope(&OLD, 2)).expect("another machine's, one at a time");
        assert!(running.shared.lock_puts().by_peer.is_empty());
        running.stop();
    }

    #[test]
    fn a_put_body_that_stalls_or_outruns_its_deadline_is_cut_with_408_and_frees_its_slot() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let stall = Tuning {
            body_stall: Duration::from_millis(100),
            body_deadline: 2 * HANG_DETECTOR,
            ..Tuning::default()
        };
        let deadline = Tuning {
            body_stall: 2 * HANG_DETECTOR,
            body_deadline: Duration::from_millis(200),
            linger: Duration::from_millis(100),
            ..Tuning::default()
        };
        let head = format!(
            "PUT /v1/alab HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\nabc",
            1 << 20
        );
        for (tuning, trickled, why) in [
            (stall, false, "the body stalled"),
            (deadline, true, "the body took too long"),
        ] {
            let running = run_tuned(&options(root.path(), Duration::from_millis(200)), tuning);
            let stream = sent(&running, head.as_bytes());
            let mut writer = stream.try_clone().expect("clone");
            let trickle = std::thread::spawn(move || {
                while trickled && writer.write_all(b"a").is_ok() {
                    std::thread::sleep(Duration::from_millis(5));
                }
            });
            let answer = read_all(stream);
            assert!(answer.starts_with("HTTP/1.1 408 "), "{why}: {answer}");
            assert!(answer.contains(why), "{answer}");
            trickle.join().expect("trickle");
            until("the PUT slot is let go", || reading(&running) == 0);
            put(&running, &envelope(&OLD, 1)).expect("the slot serves the next PUT");
            running.stop();
        }
    }

    #[test]
    fn a_join_asked_again_keeps_its_place_moves_its_time_hourly_and_a_new_proof_replaces_it() {
        use std::os::unix::fs::MetadataExt;
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run_tuned(
            &options(root.path(), Duration::from_millis(200)),
            changeable(),
        );
        put(&running, &envelope(&OLD, 1)).expect("first");
        let proof = |n: u8| crate::b64::encode_padded(&[n; 32]);
        let line = format!("{} proof={}", join_line(1, "m22"), proof(1));
        transport::post_join(&url(&running), &line).expect("queued");
        transport::post_join(&url(&running), &join_line(2, "m2")).expect("one behind it");
        let joins = root.path().join("alab.joins");
        let inode = |path: &Path| std::fs::metadata(path).expect("joins").ino();
        let first = inode(&joins);
        let queued_at = |time: u64| {
            running
                .shared
                .lock_server_state()
                .joins
                .get_mut("alab")
                .expect("queue")[0]
                .request
                .time = Some(time);
        };
        let listed = || transport::list_joins(&url(&running)).expect("listed");
        let hosts = |listed: &[String]| {
            listed
                .iter()
                .map(|line| parse_join_line(line).expect("parses").host)
                .collect::<Vec<_>>()
        };

        let recent = now() - (JOIN_REFRESH_EVERY_SECS - 60);
        queued_at(recent);
        transport::post_join(&url(&running), &line).expect("asked again");
        assert_eq!(
            listed()[0],
            format!("{} {recent} lan=1 proof={}", join_line(1, "m22"), proof(1))
        );
        assert_eq!(inode(&joins), first, "the store was rewritten");

        let earlier = now() - JOIN_REFRESH_EVERY_SECS;
        queued_at(earlier);
        let set_peer = |peer: Option<IpAddr>| {
            running
                .shared
                .lock_server_state()
                .joins
                .get_mut("alab")
                .expect("queue")[0]
                .peer = peer;
        };
        let here = running.shared.lock_server_state().joins["alab"][0].peer;
        let there: IpAddr = "192.0.2.7".parse().expect("address");
        set_peer(Some(there));
        transport::post_join(&url(&running), &line).expect("asked again, an hour on");
        let now_listed = listed();
        assert_eq!(hosts(&now_listed), ["m22", "m2"]);
        let request = parse_join_line(&now_listed[0]).expect("parses");
        assert_eq!(request.proof, Some(proof(1)));
        assert!(
            request.time.is_some_and(|time| time > earlier),
            "{request:?}"
        );
        assert_ne!(inode(&joins), first, "the moved time was not stored");
        let stored = std::fs::read_to_string(&joins).expect("store");
        assert!(
            stored.starts_with(&format!("peer {there} 1\n{}\n", now_listed[0])),
            "{stored}"
        );
        let moved = inode(&joins);
        set_peer(here);

        transport::post_join(
            &url(&running),
            &format!("{} proof={}", join_line(1, "m22"), proof(2)),
        )
        .expect("a new proof");
        let now_listed = listed();
        assert_eq!(hosts(&now_listed), ["m2", "m22"]);
        let request = parse_join_line(&now_listed[1]).expect("parses");
        assert_eq!(request.proof, Some(proof(2)));
        assert_ne!(inode(&joins), moved);
        transport::post_join(&url(&running), &join_line(1, "m22b")).expect("a new host");
        let now_listed = listed();
        assert_eq!(hosts(&now_listed), ["m2", "m22b"]);
        let request = parse_join_line(&now_listed[1]).expect("parses");
        assert_eq!(request.proof, None);
        running.stop();
    }

    #[test]
    fn another_address_can_neither_replace_nor_hide_a_queued_join_request() {
        use std::sync::atomic::AtomicU64;
        const START: u64 = 1_800_000_000;
        static CLOCK: AtomicU64 = AtomicU64::new(START);
        fn clock() -> u64 {
            CLOCK.load(Ordering::SeqCst)
        }
        let gap = JOIN_CHANGE_GAP.as_secs();
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run_tuned(
            &options(root.path(), Duration::from_millis(200)),
            Tuning {
                now: clock,
                ..Tuning::default()
            },
        );
        put(&running, &envelope(&OLD, 1)).expect("first");
        let machine: IpAddr = "192.0.2.10".parse().expect("address");
        let stranger: IpAddr = "192.0.2.66".parse().expect("address");
        let ask = |line: &str, from: IpAddr| {
            post_join(&running.shared, "alab", line.as_bytes(), from).status
        };
        let proof = |n: u8| crate::b64::encode_padded(&[n; 32]);
        let key = crate::transport::test_recipient(1);
        let asked = format!("{} proof={}", join_line(1, "m30"), proof(1));
        assert_eq!(ask(&asked, machine), 204);

        assert_eq!(ask(&format!("{key} m30 {}", code(2)), stranger), 400);
        CLOCK.store(START + gap, Ordering::SeqCst);
        assert_eq!(
            ask(
                &format!("{} proof={}", join_line(1, "evil"), proof(2)),
                stranger
            ),
            204
        );
        let listed = transport::list_joins(&url(&running)).expect("listed");
        assert_eq!(listed.len(), 2, "{listed:?}");
        assert_eq!(
            listed[0],
            format!("{} {START} proof={}", join_line(1, "m30"), proof(1)),
            "the machine's request stands as it asked it"
        );

        CLOCK.store(START + 2 * gap, Ordering::SeqCst);
        assert_eq!(
            ask(
                &format!("{} proof={}", join_line(1, "evil"), proof(3)),
                stranger
            ),
            204
        );
        assert_eq!(
            ask(
                &format!("{} proof={}", join_line(1, "m30"), proof(4)),
                machine
            ),
            204
        );
        let moved: IpAddr = "192.0.2.11".parse().expect("address");
        assert_eq!(
            ask(
                &format!("{} proof={}", join_line(1, "m30"), proof(4)),
                moved
            ),
            204
        );
        let state = running.shared.lock_server_state();
        let mine: Vec<&Queued> = state.joins["alab"]
            .iter()
            .filter(|queued| queued.request.host == "m30")
            .collect();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].peer, Some(machine));
        assert_eq!(mine[0].request.proof, Some(proof(4)));
        drop(state);

        let hour = START + 2 * gap + JOIN_REFRESH_EVERY_SECS;
        CLOCK.store(hour, Ordering::SeqCst);
        let asked = format!("{} proof={}", join_line(1, "m30"), proof(4));
        assert_eq!(ask(&asked, stranger), 204);
        CLOCK.store(hour + 1, Ordering::SeqCst);
        let asked = format!("{} proof={}", join_line(1, "m30"), proof(5));
        assert_eq!(ask(&asked, machine), 204);

        let mut at = hour + 1;
        while at < hour + 1 + JOIN_TTL_SECS + 2 * JOIN_REFRESH_EVERY_SECS {
            at += JOIN_REFRESH_EVERY_SECS;
            CLOCK.store(at, Ordering::SeqCst);
            assert_eq!(ask(&asked, moved), 204);
            let listed = transport::list_joins(&url(&running)).expect("listed");
            let refreshed = format!("{} {at} proof={}", join_line(1, "m30"), proof(5));
            assert!(listed.contains(&refreshed), "{listed:?}");
        }
        let state = running.shared.lock_server_state();
        assert_eq!(state.joins["alab"].len(), 1);
        assert_eq!(state.joins["alab"][0].peer, Some(machine));
        drop(state);
        running.stop();
    }

    #[test]
    fn a_key_two_addresses_ask_for_is_let_in_once() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(
            &root.path().join("store"),
            Duration::from_millis(200),
        ));
        let vault = format!("http://{}/v1/twice", running.addr);
        let admin = crate::client::Paths::for_home(&root.path().join("ma"));
        crate::client::create(&admin, &vault, None, Some("ma")).expect("create");
        let opened = crate::client::admin_open(&admin).expect("open");
        let mut doc = opened.doc.clone();
        doc.set_open(true).expect("open");
        crate::client::admin_publish(&admin, &opened, &doc).expect("publish");
        let key = crate::transport::test_recipient(1);
        // A member Allowed the Mac as `mb`; the copy a second address sends under another name
        // carries the same Allow, which names `mb`.
        let member = crate::commons::bootstrap::Bootstrap::cached(&admin)
            .expect("state")
            .expect("Commons");
        let trust = member.trust();
        let request = crate::commons::binding::Request::create(
            trust.network,
            trust.authority,
            [5; 16],
            [6; 16],
            &crate::age::Identity::from_secret_bytes([2; 32]),
            &[7; 32],
        )
        .expect("request");
        assert_eq!(request.binding().recipient().to_bech32(), key);
        let allowed = member
            .device()
            .with_seed(|seed| crate::members::Allowed::sign(request.binding(), "mb", seed))
            .expect("signed")
            .to_text();
        for (host, from) in [("mb", "192.0.2.10"), ("evil", "192.0.2.66")] {
            let line = format!("{key} {host} {} allowed={allowed}", code(1));
            let from = from.parse().expect("address");
            assert_eq!(
                post_join(&running.shared, "twice", line.as_bytes(), from).status,
                204
            );
        }
        let done = crate::client::auto_approve(&admin).expect("approve");
        assert_eq!(done.approved, ["mb"]);
        let doc = crate::client::admin_open(&admin).expect("open").doc.clone();
        let names: Vec<String> = doc
            .machines()
            .into_iter()
            .filter(|machine| machine.recipient == key)
            .map(|machine| machine.name)
            .collect();
        assert_eq!(names, ["mb"]);
        running.stop();
    }

    #[test]
    fn one_address_queues_at_most_4_joins_and_the_listing_never_names_it() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let joins = root.path().join("alab.joins");
        let running = run_tuned(
            &options(root.path(), Duration::from_millis(200)),
            changeable(),
        );
        put(&running, &envelope(&OLD, 1)).expect("first");
        for n in 1..=4u8 {
            transport::post_join(&url(&running), &join_line(n, &format!("m{n}"))).expect("queued");
        }
        let error = transport::post_join(&url(&running), &join_line(5, "m5")).expect_err("a fifth");
        assert!(error.0.contains("429"), "{error}");
        assert!(error.0.contains("your address"), "{error}");
        transport::post_join(&url(&running), &join_line(4, "m4b")).expect("replaced");

        let listed = transport::list_joins(&url(&running)).expect("listed");
        assert_eq!(listed.len(), 4, "{listed:?}");
        for line in &listed {
            assert_eq!(parse_join_line(line).expect("parses").listed(), *line);
            assert!(!line.contains("127.0.0.1"), "{line}");
        }
        let stored = std::fs::read_to_string(&joins).expect("joins");
        assert_eq!(stored.matches("peer 127.0.0.1 1\n").count(), 4, "{stored}");
        running.stop();

        let running = run(&options(root.path(), Duration::from_millis(200)));
        let error =
            transport::post_join(&url(&running), &join_line(5, "m5")).expect_err("still a fifth");
        assert!(error.0.contains("429"), "{error}");
        running.stop();

        let old: String = stored
            .lines()
            .filter(|line| !line.starts_with("peer "))
            .map(|line| format!("{line}\n"))
            .collect();
        fsx::write_atomic(&joins, old.as_bytes(), 0o600).expect("write");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        assert_eq!(
            transport::list_joins(&url(&running)).expect("listed").len(),
            4
        );
        transport::post_join(&url(&running), &join_line(5, "m5")).expect("counted against no one");
        running.stop();
    }

    #[test]
    fn a_publish_answers_the_queued_joins_and_one_still_waiting_counts_when_it_asks_again() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let ask = |n: u8| transport::post_join(&url(&running), &join_line(n, &format!("m{n}")));
        for n in 1..=4 {
            ask(n).expect("queued");
        }
        assert!(ask(5).expect_err("a fifth").0.contains("429"));

        put(&running, &envelope(&OLD, 2)).expect("a new version");
        ask(5).expect("m1 gave way");
        ask(3).expect("asked again");
        ask(4).expect("asked again");
        ask(6).expect("m2 gave way");
        let error = ask(7).expect_err("a fifth counted");
        assert!(error.0.contains("your address"), "{error}");
        let hosts: Vec<String> = transport::list_joins(&url(&running))
            .expect("listed")
            .iter()
            .map(|line| parse_join_line(line).expect("parses").host)
            .collect();
        assert_eq!(hosts, ["m3", "m4", "m5", "m6"]);
        running.stop();
    }

    #[test]
    fn a_key_changes_its_request_at_most_every_10_s_and_the_log_notes_it_hourly() {
        use std::sync::atomic::AtomicU64;
        const START: u64 = 1_800_000_000;
        static CLOCK: AtomicU64 = AtomicU64::new(START);
        fn clock() -> u64 {
            CLOCK.load(Ordering::SeqCst)
        }
        let at = |secs: u64| CLOCK.store(START + secs, Ordering::SeqCst);
        let gap = JOIN_CHANGE_GAP.as_secs();
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let joins = root.path().join("alab.joins");
        let running = run_tuned(
            &options(root.path(), Duration::from_millis(200)),
            Tuning {
                now: clock,
                ..Tuning::default()
            },
        );
        put(&running, &envelope(&OLD, 1)).expect("first");
        transport::post_join(&url(&running), &join_line(1, "m22")).expect("queued");
        let first = std::fs::read(&joins).expect("joins");
        let held = |running: &Running| running.shared.lock_server_state().joins["alab"][0].clone();
        assert_eq!(
            held(&running).noted,
            START,
            "a new request is noted as it is queued"
        );
        assert_eq!(held(&running).request.time, Some(START));

        // A second short of the gap, a change is refused with that second as its Retry-After,
        // which the client sits out once; on this clock, which has not moved, its retry is
        // refused too.
        at(gap - 1);
        let paused = std::cell::RefCell::new(Vec::new());
        let error = transport::post_join_pausing(&url(&running), &join_line(1, "m22b"), &|wait| {
            paused.borrow_mut().push(wait)
        })
        .expect_err("changed too soon");
        assert_eq!(*paused.borrow(), [Duration::from_secs(1)]);
        assert!(error.0.contains("429"), "{error}");
        assert!(error.0.contains("changed less than 10s ago"), "{error}");
        assert_eq!(std::fs::read(&joins).expect("joins"), first);
        assert_eq!(held(&running).request.host, "m22");
        assert_eq!(held(&running).request.time, Some(START));

        // The same change, its pause letting the clock reach the gap: the retry is taken.
        paused.borrow_mut().clear();
        transport::post_join_pausing(&url(&running), &join_line(1, "m22b"), &|wait| {
            paused.borrow_mut().push(wait);
            at(gap);
        })
        .expect("changed after its pause");
        assert_eq!(*paused.borrow(), [Duration::from_secs(1)]);
        assert_eq!(held(&running).request.host, "m22b");
        assert_eq!(held(&running).request.time, Some(START + gap));
        assert_eq!(held(&running).noted, START, "noted again within the hour");
        assert_ne!(std::fs::read(&joins).expect("joins"), first);
        at(JOIN_NOTE_EVERY_SECS - gap);
        transport::post_join(&url(&running), &join_line(1, "m22c")).expect("changed");
        assert_eq!(held(&running).request.host, "m22c");
        assert_eq!(held(&running).noted, START, "noted again within the hour");
        at(JOIN_NOTE_EVERY_SECS);
        transport::post_join(&url(&running), &join_line(1, "m22d")).expect("changed");
        assert_eq!(held(&running).request.host, "m22d");
        assert_eq!(
            held(&running).noted,
            START + JOIN_NOTE_EVERY_SECS,
            "an hour on, the change is noted"
        );
        running.stop();
    }

    #[test]
    fn a_clock_stepped_back_holds_off_no_change_and_drops_no_request() {
        use std::sync::atomic::AtomicU64;
        const START: u64 = 1_800_000_000;
        static CLOCK: AtomicU64 = AtomicU64::new(START);
        fn clock() -> u64 {
            CLOCK.load(Ordering::SeqCst)
        }
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run_tuned(
            &options(root.path(), Duration::from_millis(200)),
            Tuning {
                now: clock,
                ..Tuning::default()
            },
        );
        put(&running, &envelope(&OLD, 1)).expect("first");
        transport::post_join(&url(&running), &join_line(1, "m22")).expect("queued");
        transport::post_join(&url(&running), &join_line(2, "m23")).expect("queued");

        let back = START - 6 * 60 * 60;
        CLOCK.store(back, Ordering::SeqCst);
        transport::post_join(&url(&running), &join_line(1, "m22b")).expect("changed at once");
        let held = |running: &Running| running.shared.lock_server_state().joins["alab"].clone();
        let hosts: Vec<(String, Option<u64>)> = held(&running)
            .into_iter()
            .map(|queued| (queued.request.host, queued.request.time))
            .collect();
        assert_eq!(
            hosts,
            [
                ("m23".to_owned(), Some(START)),
                ("m22b".to_owned(), Some(back))
            ]
        );

        CLOCK.store(back - 2 * 86_400, Ordering::SeqCst);
        assert_eq!(
            transport::list_joins(&url(&running)).expect("listed").len(),
            2
        );
        running.stop();
    }

    #[test]
    fn a_join_waiting_on_its_store_write_holds_up_no_read_or_publish() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let writing = running.shared.join_writes();
        let target = url(&running);
        let poster =
            std::thread::spawn(move || transport::post_join(&target, &join_line(1, "m22")));
        assert_eq!(
            get(&running).expect("served"),
            Fetched::Body(envelope(&OLD, 1))
        );
        put(&running, &envelope(&OLD, 2)).expect("published");
        assert!(
            transport::list_joins(&url(&running))
                .expect("listed")
                .is_empty()
        );
        drop(writing);
        poster.join().expect("poster").expect("queued");
        assert_eq!(
            transport::list_joins(&url(&running)).expect("listed").len(),
            1
        );
        running.stop();
    }

    #[test]
    fn the_join_store_keeps_each_address_where_the_listing_never_shows_it() {
        let request = |n: u8| JoinRequest {
            time: Some(1_792_530_000),
            ..parse_join_line(&join_line(n, &format!("m{n}"))).expect("valid")
        };
        let here: IpAddr = "192.0.2.7".parse().expect("address");
        let there: IpAddr = "2001:db8::7".parse().expect("address");
        let queue = vec![
            queued(request(1), Some(here)),
            queued(request(2), None),
            queued(request(3), Some(there)),
        ];
        let [one, two, three] = [1, 2, 3].map(|n| request(n).listed());
        let text = stored_lines(&queue);
        assert_eq!(
            text,
            format!("peer 192.0.2.7 1\n{one}\n{two}\npeer 2001:db8::7 1\n{three}\n")
        );
        assert_eq!(join_lines(&queue), format!("{one}\n{two}\n{three}\n"));
        let triples = |queue: &[Queued]| {
            queue
                .iter()
                .map(|queued| (queued.request.clone(), queued.peer, queued.asked))
                .collect::<Vec<_>>()
        };
        let read = parse_stored(&text);
        assert_eq!(triples(&read[..1]), triples(&queue[..1]));
        assert_eq!(triples(&read[2..]), triples(&queue[2..]));
        assert_eq!(
            (read[1].request.clone(), read[1].peer, read[1].asked),
            (request(2), None, 0)
        );
        let odd = format!(
            "peer 192.0.2.7\nnot a request\n{one}\npeer nonsense 3\n{two}\npeer 192.0.2.7\n{three}\n"
        );
        let read = parse_stored(&odd);
        assert_eq!(read.len(), 3);
        assert!(read[..2].iter().all(|queued| queued.peer.is_none()));
        assert_eq!((read[2].peer, read[2].asked), (Some(here), 0));
    }

    #[test]
    fn lan_eligibility_comes_only_from_server_observations() {
        for address in [
            "127.0.0.1",
            "127.4.3.2",
            "10.2.3.4",
            "172.16.1.2",
            "172.31.255.254",
            "192.168.1.2",
            "169.254.3.4",
            "::1",
            "fdab::1234",
            "fe80::1234",
            "::ffff:192.168.1.2",
        ] {
            assert!(lan_join_peer(address.parse().unwrap()), "{address}");
        }
        for address in [
            "0.0.0.0",
            "100.64.0.1",
            "172.15.255.254",
            "172.32.0.1",
            "192.0.2.7",
            "8.8.8.8",
            "224.0.0.1",
            "::",
            "2001:db8::7",
            "ff02::1",
            "::ffff:8.8.8.8",
        ] {
            assert!(!lan_join_peer(address.parse().unwrap()), "{address}");
        }
        let request = |n| JoinRequest {
            time: Some(1_792_530_000),
            ..parse_join_line(&join_line(n, &format!("m{n}"))).unwrap()
        };
        // These are the pre-upgrade records: the request has no marker, but the server
        // already retained the actual socket peer. No repost or member action is needed.
        let old = vec![
            queued(request(1), Some("192.168.1.2".parse().unwrap())),
            queued(request(2), Some("2001:db8::7".parse().unwrap())),
            queued(request(3), None),
        ];
        let saved = stored_lines(&old);
        assert!(!saved.contains("lan="));
        let restored = parse_stored(&saved);
        let listing = join_lines(&restored);
        assert!(!listing.contains("192.168") && !listing.contains("2001:db8"));
        let flags: Vec<bool> = listing
            .lines()
            .map(|line| parse_join_line(line).unwrap().lan)
            .collect();
        assert_eq!(flags, [true, false, false]);
        let forged = vec![queued(
            JoinRequest {
                lan: true,
                ..request(4)
            },
            Some("192.0.2.7".parse().unwrap()),
        )];
        assert!(!parse_join_line(join_lines(&forged).trim()).unwrap().lan);
    }

    #[test]
    fn moving_onto_the_lan_updates_eligibility_without_replacing_the_first_peer() {
        for first in [Some("192.0.2.7".parse::<IpAddr>().unwrap()), None] {
            let root = aterm_tempfile::tempdir().unwrap();
            let opts = options(root.path(), HANG_DETECTOR);
            let running = run(&opts);
            put(&running, &envelope(&OLD, 1)).unwrap();
            let request = JoinRequest {
                time: Some((running.shared.tuning.now)()),
                ..parse_join_line(&join_line(1, "m1")).unwrap()
            };
            let original = queued(request.clone(), first);
            fsx::write_atomic(
                &running.shared.joins_path("alab"),
                stored_lines(std::slice::from_ref(&original)).as_bytes(),
                0o600,
            )
            .unwrap();
            running
                .shared
                .lock_server_state()
                .joins
                .insert("alab".into(), vec![original]);
            let at = url(&running);
            assert!(
                !parse_join_line(&transport::list_joins(&at).unwrap()[0])
                    .unwrap()
                    .lan
            );
            let generation = running.shared.lock_server_state().vaults["alab"].joins;
            // Real HTTP POST supplies the loopback socket peer, with the identical request.
            transport::post_join(&at, &request.line()).unwrap();
            assert!(
                parse_join_line(&transport::list_joins(&at).unwrap()[0])
                    .unwrap()
                    .lan
            );
            {
                let state = running.shared.lock_server_state();
                let queued = &state.joins["alab"][0];
                assert_eq!(queued.peer, first);
                assert_eq!(queued.request.time, request.time);
                assert!(queued.observed_lan);
                assert!(state.vaults["alab"].joins > generation);
            }
            running.stop();
            let running = run(&opts);
            assert!(
                parse_join_line(&transport::list_joins(&url(&running)).unwrap()[0])
                    .unwrap()
                    .lan
            );
            // A later public-source repeat cannot clear the durable LAN observation.
            assert_eq!(
                post_join(
                    &running.shared,
                    "alab",
                    request.line().as_bytes(),
                    "192.0.2.8".parse().unwrap()
                )
                .status,
                204
            );
            assert!(
                parse_join_line(&transport::list_joins(&url(&running)).unwrap()[0])
                    .unwrap()
                    .lan
            );
            running.stop();
        }
    }

    #[test]
    fn a_first_version_is_taken_only_from_this_machine_or_under_a_pin() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(
            &root.path().join("here"),
            Duration::from_millis(200),
        ));
        put(&running, &envelope(&OLD, 1)).expect("from loopback");
        running.stop();

        let elsewhere = Tuning {
            is_local: |_, _| false,
            ..Tuning::default()
        };
        let store = root.path().join("there");
        let running = run_tuned(&options(&store, Duration::from_millis(200)), elsewhere);
        let error = put(&running, &envelope(&OLD, 1)).expect_err("from another machine");
        assert!(error.0.contains("403"), "{error}");
        assert!(
            error.0.contains(
                "only from the server's own Mac, or under aterm secrets serve --pin <vault>=<key>"
            ),
            "{error}"
        );
        assert!(stored(&store).is_none());
        let answer = raw(
            &running,
            b"PUT /v1/alab HTTP/1.1\r\nHost: x\r\nContent-Length: 1000\r\n\r\n",
        );
        assert!(answer.starts_with("HTTP/1.1 403 "), "{answer}");
        assert_eq!(reading(&running), 0);
        running.stop();

        let mut pinned = options(&store, Duration::from_millis(200));
        pinned.pins = vec![("alab".to_owned(), signer_public(&OLD).expect("public"))];
        let running = run_tuned(&pinned, elsewhere);
        put(&running, &envelope(&OLD, 1)).expect("under the pin");
        put(&running, &envelope(&OLD, 2)).expect("a later version");
        let other = Source::Url(format!("http://{}/v1/other", running.addr));
        let error = transport::publish(&other, &envelope_with(&OLD, 1, Vec::new(), "other"))
            .expect_err("unpinned");
        assert!(error.0.contains("403"), "{error}");
        running.stop();
    }

    /// A version an admin publishes at the store file is served from the next request on, and a
    /// publish built on the server's older copy is refused; an older or foreign file is ignored.
    #[test]
    fn a_version_published_at_the_store_file_is_served_and_kept() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let store = root.path().join("store");
        let tuning = Tuning {
            liveness_every: Duration::from_millis(10),
            ..Tuning::default()
        };
        let running = run_tuned(&options(&store, HANG_DETECTOR), tuning);
        put(&running, &envelope(&OLD, 1)).expect("first");
        let at_path = Source::Path(store.join("alab.vault"));
        transport::publish(&at_path, &envelope(&OLD, 2)).expect("at the path");
        let stale = sign(
            &Unsigned {
                time: 1,
                ..unsigned(2, Vec::new(), "alab")
            },
            &OLD,
        )
        .expect("signed");
        let error = put(&running, &stale).expect_err("built on seq 1");
        assert!(error.0.contains("409"), "{error}");
        assert!(
            stored(&store) == Some(envelope(&OLD, 2)),
            "the path's stays"
        );
        let served = |running: &Running| match get(running).expect("read") {
            Fetched::Body(body) => body,
            other => panic!("{other:?}"),
        };
        transport::publish(&at_path, &envelope(&OLD, 3)).expect("at the path again");
        assert!(served(&running) == envelope(&OLD, 3), "the path's version");

        let polled = source(&running);
        let waiter = std::thread::spawn(move || transport::fetch(&polled, Some(3), HANG_DETECTOR));
        until_waiting(&running, 1);
        transport::publish(&at_path, &envelope(&OLD, 4)).expect("at the path a third time");
        let woke = waiter.join().expect("joined").expect("held");
        assert!(
            matches!(&woke, Fetched::Body(body) if *body == envelope(&OLD, 4)),
            "the held long-poll answers the path's next version"
        );

        // A chain naming the held key with no endorsement of it: a PUT's 403.
        let old = signer_public(&OLD).expect("public");
        let forged = crate::envelope::sign_unchecked(
            &unsigned(5, vec![(old, [0u8; 64])], "alab"),
            &STRANGER,
        )
        .expect("signed");
        // The held key's own version with a byte of what it signed changed, and one of
        // another vault.
        let altered = String::from_utf8(envelope(&OLD, 5))
            .expect("text")
            .replacen("time: 1792530005\n", "time: 1792530006\n", 1)
            .into_bytes();
        let parsed = crate::envelope::parse(&altered).expect("parses");
        assert!(parsed.verify_signature().is_err(), "altered");
        let other_vault = envelope_with(&OLD, 5, Vec::new(), "other");
        for other in [
            forged,
            altered,
            other_vault,
            envelope(&STRANGER, 5),
            envelope(&OLD, 1),
        ] {
            std::fs::write(store.join("alab.vault"), &other).expect("written");
            assert!(served(&running) == envelope(&OLD, 4), "nothing changes");
        }
        running.stop();
    }

    /// A publish at the store path that lands between a PUT's judgment and its lock.
    fn publish_seq_3_at(path: &Path, seq: u64) {
        if seq == 3 {
            transport::publish(&Source::Path(path.to_owned()), &envelope(&OLD, 3))
                .expect("at the path");
        }
    }

    #[test]
    fn a_put_is_judged_again_under_the_publish_lock() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let store = root.path().join("store");
        let tuning = Tuning {
            before_lock: publish_seq_3_at,
            ..Tuning::default()
        };
        let running = run_tuned(&options(&store, HANG_DETECTOR), tuning);
        put(&running, &envelope(&OLD, 1)).expect("first");
        put(&running, &envelope(&OLD, 2)).expect("second");
        let racing = sign(
            &Unsigned {
                time: 1,
                ..unsigned(3, Vec::new(), "alab")
            },
            &OLD,
        )
        .expect("signed");
        let error = put(&running, &racing).expect_err("the path's seq 3 came first");
        assert!(error.0.contains("409"), "{error}");
        assert!(
            stored(&store) == Some(envelope(&OLD, 3)),
            "the path's stays"
        );
        assert!(
            get(&running).expect("read") == Fetched::Body(envelope(&OLD, 3)),
            "and is served"
        );
        running.stop();
    }

    #[test]
    fn a_put_is_refused_while_a_publish_holds_the_lock_and_clears_a_lock_its_dead_holder_left() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let store = root.path().join("store");
        let running = run(&options(&store, HANG_DETECTOR));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let lock = store.join("alab.vault.lock");
        let host = aterm_uds::process::host_name().expect("a host name");
        // This test's own process: a holder that runs.
        std::fs::write(&lock, format!("{host} {}\n", std::process::id())).expect("locked");
        let error = put(&running, &envelope(&OLD, 2)).expect_err("a publish runs");
        assert!(
            error.0.contains("409")
                && error.0.contains(&format!(
                    "a publish holds alab.vault.lock in the server's store; if none is running \
                     there, remove it and publish again ({host} {})",
                    std::process::id()
                )),
            "within what a client quotes: {error}"
        );
        let error = put(&running, &envelope(&STRANGER, 2)).expect_err("a stranger");
        assert!(
            error.0.contains("403") && !error.0.contains("lock"),
            "judged before the lock: {error}"
        );
        assert!(stored(&store) == Some(envelope(&OLD, 1)), "unchanged");

        // No process has pid i32::MAX; a lock naming another machine is never cleared.
        for (holder, cleared) in [
            (format!("elsewhere.invalid {}", i32::MAX), false),
            (format!("{host} {}", i32::MAX), true),
        ] {
            std::fs::write(&lock, format!("{holder}\n")).expect("locked");
            let taken = put(&running, &envelope(&OLD, 2));
            assert_eq!(taken.is_ok(), cleared, "{holder}: {taken:?}");
        }
        assert!(stored(&store) == Some(envelope(&OLD, 2)), "taken");
        assert!(!lock.exists(), "a PUT leaves no lock");

        // A lock written before this boot has no running holder, whatever pid its line names
        // (here a live one) or when its line never reached the disk; never one of another
        // machine, nor an empty one written this boot.
        let before_boot = std::time::UNIX_EPOCH + Duration::from_secs(86_400);
        let mut seq = 2;
        for (holder, written, cleared) in [
            (format!("{host} {}", std::process::id()), before_boot, true),
            (String::new(), before_boot, true),
            (
                format!("elsewhere.invalid {}", i32::MAX),
                before_boot,
                false,
            ),
            (String::new(), std::time::SystemTime::now(), false),
        ] {
            std::fs::write(&lock, &holder).expect("locked");
            std::fs::File::options()
                .write(true)
                .open(&lock)
                .and_then(|file| file.set_modified(written))
                .expect("dated");
            let taken = put(&running, &envelope(&OLD, seq + 1));
            assert_eq!(
                taken.is_ok(),
                cleared,
                "{holder:?} at {written:?}: {taken:?}"
            );
            if cleared {
                seq += 1;
            } else {
                std::fs::remove_file(&lock).expect("unlocked");
            }
        }
        running.stop();
    }

    #[test]
    fn this_machine_is_loopback_and_its_own_addresses() {
        let ip = |text: &str| text.parse::<IpAddr>().expect("address");
        for local in ["127.0.0.1", "127.0.0.9", "::1", "::ffff:127.0.0.1"] {
            assert!(this_machine(ip(local), None), "{local}");
        }
        assert!(this_machine(ip("192.0.2.55"), Some(ip("192.0.2.55"))));
        assert!(this_machine(
            ip("::ffff:192.0.2.55"),
            Some(ip("192.0.2.55"))
        ));
        assert!(!this_machine(ip("192.0.2.55"), Some(ip("127.0.0.1"))));
        assert!(!this_machine(ip("192.0.2.55"), None));
        if let Some(primary) = primary_ipv4() {
            assert!(this_machine(IpAddr::V4(primary), None));
        }
        if let Some(primary) = primary_ipv6() {
            assert!(this_machine(IpAddr::V6(primary), None));
        }
    }

    #[test]
    fn a_hold_too_long_to_add_ends_with_a_newer_version() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::MAX));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let target = source(&running);
        let waiter = std::thread::spawn(move || transport::fetch(&target, Some(1), HANG_DETECTOR));
        until_waiting(&running, 1);
        put(&running, &envelope(&OLD, 2)).expect("newer");
        assert_eq!(
            waiter.join().expect("waiter").expect("woken"),
            Fetched::Body(envelope(&OLD, 2))
        );
        running.stop();
    }

    #[test]
    fn a_replaced_program_stops_the_server_and_ends_it_cleanly() {
        let program = std::env::current_exe().expect("this test's program");
        assert!(stamp_of(&program).is_some());
        assert_eq!(stamp_of(&program), stamp_of(&program));
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), 2 * HANG_DETECTOR));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let target = source(&running);
        let waiter = std::thread::spawn(move || transport::fetch(&target, Some(1), HANG_DETECTOR));
        until_waiting(&running, 1);
        let shared = Arc::clone(&running.shared);
        let looks = AtomicUsize::new(0);
        // The program as started, then its path gone a while (the app moved: no replacement,
        // the server keeps serving), then another file there.
        let stamp = || match looks.fetch_add(1, Ordering::SeqCst) {
            0..3 => Some((1, 1, 0, 0)),
            3..6 => None,
            _ => Some((1, 2, 0, 0)),
        };
        supervise(
            running,
            Duration::from_millis(1),
            &stamp,
            &AtomicBool::new(false),
        )
        .expect("a clean exit");
        assert!(
            looks.load(Ordering::SeqCst) >= 7,
            "served on while the path was gone"
        );
        assert!(shared.stopping.load(Ordering::SeqCst));
        assert_eq!(
            waiter.join().expect("waiter").expect("ended"),
            Fetched::NoNewer(Some(1))
        );
        until("every connection has closed", || {
            shared.active.load(Ordering::SeqCst) == 0
        });
    }

    /// Another file at the program's path is a replacement; a path gone, or a program never
    /// stamped, is none.
    #[test]
    fn only_another_file_at_the_programs_path_replaces_it() {
        let (one, two) = (Some((1, 1, 10, 0)), Some((1, 2, 10, 0)));
        assert!(replaced(one, two));
        assert!(replaced(one, Some((1, 1, 11, 0))), "rewritten in place");
        assert!(!replaced(one, one));
        assert!(!replaced(one, None), "moved or deleted");
        assert!(!replaced(None, two), "never stamped");
    }

    /// The stamps of a path a service started from: an update renamed over it is a replacement
    /// (on Linux too, where the running program's own link then names it deleted); the file moved
    /// away is none.
    #[test]
    fn a_file_renamed_over_the_programs_path_replaces_it_and_one_moved_away_does_not() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let path = root.path().join("aterm");
        std::fs::write(&path, b"one").expect("written");
        let was = stamp_of(&path);
        assert!(was.is_some());
        let update = root.path().join("aterm.new");
        std::fs::write(&update, b"two").expect("written");
        std::fs::rename(&update, &path).expect("renamed over");
        assert!(replaced(was, stamp_of(&path)), "an update in place");
        let was = stamp_of(&path);
        std::fs::rename(&path, root.path().join("moved")).expect("moved away");
        assert_eq!(stamp_of(&path), None);
        assert!(!replaced(was, stamp_of(&path)), "moved away");
    }

    #[test]
    fn a_raised_stop_flag_stops_the_server_and_ends_it_cleanly() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), 2 * HANG_DETECTOR));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let target = source(&running);
        let waiter = std::thread::spawn(move || transport::fetch(&target, Some(1), HANG_DETECTOR));
        until_waiting(&running, 1);
        let shared = Arc::clone(&running.shared);
        let signalled = Arc::new(AtomicBool::new(false));
        let supervisor = {
            let signalled = Arc::clone(&signalled);
            std::thread::spawn(move || supervise(running, 2 * HANG_DETECTOR, &|| None, &signalled))
        };
        signalled.store(true, Ordering::SeqCst);
        supervisor.thread().unpark();
        until("the supervisor returns", || supervisor.is_finished());
        supervisor
            .join()
            .expect("supervisor")
            .expect("a clean exit");
        assert!(shared.stopping.load(Ordering::SeqCst));
        assert_eq!(
            waiter.join().expect("waiter").expect("ended"),
            Fetched::NoNewer(Some(1))
        );
    }

    /// A clean stop (a signal, a replaced program) lets the server's children go, and with them
    /// every registration it holds, `alab-secrets`' too: the name is released for another
    /// server at once, not after mDNS's record lifetime.
    /// NEGATIVE CONTROL: [`Children`]' drop ending only `set.dns` leaves the `alab-secrets`
    /// stand-in alive: "<pid> outlived the server".
    #[test]
    fn letting_the_servers_children_go_ends_them() {
        let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
        let sleep = sleep.to_str().expect("a UTF-8 path");
        let children = Children::default();
        let pids = {
            let mut set = children.lock();
            let caffeinate = quiet(sleep, &["60".to_owned()]).expect("a stand-in");
            let dns_sd = quiet(sleep, &["60".to_owned()]).expect("a stand-in");
            let alab = hold(&mut set.alab_dns, crate::alab_advertised(), sleep);
            let pids = [caffeinate.id(), dns_sd.id(), alab];
            set.caffeinate = Some(caffeinate);
            set.dns.held.push((
                Target {
                    scope: None,
                    ip: Ipv4Addr::LOCALHOST,
                },
                None,
                dns_sd,
            ));
            pids
        };
        assert!(pids.iter().all(|&pid| aterm_uds::process::pid_alive(pid)));
        drop(children);
        for pid in pids {
            assert!(
                !aterm_uds::process::pid_alive(pid),
                "{pid} outlived the server"
            );
        }
    }

    #[test]
    fn a_verdict_taken_while_a_join_is_stored_is_kept() {
        let request = |n: u8, proof: &str| JoinRequest {
            proof: Some(proof.to_owned()),
            ..parse_join_line(&join_line(n, &format!("m{n}"))).expect("valid")
        };
        let decided = vec![
            queued(request(1, "p1"), None),
            queued(request(2, "p2"), None),
        ];
        let mut held = decided.clone();
        held[0].refused = Some("t1".to_owned());
        // The same key with a new proof (its person typed the password again) is not refused.
        let mut retyped = queued(request(2, "p2-again"), None);
        retyped.refused = Some("t2".to_owned());
        held[1] = retyped;
        let kept = keep_verdicts(decided.clone(), Some(&held));
        assert_eq!(kept[0].refused.as_deref(), Some("t1"));
        assert_eq!(kept[1].refused, None);
        // CONTROL: with no queue held, nothing is refused.
        assert!(
            keep_verdicts(decided, None)
                .iter()
                .all(|q| q.refused.is_none())
        );
    }

    /// A registration that ends by itself (dns-sd giving up a name another device answers to)
    /// is said once, and started again; once one stays up, that is said too. Negative control:
    /// one this server ends at an address or TXT change is no conflict.
    #[test]
    fn an_advertisement_that_gives_up_its_name_is_said_once() {
        let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
        let sleep = sleep.to_str().expect("a UTF-8 path");
        let spawn = |_: &Target, _: Option<&VaultTxt>| quiet(sleep, &["60".to_owned()]);
        let children = Children::default();
        let mut set = children.lock();
        let at = |ip| {
            vec![Target {
                scope: None,
                ip: Ipv4Addr::new(10, 0, 0, ip),
            }]
        };
        let (here, there) = (at(2), at(3));
        let txt_at = |seq| {
            Some(VaultTxt {
                vault: "alab".to_owned(),
                signer: crate::envelope::fingerprint(&OLD),
                seq,
                head: [seq as u8; 32],
                label: None,
            })
        };
        let pid = |set: &ChildSet| {
            set.dns
                .held
                .iter()
                .map(|(_, _, child)| child.id())
                .collect::<Vec<_>>()
        };
        let round = |set: &mut ChildSet, targets: &Vec<Target>, txt| {
            reregister_slot(
                &mut set.dns,
                "alab-secrets",
                Some((targets.clone(), txt)),
                Some(Wake::Timer),
                &spawn,
            );
        };

        round(&mut set, &here, None);
        assert!(set.dns.held.len() == 1 && !set.dns.dns_sd_lost);
        let before_txt = pid(&set);
        round(&mut set, &here, txt_at(1));
        let at_seq_1 = pid(&set);
        assert_ne!(at_seq_1, before_txt, "a new TXT restarts dns-sd");
        assert!(!set.dns.dns_sd_lost, "a TXT refresh is no conflict");
        round(&mut set, &there, txt_at(1));
        assert!(
            !set.dns.dns_sd_lost,
            "a move to another address is no conflict"
        );
        assert_eq!(set.dns.held.len(), 1, "the old address is given up");

        let (_, _, gone) = set.dns.held.first_mut().expect("registered");
        gone.kill().expect("killed");
        gone.wait().expect("reaped");
        round(&mut set, &there, txt_at(1));
        assert!(
            set.dns.dns_sd_lost,
            "a registration that ended by itself is said"
        );
        let again = pid(&set);
        assert_eq!(again.len(), 1, "started again");
        round(&mut set, &there, txt_at(1));
        assert!(!set.dns.dns_sd_lost, "one that stays up is said");
        assert_eq!(pid(&set), again);
    }

    /// A Mac on a phone hotspot over Wi-Fi and on the LAN over a wire (m22 on its return,
    /// 2026-10-28) advertises on each link the address valid on it, whichever link the route to
    /// mDNS's group takes. One on a single network, two interfaces of it included, keeps one
    /// registration on every interface at the routed address. Tunnels, link-local addresses and
    /// a second address of one interface start nothing of their own.
    /// NEGATIVE CONTROL: targets that name only the routed address (the first one) leave the LAN
    /// with the hotspot's address.
    #[test]
    fn a_mac_on_two_networks_advertises_each_its_own_address() {
        use crate::network::InterfaceAddress;
        const LAN_FLAGS: u32 =
            (libc::IFF_UP | libc::IFF_RUNNING | libc::IFF_BROADCAST | libc::IFF_MULTICAST) as u32;
        const TUNNEL_FLAGS: u32 =
            (libc::IFF_UP | libc::IFF_RUNNING | libc::IFF_POINTOPOINT | libc::IFF_MULTICAST) as u32;
        let v4 = |name: &str, ip: [u8; 4], mask: [u8; 4], flags: u32| InterfaceAddress {
            name: name.to_owned(),
            address: IpAddr::V4(Ipv4Addr::from(ip)),
            netmask: Some(IpAddr::V4(Ipv4Addr::from(mask))),
            flags,
        };
        let hotspot = v4("en0", [172, 20, 10, 2], [255, 255, 255, 240], LAN_FLAGS);
        let wired = v4("en7", [192, 168, 1, 5], [255, 255, 255, 0], LAN_FLAGS);
        let noise = [
            v4("utun3", [100, 64, 0, 1], [255, 192, 0, 0], TUNNEL_FLAGS),
            v4("bridge0", [169, 254, 7, 7], [255, 255, 0, 0], LAN_FLAGS),
            v4("en7", [192, 168, 99, 5], [255, 255, 255, 0], LAN_FLAGS),
            v4(
                "en9",
                [192, 168, 50, 9],
                [255, 255, 255, 0],
                LAN_FLAGS & !(libc::IFF_RUNNING as u32),
            ),
        ];
        let scoped = |name: &str, ip: [u8; 4]| Target {
            scope: Some(name.to_owned()),
            ip: Ipv4Addr::from(ip),
        };
        let mut both = vec![hotspot.clone(), wired.clone()];
        both.extend(noise.iter().cloned());
        for routed in [[172, 20, 10, 2], [192, 168, 1, 5]] {
            let targets = advert_targets(&both, Some(Ipv4Addr::from(routed)));
            let (first, second) = if routed[0] == 172 {
                (
                    scoped("en0", [172, 20, 10, 2]),
                    scoped("en7", [192, 168, 1, 5]),
                )
            } else {
                (
                    scoped("en7", [192, 168, 1, 5]),
                    scoped("en0", [172, 20, 10, 2]),
                )
            };
            assert_eq!(targets, [first, second], "routed by {routed:?}");
        }
        let args = registration_args(&scoped("en7", [192, 168, 1, 5]), "alab-secrets", 7484, None);
        assert_eq!(
            args[..4],
            ["-i", "en7", "-P", "alab-secrets"],
            "a scoped registration is made on its interface alone"
        );
        assert_eq!(args[8], "192.168.1.5");

        let routed = Some(Ipv4Addr::new(192, 168, 1, 5));
        let one_lan = [
            wired.clone(),
            v4("en0", [192, 168, 1, 20], [255, 255, 255, 0], LAN_FLAGS),
        ];
        let unscoped = vec![Target {
            scope: None,
            ip: Ipv4Addr::new(192, 168, 1, 5),
        }];
        assert_eq!(advert_targets(&one_lan, routed), unscoped);
        assert_eq!(
            advert_targets(&noise, routed),
            unscoped,
            "noise is no network"
        );
        assert_eq!(
            registration_args(&unscoped[0], "alab-secrets", 7484, None),
            dns_sd_args("alab-secrets", 7484, Ipv4Addr::new(192, 168, 1, 5), None)
        );
        assert_eq!(
            advert_targets(&[wired], None),
            unscoped,
            "no route to the group: the one network's address"
        );
        assert!(advert_targets(&[], None).is_empty());
    }

    /// Back on the LAN, the advert re-registers at the address the move brought as soon as the
    /// kernel tells of the move, and its round and log line say a route change woke it. The
    /// kernel's route socket is a test's channel here; the timer is [`ADVERTISE_EVERY`].
    /// NEGATIVE CONTROL: unhook the route source and the round that re-registers is the timer's
    /// (`started_by` reads `Timer`).
    #[test]
    fn the_advert_reregisters_on_a_route_change_and_says_why() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
        let sleep = sleep.to_str().expect("a UTF-8 path").to_owned();
        let at = |ip: [u8; 4]| Target {
            scope: None,
            ip: Ipv4Addr::from(ip),
        };
        let (away, back) = (at([172, 20, 10, 2]), at([192, 168, 1, 5]));
        let here = Arc::new(Mutex::new(vec![away.clone()]));
        let (moved, changes) = std::sync::mpsc::channel::<()>();
        let children = Children::default();
        children.tend_seamed(
            false,
            Some((
                "alab-secrets".to_owned(),
                running.addr.port(),
                Arc::clone(&running.shared),
            )),
            None,
            Seams {
                routes: Some(Box::new(move |most| changes.recv_timeout(most).is_ok())),
                browse: false,
                targets: Box::new({
                    let here = Arc::clone(&here);
                    move || here.lock().expect("targets").clone()
                }),
                spawn: Box::new(move |_, _, _, _| quiet(&sleep, &["60".to_owned()])),
                every: ADVERTISE_EVERY,
                power: Box::new(|_, _| false),
                end_for_sleep: Arc::new(end),
                sleep_bound: SLEEP_YIELD_BOUND,
            },
        );
        let registered = || -> (Vec<Target>, Option<Option<Wake>>) {
            let set = children.lock();
            (
                set.dns.held.iter().map(|(t, _, _)| t.clone()).collect(),
                set.dns.started_by,
            )
        };
        until("advertised where the server started", || {
            registered().0 == [away.clone()]
        });
        assert_eq!(registered().1, Some(None), "the first round, as it started");
        *here.lock().expect("targets") = vec![back.clone()];
        // The kernel tells of the move; a route source nobody hooked hears nothing of it.
        let _ = moved.send(());
        until("advertised where the move brought it", || {
            registered().0 == [back.clone()]
        });
        assert_eq!(
            registered().1,
            Some(Some(Wake::Route)),
            "a route change woke the round that moved it"
        );
        assert_eq!(
            advertising_line("alab-secrets", &[back.ip], Some(Wake::Route)),
            "advertising alab-secrets.local at 192.168.1.5 (woken by a route change)"
        );
        drop(children);
        running.stop();
    }

    #[test]
    fn per_tick_role_decider_restarts_or_stops_dns_sd_as_role_changes() {
        let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
        let sleep = sleep.to_str().expect("a UTF-8 path");
        let spawn = |_: &Target, _: Option<&VaultTxt>| quiet(sleep, &["60".to_owned()]);
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(100)));
        let here = [Target {
            scope: None,
            ip: Ipv4Addr::new(10, 0, 0, 9),
        }];
        let mut watcher = AlabNameWatcher::new();
        let children = Children::default();
        let mut set = children.lock();

        // Tick 1: role decider returns `None` -> no primary `dns-sd` started, pins untouched.
        let mut decide_none: RoleDecider = Box::new(|_| None);
        let (p1, s1) = tend_role_targets(
            &mut decide_none,
            running.addr.port(),
            &running.shared,
            None,
            &mut watcher,
            &here,
            &[],
            &[],
            |_| None,
        );
        assert!(p1.is_none());
        assert!(s1.is_some_and(|(_, _, t)| t.is_none()));

        // Tick 2: role decider pins `alab` and advertises `alab-secrets`.
        let key = [0x33u8; 32];
        let mut decide_alab: RoleDecider = Box::new(move |pins| {
            pins.push((crate::ALAB_VAULT.to_owned(), key));
            Some(crate::alab_advertised().to_owned())
        });
        let (p2, _) = tend_role_targets(
            &mut decide_alab,
            running.addr.port(),
            &running.shared,
            None,
            &mut watcher,
            &here,
            &[],
            &[],
            |_| None,
        );
        let (name2, _, target2) = p2.expect("advertised on tick 2");
        assert_eq!(name2, crate::alab_advertised());
        assert_eq!(
            running.shared.pins_snapshot().as_slice(),
            &[(crate::ALAB_VAULT.to_owned(), key)]
        );
        reregister_slot(&mut set.dns, &name2, target2, None, &spawn);
        let pid_alab = set
            .dns
            .held
            .first()
            .map(|(_, _, c)| c.id())
            .expect("started");
        assert_eq!(set.dns.held_name.as_deref(), Some(crate::alab_advertised()));

        // Tick 3: role switches to `"m22"` -> `reregister_slot` replaces the `alab-secrets` child!
        reregister_slot(
            &mut set.dns,
            "m22",
            Some((here.to_vec(), None)),
            None,
            &spawn,
        );
        let pid_m22 = set
            .dns
            .held
            .first()
            .map(|(_, _, c)| c.id())
            .expect("restarted");
        assert_ne!(
            pid_m22, pid_alab,
            "changing advertised label restarts dns-sd"
        );
        assert_eq!(set.dns.held_name.as_deref(), Some("m22"));
        assert!(!set.dns.dns_sd_lost);
        assert!(!aterm_uds::process::pid_alive(pid_alab));

        // Tick 4: role becomes `None` -> `reregister_slot` ends the child cleanly.
        reregister_slot(&mut set.dns, "", None, None, &spawn);
        assert!(set.dns.held.is_empty());
        assert!(set.dns.held_name.is_none());
        assert!(!aterm_uds::process::pid_alive(pid_m22));

        drop(set);
        drop(children);
        running.stop();
    }

    /// A laptop (`m5`) that took `alab-secrets` after 90 s off the network comes back to a LAN
    /// where `m22` holds the name at the same `seq`. Its `dns-sd -P` loses the host name and
    /// ends at once, so `m22` never hears its `_alab-vault._tcp` instance, and the (`seq`,
    /// `label`) order, which ranks `m5` first, never meets the two. `m5` lets the name go and
    /// stays off while `m22` answers there, and starts no further registration of it.
    /// NEGATIVE CONTROL: without [`AlabNameWatcher::lost_name`] in [`reregister_roles`], `m5`
    /// holds on every round and each round starts another `dns-sd -P` that loses again.
    #[test]
    fn a_holder_whose_registration_loses_the_name_lets_it_go() {
        let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
        let sleep = sleep.to_str().expect("a UTF-8 path").to_owned();
        let started = Arc::new(Mutex::new(Vec::<String>::new()));
        let spawn: Box<SpawnAdvert> = Box::new({
            let started = Arc::clone(&started);
            move |name, _, _, _| {
                started.lock().expect("started").push(name.to_owned());
                // `alab-secrets` loses to `m22`'s host record as it probes.
                let lasts = if name == crate::alab_advertised() {
                    "0"
                } else {
                    "60"
                };
                quiet(&sleep, &[lasts.to_owned()])
            }
        });
        let alab_started = || {
            started
                .lock()
                .expect("started")
                .iter()
                .filter(|name| *name == crate::alab_advertised())
                .count()
        };
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let key = signer_public(&OLD).expect("public");
        let mut options = options(root.path(), Duration::from_millis(100));
        options.pins = vec![(crate::ALAB_VAULT.to_owned(), key)];
        let running = run(&options);
        put(&running, &envelope(&OLD, 81)).expect("m5 holds seq 81");
        let mut decide: RoleDecider = Box::new(move |pins| {
            pins.push((crate::ALAB_VAULT.to_owned(), key));
            Some("m5".to_owned())
        });
        let here = [Target {
            scope: None,
            ip: Ipv4Addr::new(10, 0, 0, 5),
        }];

        // Off the network, nothing answers for 90 s: `m5` takes the name.
        let mut watcher = AlabNameWatcher::new();
        let start = Instant::now();
        for now in [start, start + ALAB_FALLBACK_AFTER] {
            let input = AlabNameInput {
                own_label: "m5",
                own_seq: Some(81),
                holders: &[],
                now,
            };
            watcher.decide(&input, |_| None);
        }
        assert!(watcher.is_holding(), "m5 took the name while away");

        // Back on the LAN, `m22` holds the name at seq 81 and answers there.
        let m22 = [crate::discover::DiscoveredVault {
            url: "http://alab-secrets.local:7484/v1/alab".to_owned(),
            txt: Some(VaultTxt {
                vault: crate::ALAB_VAULT.to_owned(),
                signer: crate::envelope::fingerprint(&key),
                seq: 81,
                head: [81; 32],
                label: Some("m22".to_owned()),
            }),
        }];
        let children = Children::default();
        let mut set = children.lock();
        let mut probed_name = 0;
        for round in 0..6 {
            let targets = tend_role_targets(
                &mut decide,
                running.addr.port(),
                &running.shared,
                None,
                &mut watcher,
                &here,
                &[],
                &m22,
                |target| {
                    probed_name += usize::from(target == crate::alab_advertised());
                    Some(81)
                },
            );
            reregister_roles(&mut set, targets, &mut watcher, Some(Wake::Timer), &*spawn);
            // Each registration of the name ends as it probes, before the next round looks.
            let deadline = Instant::now() + HANG_DETECTOR;
            while set
                .alab_dns
                .held
                .iter_mut()
                .any(|(_, _, child)| matches!(child.try_wait(), Ok(None)))
            {
                assert!(Instant::now() < deadline, "never: the registration ended");
                std::thread::sleep(Duration::from_millis(2));
            }
            if round == 0 {
                assert_eq!(
                    alab_started(),
                    1,
                    "back on the LAN, m5 registers the name once"
                );
            }
        }
        assert!(!watcher.is_holding(), "m5 let the name go");
        assert_eq!(
            alab_started(),
            1,
            "no registration of the name after the one that lost"
        );
        assert!(set.alab_dns.held.is_empty(), "the lost registration ended");
        assert!(
            probed_name >= 4,
            "m5 listens for the name again: {probed_name}"
        );
        assert_eq!(
            set.dns.held_name.as_deref(),
            Some("m5"),
            "m5 serves under its own name"
        );

        drop(set);
        drop(children);
        running.stop();
    }

    /// One registration of `name` in `slot`, a stand-in `sleep 60`: its pid.
    fn hold(slot: &mut DnsSdSlot, name: &str, sleep: &str) -> u32 {
        let child = quiet(sleep, &["60".to_owned()]).expect("a stand-in");
        let pid = child.id();
        let target = Target {
            scope: None,
            ip: Ipv4Addr::new(10, 0, 0, 5),
        };
        slot.held.push((target, None, child));
        slot.held_name = Some(name.to_owned());
        pid
    }

    /// The [`SleepYield`] [`Children::tend_role_seamed`] makes over `children`, ending each
    /// registration with `end` and waiting at most `bound`, its sleep flag, and the bell it
    /// rings at a wake (which the caller keeps alive).
    fn yielding(
        children: &Children,
        end: Arc<EndAdvert>,
        bound: Duration,
    ) -> (SleepYield, Arc<AtomicBool>, Arc<Bell>) {
        let bell = Arc::new(Bell::new());
        let asleep = Arc::new(AtomicBool::new(false));
        let yielding = SleepYield {
            set: Arc::clone(&children.set),
            asleep: Arc::clone(&asleep),
            bell: Arc::downgrade(&bell),
            end,
            bound,
        };
        (yielding, asleep, bell)
    }

    /// The system's message `message`, answered as the listener answers it
    /// ([`aterm_uds::power::answer`]) with `yielding` as its handler, on a thread of its own:
    /// the channel that hears its acknowledgement.
    fn announce(
        yielding: SleepYield,
        message: aterm_uds::power::Message,
    ) -> (std::thread::JoinHandle<()>, std::sync::mpsc::Receiver<()>) {
        let (allowed, allows) = std::sync::mpsc::channel();
        let answering = std::thread::spawn(move || {
            aterm_uds::power::answer(message, &mut |power| yielding.hear(power), || {
                let _ = allowed.send(());
            });
        });
        (answering, allows)
    }

    /// A will-sleep ends both of a holder's registrations, its own name's and `alab-secrets`',
    /// before the system is let sleep, so mDNSResponder has no record of either to hand a
    /// Bonjour Sleep Proxy. A round that decided to hold both before the sleep, and that the
    /// sleep overtook, registers neither after the wake and lets the name go.
    /// NEGATIVE CONTROLS: with [`take_for_sleep`] taking nothing (`return Vec::new();` before
    /// `set.slept = true`), both stand-ins are alive at the acknowledgement: `left: Some([true,
    /// true])`. Without the `slept` branch's `alab_watcher.slept(); return;` in
    /// [`reregister_awake`], the overtaken round registers both: `left: 2`, "a decision from
    /// before the sleep".
    #[test]
    fn a_will_sleep_ends_every_registration_before_the_mac_may_sleep() {
        let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
        let sleep = sleep.to_str().expect("a UTF-8 path").to_owned();
        let children = Children::default();
        let alab = crate::alab_advertised();
        let pids = {
            let mut set = children.lock();
            [
                hold(&mut set.dns, "m5", &sleep),
                hold(&mut set.alab_dns, alab, &sleep),
            ]
        };
        let (yielding, asleep, _bell) = yielding(&children, Arc::new(end), HANG_DETECTOR);
        let mut alive_when_allowed = None;
        aterm_uds::power::answer(
            aterm_uds::power::Message::WillSleep,
            &mut |power| yielding.hear(power),
            || alive_when_allowed = Some(pids.map(aterm_uds::process::pid_alive)),
        );
        assert_eq!(
            alive_when_allowed,
            Some([false, false]),
            "every registration ended before the Mac was let sleep"
        );

        let started = Arc::new(AtomicUsize::new(0));
        let spawn: Box<SpawnAdvert> = Box::new({
            let started = Arc::clone(&started);
            move |_, _, _, _| {
                started.fetch_add(1, Ordering::SeqCst);
                quiet(&sleep, &["60".to_owned()])
            }
        });
        let here = vec![Target {
            scope: None,
            ip: Ipv4Addr::new(10, 0, 0, 5),
        }];
        let both = || -> RoleTargets {
            (
                Some(("m5".to_owned(), 7484, Some((here.clone(), None)))),
                Some((alab.to_owned(), 7484, Some((here.clone(), None)))),
            )
        };
        // A round that decided to hold both (nothing answered at the name for 90 s) before the
        // sleep, and takes the lock only after the wake, registers neither: its decision
        // predates the sleep, and it lets the name go.
        let mut watcher = AlabNameWatcher::new();
        let start = Instant::now();
        for now in [start, start + ALAB_FALLBACK_AFTER] {
            let input = AlabNameInput {
                own_label: "m5",
                own_seq: Some(81),
                holders: &[],
                now,
            };
            watcher.decide(&input, |_| None);
        }
        assert!(watcher.is_holding());
        yielding.hear(Power::Woke);
        assert!(!asleep.load(Ordering::SeqCst));
        let mut set = children.lock();
        assert!(set.dns.held.is_empty() && set.alab_dns.held.is_empty());
        reregister_awake(&mut set, &asleep, both(), &mut watcher, None, &*spawn);
        assert_eq!(
            started.load(Ordering::SeqCst),
            0,
            "a decision from before the sleep"
        );
        assert!(!watcher.is_holding(), "the overtaken round let the name go");
        drop(set);
    }

    /// A will-sleep on a server that holds no registration lets the Mac sleep at once: it waits
    /// only as long as what it holds takes to end, never out its bound (an hour here, so a wait
    /// for that would fail the hang detector). The next round still lets the name go.
    /// NEGATIVE CONTROL: a will-sleep that waits out its bound before it answers
    /// (`std::thread::sleep(self.bound)` before it reads `ended`) fails the hang detector:
    /// "never: the Mac was let sleep: Timeout".
    #[test]
    fn a_will_sleep_with_nothing_registered_lets_the_mac_sleep_at_once() {
        let children = Children::default();
        let (yielding, _asleep, _bell) = yielding(
            &children,
            Arc::new(|_| panic!("nothing is registered")),
            Duration::from_secs(3600),
        );
        let (answering, allows) = announce(yielding, aterm_uds::power::Message::WillSleep);
        allows
            .recv_timeout(HANG_DETECTOR)
            .expect("never: the Mac was let sleep");
        answering.join().expect("answered");
        assert!(children.lock().slept, "the next round lets the name go");
    }

    /// A holder (`m5`) that slept lets `alab-secrets` go, and the first round after its wake
    /// (which the wake rings) registers its own name again and competes for `alab-secrets` by
    /// the probe, as any server that does not hold it does: it probes the name and holds off.
    /// NEGATIVE CONTROL: without `alab_watcher.slept()` in [`after_sleep`], `m5` still holds
    /// the name after its wake and registers it again unprobed: "m5 let the name go as it
    /// slept" fails.
    #[test]
    fn a_holder_that_slept_competes_for_the_name_by_the_probe_after_its_wake() {
        let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
        let sleep = sleep.to_str().expect("a UTF-8 path").to_owned();
        let alab = crate::alab_advertised();
        let started = Arc::new(Mutex::new(Vec::<String>::new()));
        let spawn: Box<SpawnAdvert> = Box::new({
            let started = Arc::clone(&started);
            move |name, _, _, _| {
                started.lock().expect("started").push(name.to_owned());
                quiet(&sleep, &["60".to_owned()])
            }
        });
        let alab_started = || {
            started
                .lock()
                .expect("started")
                .iter()
                .filter(|name| *name == alab)
                .count()
        };
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let key = signer_public(&OLD).expect("public");
        let mut options = options(root.path(), Duration::from_millis(100));
        options.pins = vec![(crate::ALAB_VAULT.to_owned(), key)];
        let running = run(&options);
        put(&running, &envelope(&OLD, 81)).expect("m5 holds seq 81");
        let mut decide: RoleDecider = Box::new(move |pins| {
            pins.push((crate::ALAB_VAULT.to_owned(), key));
            Some("m5".to_owned())
        });
        let here = [Target {
            scope: None,
            ip: Ipv4Addr::new(10, 0, 0, 5),
        }];
        // Nothing answered at the name for 90 s: `m5` holds it.
        let mut watcher = AlabNameWatcher::new();
        let start = Instant::now();
        for now in [start, start + ALAB_FALLBACK_AFTER] {
            let input = AlabNameInput {
                own_label: "m5",
                own_seq: Some(81),
                holders: &[],
                now,
            };
            watcher.decide(&input, |_| None);
        }
        assert!(watcher.is_holding());
        let children = Children::default();
        let (yielding, asleep, bell) = yielding(&children, Arc::new(end), HANG_DETECTOR);
        let probed = std::cell::Cell::new(0usize);
        let mut round = |watcher: &mut AlabNameWatcher, why| {
            after_sleep(&children.set, watcher);
            let targets = tend_role_targets(
                &mut decide,
                running.addr.port(),
                &running.shared,
                None,
                watcher,
                &here,
                &[],
                &[],
                |target| {
                    probed.set(probed.get() + usize::from(target == alab));
                    None
                },
            );
            let mut set = children.lock();
            reregister_awake(&mut set, &asleep, targets, watcher, why, &*spawn);
        };
        let hear = |message| {
            aterm_uds::power::answer(message, &mut |power| yielding.hear(power), || {});
        };

        round(&mut watcher, Some(Wake::Timer));
        assert_eq!(alab_started(), 1, "the holder registers the name");
        assert_eq!(probed.get(), 0, "a holder does not probe its own name");

        hear(aterm_uds::power::Message::WillSleep);
        assert!(
            children.lock().alab_dns.held.is_empty(),
            "the name was let go first"
        );
        // A round between the will-sleep and the sleep lets the name go and registers nothing.
        round(&mut watcher, Some(Wake::Timer));
        assert!(!watcher.is_holding(), "m5 let the name go as it slept");
        assert_eq!(alab_started(), 1);
        assert!(
            children.lock().dns.held.is_empty(),
            "nothing registered asleep"
        );

        hear(aterm_uds::power::Message::HasPoweredOn);
        assert_eq!(
            bell.nap(HANG_DETECTOR),
            Wake::Resumed,
            "the wake rings the advert"
        );
        let before = probed.get();
        round(&mut watcher, Some(Wake::Resumed));
        assert_eq!(probed.get(), before + 1, "awake, m5 probes the name again");
        assert_eq!(
            alab_started(),
            1,
            "and holds off while it has not had it for 90 s"
        );
        let set = children.lock();
        assert_eq!(
            set.dns.held_name.as_deref(),
            Some("m5"),
            "m5 serves under its own"
        );
        assert_eq!(set.dns.started_by, Some(Some(Wake::Resumed)));
        assert!(set.alab_dns.held.is_empty());
        drop(set);
        drop(children);
        running.stop();
    }

    /// A will-sleep whose registration will not end lets the Mac sleep once its bound runs
    /// out, and the registration still ends once it can.
    /// NEGATIVE CONTROL: waiting for the sleep thread with no bound (`recv_timeout(Duration::MAX)`
    /// in place of `recv_timeout(self.bound)`) keeps the Mac awake until the registration ends,
    /// which here comes only after the Mac was let sleep: the hang detector fails the test
    /// ("never: the Mac was let sleep: Timeout").
    #[test]
    fn a_will_sleep_lets_the_mac_sleep_even_when_a_registration_will_not_end() {
        let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
        let sleep = sleep.to_str().expect("a UTF-8 path");
        let children = Children::default();
        let pid = hold(
            &mut children.lock().alab_dns,
            crate::alab_advertised(),
            sleep,
        );
        let (entered, enters) = std::sync::mpsc::channel::<()>();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let released = Mutex::new(released);
        let stuck: Arc<EndAdvert> = Arc::new(move |child| {
            let _ = entered.send(());
            // Held until the test lets it go, after the Mac was let sleep (or the test ended).
            let _ = released.lock().expect("released").recv();
            end(child);
        });
        let (yielding, _asleep, _bell) = yielding(&children, stuck, Duration::from_millis(50));
        let (answering, allows) = announce(yielding, aterm_uds::power::Message::WillSleep);
        enters
            .recv_timeout(HANG_DETECTOR)
            .expect("the sleep thread began ending the registration");
        allows
            .recv_timeout(HANG_DETECTOR)
            .expect("never: the Mac was let sleep");
        assert!(
            aterm_uds::process::pid_alive(pid),
            "the Mac was let sleep with the registration not ended"
        );
        answering.join().expect("answered");
        release.send(()).expect("the sleep thread waits");
        until("the registration ends once it can", || {
            !aterm_uds::process::pid_alive(pid)
        });
    }

    /// Rounds while the Mac sleeps (its dark wakes, when nothing may answer at `alab-secrets`
    /// only because the network is down) never add up to the 90 s that take the name: the
    /// first round after the wake probes the name and holds off, and registers nothing unprobed.
    /// NEGATIVE CONTROL: without `alab_watcher.slept()` in [`reregister_awake`]'s asleep branch,
    /// two asleep rounds 90 s apart leave the watcher holding: "a round asleep took the name"
    /// fails.
    #[test]
    fn rounds_while_asleep_never_take_the_name_unprobed_at_the_wake() {
        let started = Arc::new(AtomicUsize::new(0));
        let spawn: Box<SpawnAdvert> = Box::new({
            let started = Arc::clone(&started);
            move |_, _, _, _| {
                started.fetch_add(1, Ordering::SeqCst);
                Err(io::Error::other("nothing registers asleep"))
            }
        });
        let decided = |watcher: &mut AlabNameWatcher, now, probed: &mut usize| {
            let input = AlabNameInput {
                own_label: "m5",
                own_seq: Some(81),
                holders: &[],
                now,
            };
            watcher.decide(&input, |_| {
                *probed += 1;
                None
            })
        };
        let children = Children::default();
        let asleep = AtomicBool::new(true);
        let mut watcher = AlabNameWatcher::new();
        let mut probed = 0;
        let start = Instant::now();
        for now in [start, start + ALAB_FALLBACK_AFTER] {
            let want = decided(&mut watcher, now, &mut probed);
            let alab = want.then(|| {
                let at = Target {
                    scope: None,
                    ip: Ipv4Addr::new(10, 0, 0, 5),
                };
                (vec![at], None)
            });
            let targets = (
                None,
                Some((crate::alab_advertised().to_owned(), 7484, alab)),
            );
            reregister_awake(
                &mut children.lock(),
                &asleep,
                targets,
                &mut watcher,
                Some(Wake::Timer),
                &*spawn,
            );
        }
        assert_eq!(
            started.load(Ordering::SeqCst),
            0,
            "nothing registers asleep"
        );
        assert!(!watcher.is_holding(), "a round asleep took the name");

        asleep.store(false, Ordering::SeqCst);
        let before = probed;
        assert!(
            !decided(&mut watcher, start + ALAB_FALLBACK_AFTER * 2, &mut probed),
            "the first round after the wake holds off"
        );
        assert_eq!(probed, before + 1, "and probes the name");
    }

    /// The server's own wiring ([`Children::tend_role_seamed`]): it hands its listener
    /// ([`Seams::power`], here the test's arm, which keeps the handler) a [`SleepYield`] that
    /// shares one sleep flag with the `vault-tend` rounds. A will-sleep through that handler
    /// ends the registration before the Mac is let sleep, no round registers again while it
    /// sleeps, and the wake brings the registration back.
    /// NEGATIVE CONTROL: a [`SleepYield`] with a sleep flag of its own (`asleep:
    /// Arc::new(AtomicBool::new(false))` in place of `Arc::clone(&asleep)`) lets the next round
    /// register again as the Mac goes to sleep: `left: 2`, "no round registers asleep".
    #[test]
    fn the_servers_listener_ends_its_registration_at_a_will_sleep_until_the_wake() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
        let sleep = sleep.to_str().expect("a UTF-8 path").to_owned();
        type Handler = Box<dyn FnMut(Power) + Send>;
        let heard: Arc<Mutex<Option<Handler>>> = Arc::default();
        let started = Arc::new(AtomicUsize::new(0));
        let rounds = Arc::new(AtomicUsize::new(0));
        let children = Children::default();
        children.tend_seamed(
            false,
            Some((
                "alab-secrets".to_owned(),
                running.addr.port(),
                Arc::clone(&running.shared),
            )),
            None,
            Seams {
                routes: None,
                browse: false,
                targets: Box::new({
                    let rounds = Arc::clone(&rounds);
                    move || {
                        rounds.fetch_add(1, Ordering::SeqCst);
                        vec![Target {
                            scope: None,
                            ip: Ipv4Addr::new(10, 0, 0, 5),
                        }]
                    }
                }),
                spawn: Box::new({
                    let started = Arc::clone(&started);
                    move |_, _, _, _| {
                        started.fetch_add(1, Ordering::SeqCst);
                        quiet(&sleep, &["60".to_owned()])
                    }
                }),
                // No timer round comes during the test: only a ring starts one.
                every: Duration::from_secs(3600),
                power: Box::new({
                    let heard = Arc::clone(&heard);
                    move |_, handler| {
                        *heard.lock().expect("heard") = Some(handler);
                        true
                    }
                }),
                end_for_sleep: Arc::new(end),
                sleep_bound: HANG_DETECTOR,
            },
        );
        let mut handler = heard
            .lock()
            .expect("heard")
            .take()
            .expect("the server listens for this Mac's sleep");
        until("the server advertises", || {
            children.lock().dns.held.len() == 1
        });
        let pid = children.lock().dns.held[0].2.id();
        let mut alive_when_allowed = None;
        aterm_uds::power::answer(aterm_uds::power::Message::WillSleep, &mut *handler, || {
            alive_when_allowed = Some(aterm_uds::process::pid_alive(pid))
        });
        assert_eq!(
            alive_when_allowed,
            Some(false),
            "the registration ended before the Mac was let sleep"
        );

        // Two rings: the second round starts only once the first, wholly after the will-sleep,
        // is done.
        let bell = children
            .bell
            .lock()
            .expect("bell")
            .clone()
            .expect("the advert's bell");
        let before = rounds.load(Ordering::SeqCst);
        for n in 1..=2 {
            bell.ring(Wake::Put);
            until("a round runs while the Mac sleeps", || {
                rounds.load(Ordering::SeqCst) >= before + n
            });
        }
        assert_eq!(
            started.load(Ordering::SeqCst),
            1,
            "no round registers asleep"
        );
        assert!(children.lock().dns.held.is_empty());

        aterm_uds::power::answer(
            aterm_uds::power::Message::HasPoweredOn,
            &mut *handler,
            || {},
        );
        until("the wake brings the registration back", || {
            children.lock().dns.held.len() == 1
        });
        assert_eq!(started.load(Ordering::SeqCst), 2);
        drop(handler);
        drop(children);
        running.stop();
    }

    #[test]
    fn a_keep_awake_that_ends_is_started_again() {
        let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
        let sleep = sleep.to_str().expect("a UTF-8 path");
        let spawn = || quiet(sleep, &["60".to_owned()]);
        let refused = || Err(io::Error::other("no such program"));
        let children = Children::default();
        let mut set = children.lock();
        let pid = |set: &ChildSet| set.caffeinate.as_ref().map(aterm_uds::spawnfd::Child::id);

        tend_awake(&mut set, &spawn);
        let first = pid(&set).expect("started");
        // A live one stays.
        tend_awake(&mut set, &spawn);
        assert_eq!(pid(&set), Some(first));
        // Killed from outside, it is started again.
        let killed = set.caffeinate.as_mut().expect("held");
        killed.kill().expect("killed");
        killed.wait().expect("reaped");
        tend_awake(&mut set, &spawn);
        let second = pid(&set).expect("started again");
        assert_ne!(second, first);
        assert!(aterm_uds::process::pid_alive(second));
        // A start that fails leaves none and is tried again at the next look.
        end(set.caffeinate.as_mut().expect("held"));
        tend_awake(&mut set, &refused);
        assert!(set.caffeinate.is_none() && set.awake_failed);
        tend_awake(&mut set, &spawn);
        let third = pid(&set).expect("started once it can be");
        assert!(!set.awake_failed);
        drop(set);
        drop(children);
        assert!(
            !aterm_uds::process::pid_alive(third),
            "it outlived the server"
        );
    }

    #[test]
    fn a_client_that_never_finishes_its_head_is_cut_at_the_deadline() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let tuning = Tuning {
            head_deadline: Duration::from_millis(100),
            linger: Duration::from_millis(100),
            ..Tuning::default()
        };
        let running = run_tuned(&options(root.path(), Duration::from_millis(200)), tuning);
        for start in [&b""[..], b"GET /v1/alab HTTP/1.1\r\nHost: x\r\n"] {
            let answer = read_all(sent(&running, start));
            assert!(answer.starts_with("HTTP/1.1 408 "), "{answer}");
        }
        let stream = sent(&running, b"GET /v1/alab HTTP/1.1\r\nX-Slow: ");
        let mut writer = stream.try_clone().expect("clone");
        let trickle = std::thread::spawn(move || {
            while writer.write_all(b"a").is_ok() {
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let answer = read_all(stream);
        assert!(answer.starts_with("HTTP/1.1 408 "), "{answer}");
        trickle.join().expect("trickle");
        until("the cut connections are closed", || {
            running.shared.active.load(Ordering::SeqCst) == 0
        });
        running.stop();
    }

    #[test]
    fn an_answer_read_too_slowly_is_cut_at_its_deadline() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let tuning = Tuning {
            answer_deadline: Duration::from_millis(200),
            write_stall: 2 * HANG_DETECTOR,
            ..Tuning::default()
        };
        let running = run_tuned(&options(root.path(), Duration::from_millis(200)), tuning);
        let big = Arc::new(vec![b'x'; 15 << 20]);
        running.shared.lock_server_state().vaults.insert(
            "alab".to_owned(),
            Slot::new(Held {
                seq: 1,
                signer: OLD,
                head: aterm_digest::Sha256::digest(&*big),
                parented: false,
                handoff: Vec::new(),
                bytes: Arc::clone(&big),
            }),
        );
        let stream = sent(&running, b"GET /v1/alab HTTP/1.1\r\nHost: x\r\n\r\n");
        until("the server gives up on the answer", || {
            running.shared.served.load(Ordering::SeqCst) == 1
                && running.shared.active.load(Ordering::SeqCst) == 0
        });
        let answer = read_all(stream);
        assert!(
            answer.starts_with("HTTP/1.1 200 "),
            "{}",
            &answer[..answer.len().min(200)]
        );
        assert!(
            answer.len() < big.len(),
            "the whole answer was sent: the socket buffers held it"
        );
        running.stop();
    }

    #[test]
    fn subscriber_header_records_and_persists_last_heard_while_rejecting_malformed_recipient() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let tuning = Tuning {
            now: || 1_793_160_500,
            ..Tuning::default()
        };
        let running = run_tuned(&options(root.path(), Duration::from_millis(200)), tuning);
        let body = Arc::new(b"envelope".to_vec());
        running.shared.lock_server_state().vaults.insert(
            "alab".to_owned(),
            Slot::new(Held {
                seq: 1,
                signer: OLD,
                head: aterm_digest::Sha256::digest(&*body),
                parented: false,
                handoff: Vec::new(),
                bytes: Arc::clone(&body),
            }),
        );
        let valid = crate::age::Identity::from_secret_bytes([0x17; 32])
            .recipient()
            .to_bech32();
        let req_valid = format!(
            "GET /v1/alab HTTP/1.1\r\nHost: x\r\n{}: {valid}\r\n\r\n",
            crate::transport::SUBSCRIBER_HEADER
        );
        let answer = read_all(sent(&running, req_valid.as_bytes()));
        assert!(answer.starts_with("HTTP/1.1 200 "), "{answer}");

        // Negative control: malformed `X-Aterm-Subscriber` or GET of non-existent vault is never
        // recorded in `<store>/*.heard`.
        let req_bad = format!(
            "GET /v1/alab HTTP/1.1\r\nHost: x\r\n{}: not-an-age-recipient\r\n\r\n",
            crate::transport::SUBSCRIBER_HEADER
        );
        let answer_bad = read_all(sent(&running, req_bad.as_bytes()));
        assert!(answer_bad.starts_with("HTTP/1.1 200 "), "{answer_bad}");

        let other = crate::age::Identity::from_secret_bytes([0x29; 32])
            .recipient()
            .to_bech32();
        let req_missing = format!(
            "GET /v1/missing HTTP/1.1\r\nHost: x\r\n{}: {other}\r\n\r\n",
            crate::transport::SUBSCRIBER_HEADER
        );
        let answer_missing = read_all(sent(&running, req_missing.as_bytes()));
        assert!(
            answer_missing.starts_with("HTTP/1.1 404 "),
            "{answer_missing}"
        );

        assert_eq!(
            read_last_heard(root.path()),
            vec![("alab".to_owned(), valid.clone(), 1_793_160_500)]
        );
        running.stop();
        assert_eq!(
            read_last_heard(root.path()),
            vec![("alab".to_owned(), valid, 1_793_160_500)]
        );
    }

    fn record_entrant(
        tag: u8,
        label: &str,
    ) -> (
        [u8; 32],
        [u8; 16],
        crate::membership_log::MemberId,
        crate::membership_log::Entrant,
    ) {
        let writer = [tag; 32];
        let machine = [tag.wrapping_add(0x20); 16];
        let incarnation = [tag; 16];
        let identity = crate::age::Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
        let request = crate::commons::binding::Request::create(
            [0x31; 16],
            [0x42; 32],
            machine,
            incarnation,
            &identity,
            &writer,
        )
        .expect("request");
        let member = request.binding().actor();
        let entrant = crate::membership_log::Entrant {
            request,
            node_key: [tag.wrapping_add(0x80); 32],
            name: format!("mac-{tag}"),
            label: label.to_owned(),
        };
        (writer, incarnation, member, entrant)
    }

    #[test]
    fn two_servers_given_different_records_hold_the_union_after_a_pull() {
        use crate::membership_log::{Body, Evidence, Log, Record, Store, encode_records};

        let root1 = aterm_tempfile::tempdir().expect("tempdir 1");
        let root2 = aterm_tempfile::tempdir().expect("tempdir 2");
        let s1 = run(&options(root1.path(), Duration::from_millis(200)));
        let s2 = run(&options(root2.path(), Duration::from_millis(200)));
        let url1 = format!("http://{}/v1/team", s1.addr);
        let url2 = format!("http://{}/v1/team", s2.addr);

        let (w1, inc1, m1, e1) = record_entrant(1, "m1");
        let (_, _, _, e2) = record_entrant(2, "m2");
        let (_, _, _, e3) = record_entrant(3, "m3");
        let genesis = Record::found(&[7; 32], vec![e1], 1_000).expect("genesis");
        let admit_two = Log::new(genesis.clone())
            .expect("log")
            .draft(
                &m1,
                1_001,
                Body::Admit {
                    entrant: e2,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit 2");
        let admit_three = Log::new(genesis.clone())
            .expect("log")
            .draft(
                &m1,
                1_002,
                Body::Admit {
                    entrant: e3,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit 3");

        crate::transport::post_records(&url1, &encode_records([&genesis, &admit_two]))
            .expect("post s1");
        crate::transport::post_records(&url2, &encode_records([&genesis, &admit_three]))
            .expect("post s2");

        // Negative control ("Last writer wins: a record is lost"): overwriting `s1` with `s2`'s
        // stream instead of taking the grow-only union loses `admit_two`.
        let lww = Store::decode("team", &s2.stored_records("team").encode()).expect("decode s2");
        assert!(
            !lww.contains(admit_two.id()),
            "negative control: last-writer-wins drops s1's divergent record"
        );

        let peer_s2 = [crate::discover::DiscoveredVault {
            url: url2.clone(),
            txt: Some(VaultTxt {
                vault: "team".to_owned(),
                signer: "0000-0000-0000-0000-0000".to_owned(),
                seq: 1,
                head: [0; 32],
                label: Some("m2".to_owned()),
            }),
        }];
        let peer_s1 = [crate::discover::DiscoveredVault {
            url: url1.clone(),
            txt: Some(VaultTxt {
                vault: "team".to_owned(),
                signer: "0000-0000-0000-0000-0000".to_owned(),
                seq: 1,
                head: [0; 32],
                label: Some("m1".to_owned()),
            }),
        }];

        assert_eq!(s1.pull_from_peers(&peer_s2), 1);
        assert_eq!(s2.pull_from_peers(&peer_s1), 1);
        assert_eq!(s1.pull_from_peers(&peer_s2), 0);

        let union = BTreeSet::from([*genesis.id(), *admit_two.id(), *admit_three.id()]);
        assert_eq!(s1.stored_records("team").inventory(), union);
        assert_eq!(s2.stored_records("team").inventory(), union);

        s1.stop();
        s2.stop();

        // Reloading `<store>/team.records` on restart preserves the full union.
        let reloaded = load(root1.path(), 0).expect("reload store");
        assert_eq!(reloaded.records["team"].store.inventory(), union);
    }

    #[test]
    fn a_record_with_a_bad_signature_is_refused() {
        use crate::membership_log::{Body, Draft, Evidence, Log, Record, Store, encode_records};

        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        let url = format!("http://{}/v1/team", running.addr);

        let (w1, inc1, m1, e1) = record_entrant(1, "m1");
        let (_, _, _, e2) = record_entrant(2, "m2");
        let (_, _, _, e3) = record_entrant(3, "m3");
        let (w_forger, inc_forger, _, _) = record_entrant(9, "m9");
        let genesis = Record::found(&[7; 32], vec![e1], 1_000).expect("genesis");
        crate::transport::post_records(&url, genesis.bytes()).expect("post genesis");

        let valid_two = Log::new(genesis.clone())
            .expect("log")
            .draft(
                &m1,
                1_001,
                Body::Admit {
                    entrant: e2,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("valid admit 2");
        let mut bad_sig = valid_two.bytes().to_vec();
        *bad_sig.last_mut().expect("non-empty") ^= 0x01;

        // 1. Corrupted signature alone is refused (`400`) and leaves the store unchanged.
        assert!(crate::transport::post_records(&url, &bad_sig).is_err());
        assert_eq!(
            running.stored_records("team").inventory(),
            BTreeSet::from([*genesis.id()])
        );

        // 2. Valid record bundled with a corrupted record in one POST is refused atomically.
        let valid_three = Log::new(genesis.clone())
            .expect("log")
            .draft(
                &m1,
                1_002,
                Body::Admit {
                    entrant: e3,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("valid admit 3");
        let mut mixed = encode_records([&valid_three]);
        mixed.extend_from_slice(&bad_sig);
        assert!(crate::transport::post_records(&url, &mixed).is_err());
        assert_eq!(
            running.stored_records("team").inventory(),
            BTreeSet::from([*genesis.id()])
        );

        // 3. Validly signed record from a non-member is refused by `Log::insert`.
        let forged_member_record = Draft {
            network: *genesis.id(),
            sequence: 1,
            previous: None,
            parents: vec![*genesis.id()],
            authored_ms: 2_000,
            body: Body::Name {
                name: "forger".to_owned(),
            },
        }
        .sign(&w_forger, inc_forger)
        .expect("self-signed by non-member");
        assert!(crate::transport::post_records(&url, forged_member_record.bytes()).is_err());
        assert_eq!(
            running.stored_records("team").inventory(),
            BTreeSet::from([*genesis.id()])
        );

        // Negative control ("Skip verification: junk is stored"): appending `bad_sig` without
        // signature verification produces a corrupt file that `Store::decode` rejects.
        let mut unverified_raw = genesis.bytes().to_vec();
        unverified_raw.extend_from_slice(&bad_sig);
        assert!(
            Store::decode("team", &unverified_raw).is_err(),
            "negative control: skipping verification stores corrupt bytes"
        );

        running.stop();
    }

    #[test]
    fn a_made_up_genesis_posted_to_alab_binds_no_server_store() {
        use crate::membership_log::Record;

        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        let url = format!("http://{}/v1/{}", running.addr, crate::ALAB_VAULT);

        // A LAN host's genesis, signed with a seed it made up and not the ALab vault's key: with
        // no ALab digest compiled, a store that took it would refuse ALab's real genesis as
        // another network's.
        let (_, _, _, e1) = record_entrant(1, "m1");
        let (_, _, _, e2) = record_entrant(2, "m2");
        let made_up = Record::found(&[0x66; 32], vec![e1], 1_000).expect("genesis");
        assert!(crate::transport::post_records(&url, made_up.bytes()).is_err());
        assert!(running.stored_records(crate::ALAB_VAULT).is_empty());
        assert!(
            !root
                .path()
                .join(format!("{}.records", crate::ALAB_VAULT))
                .exists()
        );

        // A server that holds an `alab` envelope (its own Mac's, or a pinned one) takes the
        // genesis that envelope's key signed, still not the made-up one, and keeps it across a
        // restart.
        put(&running, &envelope(&OLD, 1)).expect("held");
        assert!(crate::transport::post_records(&url, made_up.bytes()).is_err());
        let ours = Record::found(&OLD, vec![e2], 1_000).expect("genesis");
        crate::transport::post_records(&url, ours.bytes()).expect("the vault key's genesis");
        assert!(
            running
                .stored_records(crate::ALAB_VAULT)
                .contains(ours.id())
        );
        // A non-ALab vault whose envelope the server holds also takes only the genesis that
        // envelope's key signed, not one a LAN host made up.
        let team_url = format!("http://{}/v1/team", running.addr);
        transport::publish(
            &Source::Url(team_url.clone()),
            &envelope_with(&OLD, 1, Vec::new(), "team"),
        )
        .expect("held team");
        assert!(crate::transport::post_records(&team_url, made_up.bytes()).is_err());
        assert!(running.stored_records("team").is_empty());
        crate::transport::post_records(&team_url, ours.bytes()).expect("team vault key's genesis");
        assert!(running.stored_records("team").contains(ours.id()));

        running.stop();
        let reloaded = load(root.path(), 0).expect("reload store");
        assert!(
            reloaded.records[crate::ALAB_VAULT]
                .store
                .contains(ours.id())
        );
        assert!(reloaded.records["team"].store.contains(ours.id()));
    }

    /// After the ALab vault's key rotates, a server holds an envelope the new key signed, whose
    /// handoff chain names the old key, while ALab's genesis stays the one the old key signed.
    /// A dormant store keyed to the held envelope's signer alone refused that genesis on every
    /// restart (`load`) and on every new server: it rooted nothing, so it served no records, and
    /// its next write dropped the genesis from `alab.records`. It now takes a genesis signed by
    /// any key of the held envelope's line.
    /// NEGATIVE CONTROL: `held_keys` naming the held envelope's signer alone (main before this
    /// fix) leaves the reloaded store and the new server's store without a log.
    #[test]
    fn a_rotated_vault_keeps_its_old_keys_genesis_across_a_restart_and_on_a_new_server() {
        use crate::membership_log::{Body, Evidence, Log, Record, encode_records};

        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        let url = format!("http://{}/v1/{}", running.addr, crate::ALAB_VAULT);
        let (w1, inc1, m1, e1) = record_entrant(1, "m1");
        let (_, _, _, e2) = record_entrant(2, "m2");
        put(&running, &envelope(&OLD, 1)).expect("held");
        let genesis = Record::found(&OLD, vec![e1], 1_000).expect("genesis");
        let admit = Log::new(genesis.clone())
            .expect("log")
            .draft(
                &m1,
                1_001,
                Body::Admit {
                    entrant: e2,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit");
        let stream = encode_records([&genesis, &admit]);
        crate::transport::post_records(&url, &stream).expect("the vault key's genesis");

        // The vault's key rotates, and the server takes the new key's envelope by its handoff.
        let new = signer_public(&NEW).expect("public");
        let old = signer_public(&OLD).expect("public");
        let chain = vec![(old, endorse(&OLD, "alab", &new).expect("endorsed"))];
        let rotated = envelope_with(&NEW, 2, chain, "alab");
        put(&running, &rotated).expect("the rotated envelope");
        running.stop();

        let reloaded = load(root.path(), 0).expect("reload store");
        let store = &reloaded.records[crate::ALAB_VAULT].store;
        assert!(
            store.log().is_some(),
            "a restart after the rotation refused the genesis the old key signed"
        );
        assert_eq!(
            store.missing(&[]).len(),
            2,
            "a restarted server serves no records"
        );

        // A server started after the rotation holds the new key's envelope from the first.
        let fresh_root = aterm_tempfile::tempdir().expect("tempdir");
        let fresh = run(&options(fresh_root.path(), Duration::from_millis(200)));
        let fresh_url = format!("http://{}/v1/{}", fresh.addr, crate::ALAB_VAULT);
        put(&fresh, &rotated).expect("held");
        let _ = crate::transport::post_records(&fresh_url, &stream);
        assert!(
            fresh.stored_records(crate::ALAB_VAULT).log().is_some(),
            "a server started after the rotation refused ALab's genesis"
        );
        fresh.stop();
    }

    /// A seated key (a removed Mac's too) can sign any number of records at sequence one, each a
    /// head, for about 240 bytes apiece. Every frontier that took them named each head in
    /// `?have=`, 65 bytes apiece, and past about 125 heads the URL outgrew what `aterm-http`
    /// sends (`MAX_URI_LEN`), so every member pass and every server pull failed on every server,
    /// with nobody acting, and records travelled by Commons repair alone. A pull now names the
    /// heads that fit, deepest first: the server answers a subset's gap, a superset of the real
    /// one, and the puller drops what it holds.
    /// NEGATIVE CONTROL: `records_have_url` naming every head (main before this fix) fails the
    /// fetch.
    #[test]
    fn a_frontier_of_two_hundred_forked_heads_still_pulls_records() {
        use crate::membership_log::{Body, Draft, Evidence, Log, Record, Store, decode_records};

        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        let url = format!("http://{}/v1/team", running.addr);
        let (w1, inc1, m1, e1) = record_entrant(1, "m1");
        let (w2, inc2, _, e2) = record_entrant(2, "m2");
        let genesis = Record::found(&[7; 32], vec![e1], 1_000).expect("genesis");
        let mut log = Log::new(genesis.clone()).expect("log");
        let admit = log
            .draft(
                &m1,
                1_001,
                Body::Admit {
                    entrant: e2,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit");
        log.insert(admit.clone()).expect("admit enters");
        let honest = log
            .draft(
                &m1,
                1_002,
                Body::Name {
                    name: "m1".to_owned(),
                },
            )
            .sign(&w1, inc1)
            .expect("honest");
        let forks: Vec<Record> = (0..200u64)
            .map(|i| {
                Draft {
                    network: *genesis.id(),
                    sequence: 1,
                    previous: None,
                    parents: vec![*admit.id()],
                    authored_ms: 2_000 + i,
                    body: Body::Name {
                        name: format!("fork-{i}"),
                    },
                }
                .sign(&w2, inc2)
                .expect("fork")
            })
            .collect();
        let mut local = Store::for_vault("team").expect("store");
        local.insert_many(
            [genesis.clone(), admit.clone()]
                .into_iter()
                .chain(forks.clone()),
        );
        let have = local.frontier();
        assert_eq!(have.len(), 200);
        let mut stream = crate::membership_log::encode_records([&genesis, &admit, &honest]);
        stream.extend_from_slice(&crate::membership_log::encode_records(&forks));
        crate::transport::post_records(&url, &stream).expect("post");

        let pulled = crate::transport::fetch_records(&url, &have, None, HANG_DETECTOR, None)
            .expect("a frontier of 200 heads pulled nothing");
        let ids: BTreeSet<_> = decode_records(&pulled)
            .expect("decode")
            .iter()
            .map(|r| *r.id())
            .collect();
        assert!(
            ids.contains(honest.id()),
            "the honest record did not travel"
        );
        assert!(!ids.contains(genesis.id()) && !ids.contains(admit.id()));
        running.stop();
    }

    #[test]
    fn a_record_whose_parent_exists_nowhere_is_not_sent_on_every_pull() {
        use crate::membership_log::{Body, Draft, Record, decode_records};

        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        let url = format!("http://{}/v1/team", running.addr);

        let (_, _, _, e1) = record_entrant(1, "m1");
        // A founder's key: only a seated signer's record waits for its parents.
        let (w_junk, inc_junk, _, e_junk) = record_entrant(9, "m9");
        let genesis = Record::found(&[7; 32], vec![e1, e_junk], 1_000).expect("genesis");
        crate::transport::post_records(&url, genesis.bytes()).expect("post genesis");
        let orphan = Draft {
            network: *genesis.id(),
            sequence: 1,
            previous: None,
            parents: vec![[0x77; 32]],
            authored_ms: 2_000,
            body: Body::Name {
                name: "orphan".to_owned(),
            },
        }
        .sign(&w_junk, inc_junk)
        .expect("signed");
        crate::transport::post_records(&url, orphan.bytes()).expect("post orphan");
        assert!(running.stored_records("team").contains(orphan.id()));

        // A puller names the complete heads it holds; the orphan never becomes one, so a reply
        // that carried it would carry it again on every pull of every member and server.
        let at_frontier =
            crate::transport::fetch_records(&url, &[*genesis.id()], None, HANG_DETECTOR, None)
                .expect("fetch at the frontier");
        assert!(at_frontier.is_empty(), "the orphan went out again");
        let from_nothing = crate::transport::fetch_records(&url, &[], None, HANG_DETECTOR, None)
            .expect("fetch from nothing");
        assert_eq!(
            decode_records(&from_nothing)
                .expect("decode")
                .iter()
                .map(|r| *r.id())
                .collect::<Vec<_>>(),
            vec![*genesis.id()]
        );

        running.stop();
    }

    #[test]
    fn a_member_long_poll_wakes_on_a_post() {
        use crate::membership_log::{Body, Evidence, Log, Record, decode_records};

        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_secs(30)));
        let url = format!("http://{}/v1/team", running.addr);

        let (w1, inc1, m1, e1) = record_entrant(1, "m1");
        let (_, _, _, e2) = record_entrant(2, "m2");
        let genesis = Record::found(&[7; 32], vec![e1], 1_000).expect("genesis");
        crate::transport::post_records(&url, genesis.bytes()).expect("post genesis");

        // Immediate poll (`wait == None`) at the current frontier returns `204` (`empty`) at once.
        let immediate =
            crate::transport::fetch_records(&url, &[*genesis.id()], None, HANG_DETECTOR, None)
                .expect("immediate fetch");
        assert!(immediate.is_empty());

        // Long-poll (`wait == Some(...)`) parks until a new record is POSTed, then wakes at once.
        let poll_url = url.clone();
        let genesis_id = *genesis.id();
        let poller = std::thread::spawn(move || {
            crate::transport::fetch_records(
                &poll_url,
                &[genesis_id],
                Some(Duration::from_secs(30)),
                HANG_DETECTOR,
                None,
            )
            .expect("long-poll fetch")
        });

        until("the member long-poll is waiting on /records", || {
            running.waiting() == 1
        });

        let admit_two = Log::new(genesis.clone())
            .expect("log")
            .draft(
                &m1,
                1_001,
                Body::Admit {
                    entrant: e2,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit 2");
        crate::transport::post_records(&url, admit_two.bytes()).expect("post admit");

        let woken_bytes = poller.join().expect("poller thread");
        let decoded = decode_records(&woken_bytes).expect("decode woken records");
        assert_eq!(
            decoded.iter().map(|r| *r.id()).collect::<Vec<_>>(),
            vec![*admit_two.id()]
        );

        running.stop();
    }

    #[test]
    fn an_unseeded_records_long_poll_neither_admits_remote_seed_nor_leaks_a_slot() {
        use crate::membership_log::Record;

        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(80)));
        let url = format!("http://{}/v1/unseeded", running.addr);
        let (_, _, _, e1) = record_entrant(1, "m1");
        let genesis = Record::found(&[7; 32], vec![e1], 1_000).expect("genesis");

        // While a local long-poll is parked on `/v1/unseeded/records` before any record exists,
        // a non-local `POST /v1/unseeded/records` is still refused (`403`) because the slot holds
        // no records yet.
        let poll_url = url.clone();
        let poller = std::thread::spawn(move || {
            crate::transport::fetch_records(
                &poll_url,
                &[],
                Some(Duration::from_millis(80)),
                HANG_DETECTOR,
                None,
            )
            .expect("long-poll fetch")
        });
        until("the unseeded long-poll is parked", || {
            running.waiting() == 1
        });
        assert_eq!(
            post_records(&running.shared, "unseeded", genesis.bytes(), false).status,
            403
        );
        let empty = poller.join().expect("poller thread");
        assert!(empty.is_empty());

        // Once the unseeded long-poll ends without any record arriving, the empty slot is
        // removed rather than retained in `state.records`.
        assert!(
            !running
                .shared
                .lock_server_state()
                .records
                .contains_key("unseeded")
        );

        running.stop();
    }

    /// Any LAN host may POST to `/v1/alab/records`, and to a non-ALab vault this server pins.
    /// Before a genesis, ALab's store (`Store::dormant`) and a pinned one (`Store::under`) took
    /// records any key signed under any network, up to `MAX_WAITING - 1` records or about 63 MiB,
    /// rewrote the file whole on each POST that added one and read it back at every start,
    /// though no member is ever served a record before a genesis. A store with no genesis now
    /// takes records only with the genesis that roots them: junk leaves no file and no records,
    /// a file an earlier build filled reads as empty (while serving and at a restart), and the
    /// pinned key's genesis with a record beside it is kept and read back after a restart.
    /// NEGATIVE CONTROL: without `post_records`' no-genesis check (main before this fix) the
    /// junk POST to ALab answers 204 ("junk POST to alab: 204"); with `kept_records` keeping what
    /// it decoded (main before this fix), the file an earlier build filled reads back with its
    /// junk ("the server kept a records file's junk").
    #[test]
    fn a_store_with_no_genesis_takes_posted_records_only_with_the_genesis_that_roots_them() {
        use crate::membership_log::{
            Body, Draft, EpochAudience, Evidence, Log, Record, encode_records,
        };

        let root = aterm_tempfile::tempdir().expect("tempdir");
        let seed = [7u8; 32];
        let mut opts = options(root.path(), Duration::from_millis(200));
        opts.pins = vec![("team".to_owned(), signer_public(&seed).expect("key"))];
        let running = run(&opts);
        let junk = (0..4u64)
            .map(|i| {
                let mut parent = [0x66; 32];
                parent[..8].copy_from_slice(&i.to_le_bytes());
                Draft {
                    network: [0xab; 32],
                    sequence: 1,
                    previous: None,
                    parents: vec![parent],
                    authored_ms: 3_000 + i,
                    body: Body::Put {
                        audience: EpochAudience::Everyone,
                        epoch: [0x44; 32],
                        salt: [0x33; 32],
                        ciphertext: vec![0x5a; 4_096],
                    },
                }
                .sign(&[0xee; 32], [0xee; 16])
                .expect("signed")
            })
            .collect::<Vec<Record>>();
        let junk = encode_records(&junk);
        for vault in [crate::ALAB_VAULT, "team"] {
            let status = post_records(&running.shared, vault, &junk, false).status;
            assert_eq!(status, 422, "junk POST to {vault}: {status}");
            assert!(
                running.stored_records(vault).is_empty(),
                "{vault} kept junk"
            );
            assert!(
                !root.path().join(format!("{vault}.records")).exists(),
                "junk left a {vault}.records file"
            );
        }

        // A file an earlier build let a LAN host fill.
        let earlier = root.path().join("team.records");
        fsx::write_atomic(&earlier, &junk, 0o600).expect("an earlier build's file");
        assert!(
            running.stored_records("team").is_empty(),
            "the server kept a records file's junk"
        );

        let (w1, inc1, m1, e1) = record_entrant(1, "m1");
        let (_, _, _, e2) = record_entrant(2, "m2");
        let genesis = Record::found(&seed, vec![e1], 1_000).expect("genesis");
        let admit = Log::new(genesis.clone())
            .expect("log")
            .draft(
                &m1,
                1_001,
                Body::Admit {
                    entrant: e2,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit");
        let rooted = encode_records([&genesis, &admit]);
        assert_eq!(
            post_records(&running.shared, "team", &rooted, false).status,
            204
        );
        let held = running.stored_records("team");
        assert_eq!(
            held.inventory(),
            [*genesis.id(), *admit.id()].into_iter().collect()
        );
        running.stop();

        let alab_earlier = root.path().join(format!("{}.records", crate::ALAB_VAULT));
        fsx::write_atomic(&alab_earlier, &junk, 0o600).expect("an earlier build's file");
        let reloaded = load(root.path(), 0).expect("reload store");
        assert_eq!(
            reloaded
                .records
                .get("team")
                .map(|slot| slot.store.inventory()),
            Some(held.inventory()),
            "the restarted server skipped its records file"
        );
        assert!(
            reloaded
                .records
                .get(crate::ALAB_VAULT)
                .is_none_or(|slot| slot.store.is_empty()),
            "the server kept a records file's junk"
        );
    }

    /// A POST that adds a record to a store puts only that record on the end of its file, and a
    /// restart reads the file back, even one whose last append a power loss cut short. Before,
    /// each POST that added one rewrote the whole file (up to `MAX_RECORDS_STREAM`), so a key
    /// once seated (a removed Mac's among them) posting one waiting record at a time wrote about
    /// n²/2 bytes. The next POST after such a restart writes the file whole, not onto the torn
    /// bytes, so a second restart keeps every record.
    /// NEGATIVE CONTROLS: writing the file whole on every POST that adds (main before this fix)
    /// replaces it, and the test fails "the POST rewrote the records file"; reading the file
    /// strictly at a restart (`Store::ingest`, main before this fix) skips the whole file for
    /// its torn tail, and the test fails "the restarted server kept no records"; appending
    /// without `post_records`' check that the file is as long as the store's records puts the
    /// next record after the torn bytes, and the test fails "the POST after a torn tail left a
    /// file a reader refuses".
    #[test]
    fn a_post_that_adds_a_record_appends_only_its_bytes_to_the_file() {
        use crate::membership_log::{Body, Evidence, Log, Record, decode_records, encode_records};
        use std::os::unix::fs::MetadataExt;

        let root = aterm_tempfile::tempdir().expect("tempdir");
        let seed = [7u8; 32];
        let mut opts = options(root.path(), Duration::from_millis(200));
        opts.pins = vec![("team".to_owned(), signer_public(&seed).expect("key"))];
        let running = run(&opts);
        let (w1, inc1, m1, e1) = record_entrant(1, "m1");
        let (_, _, _, e2) = record_entrant(2, "m2");
        let (_, _, _, e3) = record_entrant(3, "m3");
        let (_, _, _, e4) = record_entrant(4, "m4");
        let genesis = Record::found(&seed, vec![e1], 1_000).expect("genesis");
        let admit = |log: &Log, entrant, ms| {
            log.draft(
                &m1,
                ms,
                Body::Admit {
                    entrant,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit")
        };
        let mut log = Log::new(genesis.clone()).expect("log");
        let two = admit(&log, e2, 1_001);
        log.insert(two.clone()).expect("two");
        let three = admit(&log, e3, 1_002);

        let post = |body: &[u8]| post_records(&running.shared, "team", body, false).status;
        assert_eq!(post(&encode_records([&genesis, &two])), 204);
        let path = root.path().join("team.records");
        let before = std::fs::read(&path).expect("stored");
        let inode = std::fs::metadata(&path).expect("stored").ino();
        assert_eq!(post(three.bytes()), 204);
        assert_eq!(
            std::fs::metadata(&path).expect("stored").ino(),
            inode,
            "the POST rewrote the records file"
        );
        assert_eq!(
            std::fs::read(&path).expect("stored"),
            [before.as_slice(), three.bytes()].concat()
        );
        let held = running.stored_records("team");
        assert_eq!(held.len(), 3);
        running.stop();

        // A power loss cut the next append short.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("the records file");
        file.write_all(&three.bytes()[..40]).expect("a torn tail");
        drop(file);
        let reloaded = load(root.path(), 0).expect("reload store");
        assert_eq!(
            reloaded
                .records
                .get("team")
                .map(|slot| slot.store.inventory()),
            Some(held.inventory()),
            "the restarted server kept no records"
        );

        // The next POST, on the restarted server, adds a record past the torn bytes.
        log.insert(three.clone()).expect("three");
        let four = admit(&log, e4, 1_003);
        let running = run(&opts);
        assert_eq!(
            post_records(&running.shared, "team", four.bytes(), false).status,
            204
        );
        let held = running.stored_records("team");
        assert_eq!(held.len(), 4);
        running.stop();
        let stored = std::fs::read(&path).expect("stored");
        assert!(
            decode_records(&stored).is_ok_and(|records| records.len() == 4),
            "the POST after a torn tail left a file a reader refuses"
        );
        let reloaded = load(root.path(), 0).expect("reload store");
        assert_eq!(
            reloaded
                .records
                .get("team")
                .map(|slot| slot.store.inventory()),
            Some(held.inventory()),
            "the second restart lost a record"
        );
    }

    /// A store a key once seated (a removed Mac's among them) filled to `MAX_RECORDS` with
    /// records waiting on parents that never arrive lets one go for each record a member posts
    /// whose parents it holds. The POST that does writes the file whole, holding only the
    /// store's records, so the file stays one a reader takes and a restart keeps the member's
    /// record.
    /// NEGATIVE CONTROL: appending without `post_records`' check that no record went puts the
    /// member's record after `MAX_RECORDS` others, and the test fails "the file holds more than
    /// a reader takes".
    #[test]
    fn a_post_that_makes_a_full_store_let_a_waiting_record_go_writes_the_file_whole() {
        use crate::membership_log::{
            Body, Draft, Evidence, Log, MAX_RECORDS, MAX_RECORDS_STREAM, Record, decode_records,
            encode_records,
        };

        let root = aterm_tempfile::tempdir().expect("tempdir");
        let seed = [7u8; 32];
        let mut opts = options(root.path(), Duration::from_millis(200));
        opts.pins = vec![("team".to_owned(), signer_public(&seed).expect("key"))];
        let running = run(&opts);
        let (w1, inc1, m1, e1) = record_entrant(1, "m1");
        let (_, _, _, e2) = record_entrant(2, "m2");
        let genesis = Record::found(&seed, vec![e1], 1_000).expect("genesis");
        let waiting = (0..MAX_RECORDS - 1).map(|i| {
            let mut parent = [0x66; 32];
            parent[..8].copy_from_slice(&(i as u64).to_le_bytes());
            Draft {
                network: *genesis.id(),
                sequence: 1,
                previous: None,
                parents: vec![parent],
                authored_ms: 2_000 + i as u64,
                body: Body::Name {
                    name: format!("w{i}"),
                },
            }
            .sign(&w1, inc1)
            .expect("signed")
        });
        let full = encode_records(
            &std::iter::once(genesis.clone())
                .chain(waiting)
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            post_records(&running.shared, "team", &full, false).status,
            204
        );
        assert_eq!(running.stored_records("team").len(), MAX_RECORDS);

        let admit = Log::new(genesis)
            .expect("log")
            .draft(
                &m1,
                1_001,
                Body::Admit {
                    entrant: e2,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit");
        assert_eq!(
            post_records(&running.shared, "team", admit.bytes(), false).status,
            204
        );
        let held = running.stored_records("team");
        assert!(
            held.inventory().contains(admit.id()),
            "the store took the admit"
        );
        running.stop();
        let stored = std::fs::read(root.path().join("team.records")).expect("stored");
        assert!(
            stored.len() <= MAX_RECORDS_STREAM && decode_records(&stored).is_ok(),
            "the file holds more than a reader takes"
        );
        let reloaded = load(root.path(), 0).expect("reload store");
        assert_eq!(
            reloaded
                .records
                .get("team")
                .map(|slot| slot.store.inventory()),
            Some(held.inventory()),
            "the restarted server lost the admit"
        );
    }

    /// A browsed peer at `addr` advertising vault `vault`, as the standing browse hands it to
    /// [`pull_from_peers_shared`].
    fn peer_at(addr: SocketAddr, vault: &str) -> crate::discover::DiscoveredVault {
        crate::discover::DiscoveredVault {
            url: format!("http://{addr}/v1/{vault}"),
            txt: Some(VaultTxt {
                vault: vault.to_owned(),
                signer: "0000-0000-0000-0000-0000".to_owned(),
                seq: 1,
                head: [0; 32],
                label: Some("m9".to_owned()),
            }),
        }
    }

    /// Read one request's head from `stream` (the hang detector bounds it): what arrived of it.
    fn read_request_head(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(HANG_DETECTOR))
            .expect("timeout");
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(1) => head.push(byte[0]),
                _ => break,
            }
        }
        String::from_utf8_lossy(&head).into_owned()
    }

    /// The connections `listener` accepts, handed over as they arrive: a test reads them within
    /// the hang detector, so a peer the code under test never dials fails the test rather than
    /// hanging it in `accept`. The accepting thread is left to end with the test binary.
    fn accepted(listener: TcpListener) -> std::sync::mpsc::Receiver<TcpStream> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                if tx.send(stream).is_err() {
                    return;
                }
            }
        });
        rx
    }

    const NOT_FOUND: &[u8] =
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    /// A peer whose envelope pull just failed (here one that drops every connection, as a Mac
    /// gone from the LAN or a server that is down does) is asked nothing more in that pass: on
    /// main the pass asked it for its records too, so each such peer cost the vault-tend thread
    /// two failed pulls per vault where one tells it the peer is not there.
    /// NEGATIVE CONTROL: asking a peer for its records whatever its envelope pull did (main before
    /// this fix) dials the dropping peer twice.
    #[test]
    fn a_peer_whose_envelope_pull_failed_is_not_asked_for_its_records() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let mut opts = options(root.path(), Duration::from_millis(200));
        opts.pins = vec![("team".to_owned(), [0x5a; 32])];
        let running = run(&opts);
        let dropping = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = dropping.local_addr().expect("addr");
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let peer = std::thread::spawn(move || {
            let mut dials = 0usize;
            for stream in dropping.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                dials += 1;
                drop(stream);
            }
            dials
        });

        assert_eq!(running.pull_from_peers(&[peer_at(addr, "team")]), 0);
        stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(addr);
        let dials = peer.join().expect("the dropping peer");
        assert_eq!(
            dials, 1,
            "a peer whose envelope pull failed was asked for its records in the same pass"
        );
        running.stop();
    }

    /// A peer that takes a pull and never answers (a Mac asleep mid-answer, a stalled server)
    /// holds no other peer's pull behind it: on main the vault-tend thread asked its peers one
    /// after another, so one such peer held every peer after it for its whole budget, twice.
    /// Here every pull may wait past the hang detector, so only the silent peer's own answer ends
    /// its pull, and it answers only once the other peer's records landed.
    /// NEGATIVE CONTROL: pulling from the peers one after another (main before this fix) asks the
    /// answering peer only after the silent one answered, and the hang detector fires.
    #[test]
    fn a_peer_that_never_answers_holds_no_other_peers_pull_behind_it() {
        use crate::membership_log::Record;

        let root = aterm_tempfile::tempdir().expect("tempdir");
        let seed = [7u8; 32];
        let mut opts = options(&root.path().join("puller"), Duration::from_millis(200));
        opts.pins = vec![("team".to_owned(), signer_public(&seed).expect("key"))];
        let running = run_tuned(
            &opts,
            Tuning {
                peer_pull: HANG_DETECTOR * 2,
                ..Tuning::default()
            },
        );
        let answering = run(&options(
            &root.path().join("answering"),
            Duration::from_millis(200),
        ));
        let (_, _, _, founder) = record_entrant(1, "m1");
        let genesis = Record::found(&seed, vec![founder], 1_000).expect("genesis");
        transport::post_records(
            &format!("http://{}/v1/team", answering.addr),
            genesis.bytes(),
        )
        .expect("the answering peer holds a genesis");

        let silent = TcpListener::bind("127.0.0.1:0").expect("bind");
        let silent_addr = silent.local_addr().expect("addr");
        let dialed = accepted(silent);
        let landed = root.path().join("puller").join("team.records");
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let peer = std::thread::spawn(move || {
            let Ok(mut first) = dialed.recv_timeout(HANG_DETECTOR) else {
                return None;
            };
            let asked = !read_request_head(&mut first).is_empty();
            let deadline = Instant::now() + HANG_DETECTOR;
            let mut heard = landed.exists();
            while !heard && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
                heard = landed.exists();
            }
            let _ = first.write_all(NOT_FOUND);
            drop(first);
            // Answer what else the pass asks until the test is done with it.
            while let Ok(mut stream) = dialed.recv_timeout(HANG_DETECTOR) {
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                read_request_head(&mut stream);
                let _ = stream.write_all(NOT_FOUND);
            }
            Some((asked, heard))
        });

        running.pull_from_peers(&[
            peer_at(silent_addr, "team"),
            peer_at(answering.addr, "team"),
        ]);
        stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(silent_addr);
        let silent_peer = peer.join().expect("the silent peer");
        assert!(
            matches!(silent_peer, Some((true, _))),
            "the pass never asked the silent peer"
        );
        assert!(
            matches!(silent_peer, Some((_, true))),
            "the answering peer was asked only once the silent peer's pull had ended"
        );
        assert!(running.stored_records("team").contains(genesis.id()));
        running.stop();
        answering.stop();
    }

    /// What the peers of [`a_pass_reads_at_most_two_peers_records_at_once`] saw: the records
    /// pulls they hold now, the most they held at once, and whether the test let them answer.
    #[derive(Default)]
    struct HeldPulls {
        in_flight: usize,
        most: usize,
        released: bool,
    }

    /// A pass reads at most [`RECORDS_PULLS_AT_ONCE`] peers' records at once, however many peers
    /// it pulls: each body is up to 64 MiB and is decoded again where it is stored. Before, every
    /// peer's pull read its records at once, so a LAN host that advertised 16 instances of itself
    /// had a vault server (some owner's working Mac) hold 16 such bodies, over 1 GiB, on every
    /// tend pass. Here each peer holds its records answer until the test lets it go, and every
    /// pull may wait past the hang detector, so the pass can neither finish nor time a pull out
    /// while the test counts.
    /// NEGATIVE CONTROL: pulling records without a slot (no `PullSlots::take` in
    /// `pull_from_peer`) has all four peers' records pulls in flight at once.
    #[test]
    fn a_pass_reads_at_most_two_peers_records_at_once() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let mut opts = options(root.path(), Duration::from_millis(200));
        opts.pins = vec![("team".to_owned(), [0x5a; 32])];
        let running = run_tuned(
            &opts,
            Tuning {
                peer_pull: HANG_DETECTOR * 2,
                ..Tuning::default()
            },
        );
        let held = Arc::new((Mutex::new(HeldPulls::default()), Condvar::new()));
        let peers: Vec<_> = (0..4)
            .map(|_| {
                let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
                let addr = listener.local_addr().expect("addr");
                let dialed = accepted(listener);
                let held = Arc::clone(&held);
                std::thread::spawn(move || {
                    while let Ok(mut stream) = dialed.recv_timeout(HANG_DETECTOR) {
                        let held = Arc::clone(&held);
                        std::thread::spawn(move || {
                            let head = read_request_head(&mut stream);
                            if head.starts_with("GET ") && head.contains("/records") {
                                let (lock, bell) = &*held;
                                let mut pulls = lock.lock().expect("pulls");
                                pulls.in_flight += 1;
                                pulls.most = pulls.most.max(pulls.in_flight);
                                let deadline = Instant::now() + HANG_DETECTOR;
                                while !pulls.released {
                                    let Some(left) =
                                        deadline.checked_duration_since(Instant::now())
                                    else {
                                        break;
                                    };
                                    pulls = bell.wait_timeout(pulls, left).expect("pulls").0;
                                }
                                pulls.in_flight -= 1;
                            }
                            let _ = stream.write_all(NOT_FOUND);
                        });
                    }
                });
                peer_at(addr, "team")
            })
            .collect();
        let waiting = || running.shared.records_pulls.waiting.load(Ordering::SeqCst);
        let in_flight = || held.0.lock().expect("pulls").in_flight;

        std::thread::scope(|scope| {
            let pass = scope.spawn(|| running.pull_from_peers(&peers));
            until(
                "two records pulls in flight and two waiting for a slot, or more in flight",
                || in_flight() > 2 || (in_flight() == 2 && waiting() == 2),
            );
            let (now, queued) = (in_flight(), waiting());
            held.0.lock().expect("pulls").released = true;
            held.1.notify_all();
            assert_eq!(pass.join().expect("the pass"), 0);
            assert_eq!(
                now, RECORDS_PULLS_AT_ONCE,
                "records pulls in flight at once"
            );
            assert_eq!(queued, 2, "records pulls waiting for a slot");
        });
        assert_eq!(held.0.lock().expect("pulls").most, RECORDS_PULLS_AT_ONCE);
        assert_eq!(waiting(), 0);
        running.stop();
    }
}
