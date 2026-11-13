// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The keyless HTTP vault server ([`serve`]): it holds each vault's latest signed envelope,
//! takes a PUT only when its signer and handoff chain check out, holds long-polls, and queues
//! join requests, under per-server and per-address caps.

mod heard;
#[path = "server/records_input.rs"]
mod records_input;

#[cfg(test)]
#[path = "server/persistence_tests.rs"]
mod persistence_tests;

#[cfg(test)]
#[path = "server/reply_tests.rs"]
mod reply_tests;

#[cfg(test)]
#[path = "server/crossing_tests.rs"]
mod crossing_tests;

#[cfg(test)]
#[path = "server/takes_over_tests.rs"]
mod takes_over_tests;

use crate::envelope::{self, MAX_ENVELOPE};
use crate::transport::{JoinRequest, parse_join_line};
use crate::wake::{Bell, Wake};
use crate::{Error, fsx};
use aterm_mesh_core::no_lost_edit as nle;
use aterm_uds::power::Power;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{self, Read, Write};
use std::net::{
    IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket,
};
use std::os::fd::AsFd as _;
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
pub use crate::transport::MAX_HEARD;
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
/// The conservative allowance for a server stopping for a new program: the answer drain
/// ([`DRAIN_DEADLINE`]), plus stopping bookkeeping and the accept loop's cancellation and join.
/// This is a takeover margin, not a bound on a stalled filesystem. Its registrations end,
/// and a holder of `alab-secrets` leaves its [`HeldStamp`], only at that end
/// ([`Children`]' drop).
const STOP_TAKES: Duration = Duration::from_secs(15);
/// How often a server whose address another process holds tries it again
/// ([`bind_when_free`]).
const BIND_AGAIN_EVERY: Duration = Duration::from_secs(60);
/// The file a server that stops while it holds `alab-secrets` leaves in its store
/// ([`HeldStamp`]), for the server that starts in its place ([`AlabNameWatcher::resume`]).
const ALAB_HELD_FILE: &str = ".alab-held";
/// How long after a server that held `alab-secrets` stopped the server started in its place
/// may take the name back at once ([`AlabNameWatcher::resume`]): its first decision must come
/// within this of the stamp. Every other server that follows the probe takes the name only
/// after [`ALAB_FALLBACK_AFTER`] in which nothing answered there, counted from a round of its
/// own after the holder stopped answering, allowing [`STOP_TAKES`] before the stamp;
/// one round ([`ADVERTISE_EVERY`]) of what is left is kept as a margin. A device that registers
/// the name with no probe can hold it anyway, and takes it from this one by mDNS
/// ([`AlabNameWatcher::lost_name`]).
const ALAB_RESUME_WITHIN: Duration = Duration::from_secs(
    ALAB_FALLBACK_AFTER.as_secs() - STOP_TAKES.as_secs() - ADVERTISE_EVERY.as_secs(),
);

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
    /// Resolved `alab-secrets` / `alab-secrets (<N>)` registrations from this bounded
    /// browse pass. A rotating watch visits the remaining holders on later passes.
    pub holders: &'a [crate::discover::DiscoveredVault],
    /// Monotone wall-clock instant for fallback duration measurement.
    pub now: Instant,
}

/// Lease-free controller deciding whether this server should advertise `alab-secrets.local`
/// alongside its own `<label>.local` (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1 F7).
///
/// - Every server probes `alab-secrets.local` itself and takes the name only after
///   [`ALAB_FALLBACK_AFTER`] (90 s) in which nothing answered there with a verified ALab envelope
///   at least as new as its own, so a returning server leaves an up-to-date holder in place.
/// - When two `>= 0.146` holders collide after a partition heal, every server settles them by one
///   strict order of verified envelope sequence descending, then discovery label ascending;
///   the loser withdraws and serves under its own label. TXT sequence is not evidence, and a
///   verified public envelope does not authenticate the serving device or its label.
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
    /// The `seq` a [`HeldStamp`] let this watcher take the name back at, on its first
    /// [`Self::decide`], and the instant by which that decision must come ([`Self::resume`]).
    resumed: Option<(u64, Instant)>,
}

/// What a server that stopped while it held `alab-secrets` left in its store
/// ([`ALAB_HELD_FILE`], [`Children`]' drop): when it stopped (unix milliseconds) and the `seq`
/// of the ALab envelope it advertised under the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeldStamp {
    pub(crate) at_ms: u64,
    pub(crate) seq: u64,
}

impl HeldStamp {
    fn line(self) -> String {
        format!("{} {}\n", self.at_ms, self.seq)
    }

    fn parse(text: &str) -> Option<HeldStamp> {
        let (at_ms, seq) = text.strip_suffix('\n')?.split_once(' ')?;
        Some(HeldStamp {
            at_ms: at_ms.parse().ok()?,
            seq: seq.parse().ok()?,
        })
    }
}

/// Leave `stamp` at `path` as this server stops ([`HeldStamp`]); the log says when it cannot.
fn leave_held_stamp(path: &Path, stamp: HeldStamp) {
    if let Err(e) = fsx::write_atomic(path, stamp.line().as_bytes(), 0o600) {
        note(&format!(
            "could not note that this server held {}.local as it stopped: {e}",
            crate::alab_advertised()
        ));
    }
}

/// The stamp a server that stopped left at `path`, removed as it is read: a server reads it
/// once, as it starts ([`AlabNameWatcher::resume`]).
fn take_held_stamp(path: &Path) -> Option<HeldStamp> {
    let bytes = fsx::read_owned(path, 64).ok().flatten();
    let _ = std::fs::remove_file(path);
    HeldStamp::parse(std::str::from_utf8(&bytes?).ok()?)
}

/// The stamp this server leaves as it stops at `now_ms` ([`HeldStamp`]): one while a
/// registration of `alab-secrets` it holds in `slot` is still up, at the highest `seq` its TXT
/// advertises. A registration a will-sleep took ([`take_for_sleep`]), or one that ended by
/// itself, leaves none.
fn held_alab(slot: &mut AdvertSlot, now_ms: u64) -> Option<HeldStamp> {
    if slot.held_name.as_deref() != Some(crate::alab_advertised()) {
        return None;
    }
    slot.held
        .iter_mut()
        .filter_map(|(_, txt, child)| {
            let seq = txt.as_ref()?.seq;
            (!child.ended()).then_some(seq)
        })
        .max()
        .map(|seq| HeldStamp { at_ms: now_ms, seq })
}

impl AlabNameWatcher {
    /// Create a fresh watcher.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The watcher of a server that starts at `now_ms` (unix milliseconds; `now` is the same
    /// instant) where one stopped, which left `stamp` if it held `alab-secrets` as it stopped
    /// (an update that replaced the program, a move of the server's job); this Mac's last sleep
    /// began at `slept_ms` (unix milliseconds). When this Mac has not slept since that server
    /// stopped, a first [`Self::decide`] that comes within [`ALAB_RESUME_WITHIN`] of the stop
    /// takes the name back at once if this server's ALab envelope is at least as new as the one
    /// advertised there, where a new watcher waits [`ALAB_FALLBACK_AFTER`] first: no other
    /// server that follows the probe takes the name in that time, and a device that holds it
    /// anyway takes it from this one as at any registration ([`Self::lost_name`]). A first
    /// decision that comes later (a round that decided nothing for a while) takes nothing back.
    /// A Mac that slept since gave the name up as it slept ([`Self::slept`]), so another may
    /// hold it now, and its server competes for it by the probe and [`ALAB_FALLBACK_AFTER`] as
    /// any server that does not hold it does.
    pub(crate) fn resume(
        stamp: Option<HeldStamp>,
        now_ms: u64,
        now: Instant,
        slept_ms: Option<u64>,
    ) -> Self {
        let resumed = stamp
            .filter(|stamp| slept_ms.is_none_or(|slept| slept < stamp.at_ms))
            .and_then(|stamp| {
                let age = Duration::from_millis(now_ms.checked_sub(stamp.at_ms)?);
                Some((stamp.seq, now + ALAB_RESUME_WITHIN.checked_sub(age)?))
            });
        Self {
            resumed,
            ..Self::default()
        }
    }

    /// Whether this server currently holds `alab-secrets`.
    #[must_use]
    pub const fn is_holding(&self) -> bool {
        self.holding
    }

    /// This server's `alab-secrets` registration ended by itself while it held the name, and
    /// another device answers to it ([`reregister_roles`]). mDNS keeps one host per name, and
    /// mDNSResponder gives a proxy host name up to a device that already answers to it. That device's
    /// registration also hides this server's `_alab-vault._tcp` instance, so neither holder
    /// hears the other and the (`seq`, `label`) order in [`Self::decide`] never meets them:
    /// the name stays with the one mDNS kept. This lets it go and listens again. The
    /// next [`Self::decide`] probes `alab-secrets.local`, stays off while a holder answers there,
    /// and takes the name again only after [`ALAB_FALLBACK_AFTER`] in which none does. The heads
    /// still meet: the holder pulls a newer one from this server's own-label instance (F6).
    pub fn lost_name(&mut self) {
        self.holding = false;
        self.failing_since = None;
        self.resumed = None;
    }

    /// This Mac went to sleep, and this server ended its registrations before it let the Mac
    /// sleep, so mDNSResponder hands a Bonjour Sleep Proxy no `alab-secrets` record to answer
    /// for while nothing serves it. This lets the name go, as [`Self::lost_name`] does, and
    /// forgets how long nothing answered there (a sleep stops that clock): on wake the next
    /// [`Self::decide`] competes for the name by the same probe and [`ALAB_FALLBACK_AFTER`] as
    /// any server that does not hold it. A server that started where a holder stopped
    /// ([`Self::resume`]) and slept before its first decision takes nothing back either.
    pub fn slept(&mut self) {
        self.holding = false;
        self.failing_since = None;
        self.resumed = None;
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
        let resumed = self
            .resumed
            .take()
            .filter(|&(_, by)| input.now <= by)
            .map(|(seq, _)| seq);
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
        if resumed.is_some_and(|seq| own_seq >= seq) {
            self.holding = true;
            self.failing_since = None;
        }

        let peer_outranks = |holders: &[crate::discover::DiscoveredVault],
                             probe: &mut dyn FnMut(&str) -> Option<u64>|
         -> bool {
            for holder in holders.iter().take(crate::discover::MAX_CANDIDATES) {
                match &holder.txt {
                    Some(txt) if txt.vault == crate::ALAB_VAULT => {
                        if let Some(peer_label) = txt.label.as_deref()
                            && let Some(peer_rank) = label_rank(peer_label)
                        {
                            if peer_rank.1 == own_rank.1 {
                                continue;
                            }
                            // TXT is a discovery hint, not evidence that this endpoint holds
                            // that version. Only the verified response participates in rank.
                            if probe(&holder.url).is_some_and(|seq| {
                                seq > own_seq || (seq == own_seq && peer_rank < own_rank)
                            }) {
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

        let holder_answers = probe_alab(crate::alab_advertised()).is_some_and(|seq| seq >= own_seq)
            || input
                .holders
                .iter()
                .take(crate::discover::MAX_CANDIDATES)
                .any(|holder| {
                    !holder.url.contains("alab-secrets.local")
                        && probe_alab(&holder.url).is_some_and(|seq| seq >= own_seq)
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
    let program = crate::program_watch::ProgramWatch::current();
    let stopping = || stop_reason(&signalled, program.replaced());
    let Some(listener) = bind_when_free(
        options.listen,
        BIND_AGAIN_EVERY,
        &mut |addr| aterm_uds::kept_out(|| TcpListener::bind(addr)),
        &stopping,
    )?
    else {
        return Ok(());
    };
    let running = launch(listener, &options, Tuning::default())?;
    note(&format!(
        "serving the vaults in {} on {}",
        options.store.display(),
        running.addr
    ));
    let bonjour = Bonjour::here(role_decide.is_some() || options.advertise.is_some());
    let unheld = unheld_note(
        &running.shared.lock_server_state(),
        &options.pins,
        bonjour,
        role_decide.is_some(),
    );
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
    let health_paths = client_paths.clone();
    let health_address = running.addr;
    children.tend_with_role(options.keep_awake, advertise, client_paths);
    let result = supervise(running, PROGRAM_EVERY, &|| {
        if let Some(paths) = &health_paths {
            let _ = crate::service_health::bound_server(paths, health_address);
        }
        stopping()
    });
    drop(children);
    result
}

/// A listener on `addr` (made by `bind`) once no other process holds the address. Two logins
/// on one Mac that are both ALab members each run a vault server on `0.0.0.0:7484`, and the
/// one that starts second finds the address taken by the first, which serves the same vaults
/// to both. Exiting there let launchd start it again every 10 s for good; this waits instead,
/// trying again every `every`, and looks at least every [`PROGRAM_EVERY`] whether it should
/// stop (`stopping`: a stop signal, which also wakes this thread, or its program replaced on
/// disk), when it says why and returns `None`. The log says once that it waits, and again when
/// it binds. Any other failure to bind is the error it was.
fn bind_when_free(
    addr: SocketAddr,
    every: Duration,
    bind: &mut dyn FnMut(SocketAddr) -> io::Result<TcpListener>,
    stopping: &dyn Fn() -> Option<&'static str>,
) -> crate::Result<Option<TcpListener>> {
    let mut waited = false;
    loop {
        match bind(addr) {
            Ok(listener) => {
                if waited {
                    note(&format!("{addr} is free now"));
                }
                return Ok(Some(listener));
            }
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
                if !std::mem::replace(&mut waited, true) {
                    note(&format!(
                        "another process listens on {addr} (another login's vault server on \
                         this Mac, say); trying again every {}s",
                        every.as_secs()
                    ));
                }
                let again = Instant::now() + every;
                loop {
                    if let Some(why) = stopping() {
                        note(why);
                        return Ok(None);
                    }
                    let Some(left) = again
                        .checked_duration_since(Instant::now())
                        .filter(|left| !left.is_zero())
                    else {
                        break;
                    };
                    std::thread::park_timeout(left.min(PROGRAM_EVERY));
                }
            }
            Err(e) => return Err(Error::new(format!("listening on {addr}: {e}"))),
        }
    }
}

/// Why a service process stops now, if it does: a stop signal came (`signalled`), or the
/// program it started from was replaced on disk (`replaced`).
fn stop_reason(signalled: &AtomicBool, replaced: bool) -> Option<&'static str> {
    if signalled.load(Ordering::SeqCst) {
        Some("stopped by a signal")
    } else if replaced {
        Some("this program was replaced on disk; exiting so the service starts the new one")
    } else {
        None
    }
}

/// Run until the accept loop ends or the startup watch says to stop. The same watch covers
/// binding and supervision, so an update between them cannot establish a new baseline.
fn supervise(
    running: Running,
    every: Duration,
    stopping: &dyn Fn() -> Option<&'static str>,
) -> crate::Result<()> {
    loop {
        if running.accept.as_ref().is_none_or(JoinHandle::is_finished) {
            return running.wait();
        }
        if let Some(why) = stopping() {
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
    cancel: Option<io::PipeWriter>,
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
            if let Some(coverage) = state.heard_coverage.take()
                && let Err(error) = coverage.finish(&state.heard, (self.shared.tuning.now)())
            {
                note(&format!("heard coverage was not closed cleanly: {error}"));
            }
        }
        drop(self.cancel.take());
        if let Some(accept) = self.accept.take() {
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

    /// Handle one connected stream synchronously through the actual HTTP request path.
    /// This lets a fixture measure its caller thread without racing the accept loop.
    ///
    /// # Errors
    /// The connected stream's peer address cannot be read.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn test_handle_stream(&self, stream: TcpStream) -> io::Result<()> {
        let peer = stream.peer_addr()?.ip();
        handle(&self.shared, stream, peer);
        Ok(())
    }

    /// Observe the existing records slot's durable prefix without refreshing it from disk.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn test_records_on_file(&self, vault: &str) -> Option<usize> {
        self.shared
            .lock_server_state()
            .records
            .get(vault)
            .and_then(|slot| slot.on_file)
    }

    /// Persist an already accepted Store through the actual append/rewrite operation.
    /// This holds the normal write fences, without installing a Store or refreshing a file.
    ///
    /// # Errors
    /// The snapshot's file has changed, or the append and fallback rewrite cannot persist
    /// the accepted records.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn test_persist_records(
        &self,
        name: &str,
        accepted: &crate::membership_log::Store,
        stamp: Option<fsx::FileStamp>,
        on_file: Option<usize>,
    ) -> crate::Result<Option<fsx::FileStamp>> {
        let _write = self
            .shared
            .record_writes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let _writing = Writing::mark(&self.shared, name);
        persist_records(&self.shared, name, accepted, stamp, on_file)
            .map_err(|failure| failure.error)
    }

    /// How many long-polls it holds now.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.test_waiting()
    }

    /// Observe the actual held-request counter in an isolated protocol qualification.
    /// No request is started or woken by this reading.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn test_waiting(&self) -> usize {
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

/// [`start`] with `budget` for each pull from a current peer in place of `PEER_PULL_BUDGET`
/// ([`Running::pull_from_peers`]): a test whose own peer must answer passes its hang detector.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn start_with_peer_pull(
    listener: std::net::TcpListener,
    options: &Options,
    budget: Duration,
) -> crate::Result<Running> {
    launch(
        listener,
        options,
        Tuning {
            peer_pull: budget,
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
    let mut state = load_with_pins(&options.store, (tuning.now)(), &options.pins)?;
    state.heard_coverage = Some(heard::Coverage::begin(
        &options.store,
        (tuning.now)(),
        &mut state.heard,
    )?);
    let _ = aterm_uds::keep_out_of_children(&listener);
    let (cancelled, cancel) = aterm_uds::pipe()?;
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
        records_pulls: PullSlots::new(RECORDS_PULLS_AT_ONCE),
        envelope_pulls: PullSlots::new(ENVELOPE_PULLS_AT_ONCE),
        untaken: crate::membership_log::untaken::Untaken::new(),
        advertiser: Mutex::new(None),
        records_boot: aterm_uds::rand::hex_token::<16>()?,
        #[cfg(any(test, debug_assertions))]
        waiting: AtomicUsize::new(0),
        #[cfg(test)]
        served: AtomicUsize::new(0),
        #[cfg(test)]
        envelope_gets: AtomicUsize::new(0),
        #[cfg(test)]
        join_gets: AtomicUsize::new(0),
        #[cfg(test)]
        record_wakes: AtomicUsize::new(0),
        #[cfg(test)]
        record_looks: AtomicUsize::new(0),
        #[cfg(test)]
        post_clones: AtomicUsize::new(0),
        #[cfg(test)]
        post_verifies: AtomicUsize::new(0),
        #[cfg(test)]
        verifying: Mutex::new(None),
        #[cfg(test)]
        appends_fail: AtomicBool::new(false),
        #[cfg(test)]
        appends_tear: AtomicBool::new(false),
        #[cfg(test)]
        appended: Mutex::new(None),
        #[cfg(test)]
        writes_fail: AtomicBool::new(false),
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
            .spawn(move || accept_loop(&listener, &cancelled, &shared, &stop, &refusals))?
    };
    Ok(Running {
        addr,
        stop,
        shared,
        cancel: Some(cancel),
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
    /// The records file as this server last read or wrote it (`docs/DESIGN-fluid-swarm-
    /// 2026-10-28.md` §4.1 F23, rule 2): a look that finds the same stamp reads nothing
    /// ([`look_at_records`]). `None` until this server read, wrote or was refused one.
    stamp: Option<fsx::FileStamp>,
    /// `Some(n)` when the file holds exactly the records of the log's first `n` (as a set: a
    /// reload may order them otherwise), so what the log holds past them can go on its end.
    on_file: Option<usize>,
    /// A record application is writing the file ([`apply_records`], [`Writing`]): a look reads nothing, since
    /// what the file holds past the stamp may not be durable yet, and reading it would take
    /// those records into the store and verify the whole file under `server_state`. The POST
    /// stamps the slot when its write is done.
    writing: bool,
    wake: Arc<Condvar>,
}

impl RecordSlot {
    /// A slot for `store` that knows no records file.
    fn new(store: crate::membership_log::Store) -> Self {
        Self::stamped(store, None, None)
    }

    /// A slot for `store`, the records file as `stamp` names it, holding `on_file`.
    fn stamped(
        store: crate::membership_log::Store,
        stamp: Option<fsx::FileStamp>,
        on_file: Option<usize>,
    ) -> Self {
        Self {
            store,
            stamp,
            on_file,
            writing: false,
            wake: Arc::new(Condvar::new()),
        }
    }
}

/// A records POST writing `name`'s file ([`RecordSlot::writing`]): marked while it lives, and
/// unmarked when it goes, however the POST ends.
struct Writing<'a> {
    shared: &'a Shared,
    name: &'a str,
}

impl<'a> Writing<'a> {
    fn mark(shared: &'a Shared, name: &'a str) -> Self {
        if let Some(slot) = shared.lock_server_state().records.get_mut(name) {
            slot.writing = true;
        }
        Self { shared, name }
    }
}

impl Drop for Writing<'_> {
    fn drop(&mut self) {
        if let Some(slot) = self.shared.lock_server_state().records.get_mut(self.name) {
            slot.writing = false;
        }
    }
}

#[derive(Default)]
struct State {
    vaults: BTreeMap<String, Slot>,
    records: BTreeMap<String, RecordSlot>,
    /// The vaults whose records this server found crossed ([`Shared::crossed`]): crossing is
    /// one-way, so once found it is not looked for again.
    crossed: BTreeSet<String>,
    joins: BTreeMap<String, Vec<Queued>>,
    heard: heard::History,
    heard_flushed: BTreeMap<String, u64>,
    heard_dirty: BTreeSet<String>,
    heard_coverage: Option<heard::Coverage>,
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
    /// The pulls of a whole envelope longer than [`ENVELOPE_PEEK`] from peers under way now
    /// ([`ENVELOPE_PULLS_AT_ONCE`]).
    envelope_pulls: PullSlots,
    /// What each peer sent a records pull that this server's store could not take, by vault and
    /// the peer's vault URL, for this run ([`crate::membership_log::untaken`]): the next pull from
    /// that peer names it first, so the peer answers only what is new past it.
    untaken: crate::membership_log::untaken::Untaken<String>,
    /// The bell the `vault-tend` thread naps on when advertising, rung so a new version updates
    /// its TXT at once.
    advertiser: Mutex<Option<Arc<Bell>>>,
    #[cfg(any(test, debug_assertions))]
    waiting: AtomicUsize,
    /// Connections admitted since the start.
    #[cfg(test)]
    served: AtomicUsize,
    /// GETs of a vault's envelope (`/v1/<name>`, long-polls among them) since the start.
    #[cfg(test)]
    envelope_gets: AtomicUsize,
    /// 16 random bytes in lowercase hex, drawn when the server starts and sent on every records
    /// GET reply ([`crate::transport::RECORDS_BOOT_HEADER`]): a restart forgets the records the
    /// store held waiting on a parent, and a member that hears another boot id from a server
    /// that lacks its frontier posts it the whole store again (`client::Reposted`).
    records_boot: String,
    /// GETs of a vault's join queue (`/v1/<name>/join`, long-polls among them) since the start.
    #[cfg(test)]
    join_gets: AtomicUsize,
    /// The records long-polls an application woke ([`apply_records`]) since the start.
    #[cfg(test)]
    record_wakes: AtomicUsize,
    /// The looks at a records file ([`look_for_records`]) since the start.
    #[cfg(test)]
    record_looks: AtomicUsize,
    /// The copies of a records slot's store an application took ([`apply_records`]) since the start.
    #[cfg(test)]
    post_clones: AtomicUsize,
    /// The records a POST verified ([`records_input`]) since the start.
    #[cfg(test)]
    post_verifies: AtomicUsize,
    /// Run by a records POST as it starts verifying its body ([`records_input`]).
    #[cfg(test)]
    verifying: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// While set, a records POST's append fails having touched nothing
    /// ([`Shared::append_records`]), so the POST writes the file whole.
    #[cfg(test)]
    appends_fail: AtomicBool,
    /// While set, a records POST's append puts all but the last byte of its records on the
    /// file, unflushed, and fails ([`Shared::append_records`]).
    #[cfg(test)]
    appends_tear: AtomicBool,
    /// Run by a records POST once its append is on the file, before it stamps the slot
    /// ([`Shared::append_records`]).
    #[cfg(test)]
    appended: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// While set, a records POST's append and whole write both fail having touched nothing
    /// ([`Shared::append_records`], [`Shared::write_records`]): `SwarmRecordsFile`'s `Fail`.
    #[cfg(test)]
    writes_fail: AtomicBool,
}

impl Shared {
    fn lock_server_state(&self) -> MutexGuard<'_, State> {
        self.server_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// [`fsx::append_at_with`] of an application's new records ([`apply_records`]), which a test's
    /// disk refuses having touched nothing while `appends_fail` or `writes_fail` is set, or cuts
    /// short while `appends_tear` is.
    fn append_records(
        &self,
        path: &Path,
        accepted: &crate::membership_log::Store,
        plan: &AppendAttempt,
    ) -> crate::Result<fsx::FileStamp> {
        #[cfg(test)]
        {
            if self.appends_tear.load(Ordering::SeqCst) {
                let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
                let mut prefix = PrefixWriter {
                    file: &mut file,
                    remaining: plan.len.saturating_sub(1),
                };
                write_held_buffered(&mut prefix, accepted, plan.from)?;
                return Err(Error::new("the test's disk cut the append short"));
            }
            if self.appends_fail.load(Ordering::SeqCst) || self.writes_fail.load(Ordering::SeqCst) {
                return Err(Error::new("the test's disk refused the append"));
            }
        }
        let appended = fsx::append_at_with(path, &plan.stamp, plan.len, |writer| {
            write_held_buffered(writer, accepted, plan.from)
        });
        #[cfg(test)]
        if appended.is_ok() {
            let hook = self
                .appended
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        appended
    }

    /// [`fsx::write_atomic_stamped_with`] of an application's held records ([`apply_records`]),
    /// which a test's disk refuses having touched nothing while `writes_fail` is set.
    fn write_records(
        &self,
        path: &Path,
        accepted: &crate::membership_log::Store,
    ) -> crate::Result<Option<fsx::FileStamp>> {
        #[cfg(test)]
        if self.writes_fail.load(Ordering::SeqCst) {
            return Err(Error::new("the test's disk refused the whole write"));
        }
        fsx::write_atomic_stamped_with(path, 0o600, |writer| {
            write_held_buffered(writer, accepted, 0)
        })
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

    /// `name`'s crossed mark ([`crate::crossing::Mark`]), beside its records file.
    fn crossing_mark(&self, name: &str) -> crate::crossing::Mark {
        crate::crossing::Mark::at(self.store.join(format!("{name}.crossed")))
    }

    /// Whether `name`'s records have crossed ([`crate::crossing`]): `state` found them so
    /// before, their log holds more than [`crate::membership_log::LEGACY_RECORDS`] records, or
    /// their mark is stored.
    ///
    /// # Errors
    /// The look for the mark failed (an `lstat` that fails but for its absence): a `500` the
    /// caller answers, so on a failed look nothing is served, taken or stated, and the next
    /// request looks again.
    fn crossed(&self, state: &mut State, name: &str) -> Result<bool, Reply> {
        if state.crossed.contains(name) {
            return Ok(true);
        }
        let held = state
            .records
            .get(name)
            .and_then(|slot| slot.store.log())
            .map_or(0, crate::membership_log::Log::held_count);
        let crossed = crate::crossing::past_legacy(held)
            || match self.crossing_mark(name).stored() {
                Ok(stored) => stored,
                Err(e) => {
                    note(&format!("{name}: could not look for the crossed mark: {e}"));
                    return Err(Reply::text(
                        500,
                        "the server could not look for whether its records crossed",
                    ));
                }
            };
        if crossed {
            state.crossed.insert(name.to_owned());
        }
        Ok(crossed)
    }

    /// [`Shared::crossed`], taking the state lock.
    ///
    /// # Errors
    /// As [`Shared::crossed`].
    fn records_crossed(&self, name: &str) -> Result<bool, Reply> {
        self.crossed(&mut self.lock_server_state(), name)
    }

    /// Store `name`'s crossed mark, before this server takes a record of a crossed log or holds
    /// more than [`crate::membership_log::LEGACY_RECORDS`]: a `500` where it cannot, and the
    /// caller then takes nothing.
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "SwarmRecordsCapacity",
            action = "CrossedPost",
            project = "aterm_secrets::server::crossing_tests::records_capacity_crossing"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "SwarmRecordsCapacity",
            action = "CrossedPostDies",
            project = "aterm_secrets::server::crossing_tests::records_capacity_crossing"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "SwarmRecordsCapacity",
            action = "Grow",
            project = "aterm_secrets::server::crossing_tests::records_capacity_crossing"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "SwarmRecordsCapacity",
            action = "GrowDies",
            project = "aterm_secrets::server::crossing_tests::records_capacity_crossing"
        )
    )]
    fn mark_crossed(&self, name: &str) -> Result<(), Reply> {
        if let Err(e) = self.crossing_mark(name).store() {
            note(&format!("{name}: could not store the crossed mark: {e}"));
            return Err(Reply::text(
                500,
                "the server could not store that its records crossed",
            ));
        }
        self.lock_server_state().crossed.insert(name.to_owned());
        Ok(())
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

/// Whether a server pulls from the servers it finds over Bonjour and is found by them there
/// ([`Children::tend_with_role`]), which an empty store's note names ([`unheld_note`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bonjour {
    /// It advertises on macOS: it browses, and pulls from what it finds.
    Advertised,
    /// It advertises nothing (`--no-advertise`, or an address the ALab URL does not reach).
    Unadvertised,
    /// Off macOS, where aterm advertises and browses nothing ([`aterm_uds::lan`]).
    Absent,
}

impl Bonjour {
    /// This server's: on macOS, by whether it advertises a name.
    fn here(advertises: bool) -> Bonjour {
        match (cfg!(target_os = "macos"), advertises) {
            (false, _) => Bonjour::Absent,
            (true, true) => Bonjour::Advertised,
            (true, false) => Bonjour::Unadvertised,
        }
    }
}

/// What [`serve`] logs at start when its store holds no vault yet: who may give it a first
/// version ([`refuse_unheld`]), and how one comes. `pins` are this start's; where a role decider
/// runs (`decided`, on an advertising Mac) it can pin a vault later ([`tend_role_targets`]).
fn unheld_note(
    state: &State,
    pins: &[(String, [u8; 32])],
    bonjour: Bonjour,
    decided: bool,
) -> Option<String> {
    if !state.vaults.is_empty() {
        return None;
    }
    let names: Vec<&str> = pins.iter().map(|(name, _)| name.as_str()).collect();
    let names = names.join(", ");
    let pins_later = decided && bonjour == Bonjour::Advertised;
    let from = if pins.is_empty() && pins_later {
        "only from this machine until it pins a vault, as a server that decides its ALab role \
         pins alab once it reads this machine's membership"
            .to_string()
    } else if pins.is_empty() {
        "only from this machine, since no --pin names a vault".to_string()
    } else {
        format!("from this machine, or from another for a vault this server pins ({names})")
    };
    // A pull is never this machine's put ([`pull_from_peer`]), so it brings a first version
    // only for a pinned name.
    let pulled = match bonjour {
        Bonjour::Advertised if pins.is_empty() && pins_later => {
            "; it pulls a first version from the servers it finds over Bonjour only for a vault \
             it comes to pin, since a pull is never this machine's"
                .to_owned()
        }
        Bonjour::Advertised if pins.is_empty() => {
            "; it pulls no first version from the servers it finds over Bonjour, since a pull is \
             never this machine's"
                .to_owned()
        }
        Bonjour::Advertised => {
            format!(", or, for {names}, pulled from a server this one finds over Bonjour")
        }
        Bonjour::Unadvertised => {
            "; this server advertises nothing, so no server finds it over Bonjour and it pulls \
             from none"
                .to_owned()
        }
        Bonjour::Absent => {
            "; aterm advertises and browses Bonjour on macOS only, so here this server finds no \
             server to pull from and none finds it: only a machine that names its address \
             reaches it"
                .to_owned()
        }
    };
    Some(format!(
        "the store holds no vault yet, so a machine that asks here for one finds none; a first \
         version is taken {from}, put here by an aterm secrets admin command that publishes to \
         this server or by a subscriber whose vault source is this server (it puts its own copy \
         back){pulled}"
    ))
}

/// Read the store: every `<name>.vault` that parses and verifies, and every `<name>.joins` line
/// that parses and has not expired.
#[cfg(test)]
fn load(store: &Path, now: u64) -> crate::Result<State> {
    load_with_pins(store, now, &[])
}

fn load_with_pins(store: &Path, now: u64, pins: &[(String, [u8; 32])]) -> crate::Result<State> {
    let mut state = State::default();
    let entries =
        std::fs::read_dir(store).map_err(|e| Error::new(format!("{}: {e}", store.display())))?;
    let mut names: Vec<String> = entries
        .filter_map(|entry| {
            let file = entry.ok()?.file_name().into_string().ok()?;
            let stem = file
                .strip_suffix(".vault")
                .or_else(|| file.strip_suffix(".records"))
                .or_else(|| file.strip_suffix(".joins"))
                .or_else(|| file.strip_suffix(".heard"))?;
            envelope::valid_name(stem).then_some(file)
        })
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
            if let Err(error) = crate::records_storage::recover_pending(&path) {
                note(&format!("{file}: storage recovery refused: {error}"));
                continue;
            }
            let keys = held_keys(&state, name).or_else(|| {
                pins.iter()
                    .find_map(|(v, key)| (v == name).then(|| vec![*key]))
            });
            if let Err(unread) = look_at_records(&mut state, name, &path, keys.as_deref()) {
                note(&format!("{file}: skipped: {}", unread.into_error()));
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
                        state.heard.insert(
                            name.to_owned(),
                            heard::VaultHistory {
                                subscribers: map,
                                anonymous: None,
                            },
                        );
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
        .filter_map(|entry| {
            let file = entry.ok()?.file_name().into_string().ok()?;
            file.strip_suffix(".heard")
                .is_some_and(envelope::valid_name)
                .then_some(file)
        })
        .collect();
    names.sort();
    let mut out = Vec::new();
    for file in names {
        let Some(name) = file.strip_suffix(".heard") else {
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
        if crate::age::Recipient::parse(recipient).is_err()
            || !at.bytes().all(|b| b.is_ascii_digit())
        {
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

fn record_heard(
    shared: &Shared,
    state: &mut State,
    name: &str,
    subscriber: Option<&str>,
    now: u64,
) -> bool {
    if shared.stopping.load(Ordering::SeqCst) || state.heard_coverage.is_none() {
        return false;
    }
    let rows = state.heard.entry(name.to_owned()).or_default();
    if let Some(subscriber) = subscriber {
        let last = rows.subscribers.entry(subscriber.to_owned()).or_default();
        *last = (*last).max(now);
    } else {
        rows.anonymous = rows.anonymous.max(Some(now));
    }
    let lost_recent = heard::bound(&mut state.heard, now);
    if let Some(coverage) = state.heard_coverage.as_mut() {
        coverage.observe_time(now);
        if lost_recent {
            coverage.invalidate(now);
        }
    }
    state.heard_dirty.insert(name.to_owned());
    let flush_due = match state.heard_flushed.get(name) {
        None => true,
        Some(&last) => now.saturating_sub(last) >= HEARD_FLUSH_SECS,
    };
    if flush_due {
        let body = state
            .heard
            .get(name)
            .map(|rows| format_heard(&rows.subscribers))
            .unwrap_or_default();
        if fsx::write_atomic(&shared.heard_path(name), body.as_bytes(), 0o600).is_ok() {
            state.heard_flushed.insert(name.to_owned(), now);
            state.heard_dirty.remove(name);
        } else if let Some(coverage) = state.heard_coverage.as_mut() {
            coverage.invalidate(now);
        }
    }
    true
}

fn flush_all_heard(shared: &Shared, state: &mut State) {
    let dirty: Vec<String> = state.heard_dirty.iter().cloned().collect();
    let now = (shared.tuning.now)();
    for name in dirty {
        if let Some(map) = state.heard.get(&name) {
            let body = format_heard(&map.subscribers);
            if fsx::write_atomic(&shared.heard_path(&name), body.as_bytes(), 0o600).is_ok() {
                state.heard_flushed.insert(name.clone(), now);
                state.heard_dirty.remove(&name);
            } else if let Some(coverage) = state.heard_coverage.as_mut() {
                coverage.invalidate(now);
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

/// The vaults `store` holds, in name order: each `<name>.vault` there that names `name` and
/// verifies, as a server started on the store takes it ([`load_vault`]). A store not made yet
/// holds none.
/// # Errors
/// The store cannot be listed.
pub(crate) fn held_vaults(store: &Path) -> crate::Result<Vec<String>> {
    let entries = match std::fs::read_dir(store) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::new(format!("{}: {e}", store.display()))),
    };
    let mut held: Vec<String> = entries
        .filter_map(|entry| {
            let file = entry.ok()?.file_name().into_string().ok()?;
            let name = file.strip_suffix(".vault")?;
            (envelope::valid_name(name) && load_vault(&store.join(&file), name).is_ok())
                .then(|| name.to_owned())
        })
        .collect();
    held.sort();
    Ok(held)
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

/// The listing, which names no request's address. It carries no LAN marker (` lan=1`,
/// ` home=`): an approver before aterm 0.148 admits a `lan=1` request by that marker alone, and
/// one of 0.148 or 0.149 an open ALab vault's `home=` request with nobody's Allow.
fn join_lines(queue: &[Queued]) -> String {
    let mut text = String::new();
    for queued in queue {
        text.push_str(&queued.request.listed());
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
        text.push_str(&queued.request.listed());
        text.push('\n');
    }
    text
}

/// Read [`stored_lines`]: an older build's `lan=1` (any private peer's) is read past
/// ([`parse_join_line`]).
fn parse_stored(text: &str) -> Vec<Queued> {
    let mut queue = Vec::new();
    let mut peer = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("peer ") {
            let (address, asked) = rest.split_once(' ').unwrap_or((rest, "0"));
            let asked = (!asked.is_empty() && asked.bytes().all(|b| b.is_ascii_digit()))
                .then(|| asked.parse::<u64>().ok())
                .flatten()
                .unwrap_or(0);
            peer = address.parse::<IpAddr>().ok().map(|ip| (ip, asked));
            continue;
        }
        let (from, asked) = peer.take().unzip();
        if let Ok(request) = parse_join_line(line)
            && request.time.is_some()
        {
            queue.push(Queued {
                request,
                peer: from,
                asked: asked.unwrap_or(0),
                changed: 0,
                noted: 0,
                refused: None,
            });
        }
    }
    queue
}

/// Wait for a connection or the owner's cancellation pipe to close. Only the nonblocking
/// accept holds the spawn lock, keeping each connection out of children from its creation.
fn accept_loop(
    listener: &TcpListener,
    cancelled: &io::PipeReader,
    shared: &Arc<Shared>,
    stop: &AtomicBool,
    refusals: &SyncSender<TcpStream>,
) -> io::Result<()> {
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let ready = aterm_uds::ready::either(listener.as_fd(), cancelled.as_fd())?;
        if ready.second || stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        match aterm_uds::accept(
            listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ) {
            Ok((stream, peer)) => {
                if stop.load(Ordering::SeqCst) {
                    return Ok(());
                }
                dispatch(shared, stream, peer.ip(), refusals);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(e) if is_transient(&e) => {
                match aterm_uds::ready::until(
                    cancelled.as_fd(),
                    aterm_uds::ready::Interest::Read,
                    Instant::now() + Duration::from_millis(50),
                ) {
                    Ok(()) => return Ok(()),
                    Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
                    Err(e) => return Err(e),
                }
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
        None,
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
    /// Whether the reply carries this server's boot id
    /// ([`crate::transport::RECORDS_BOOT_HEADER`], [`Shared::records_boot`]), as every records
    /// GET reply does. A flag, not the id: a wider field makes every `Result<_, Reply>` an
    /// error variant clippy calls too large.
    records_boot: bool,
    /// What the reply states of the vault's records ([`crate::transport::RECORDS_BOUND_HEADER`]),
    /// as every records reply does: `Some(crossed)`, stated with this build's held bound
    /// ([`crate::crossing::RecordsBound::here`]). Whether crossed alone, for the same reason
    /// `records_boot` is a flag.
    records_bound: Option<bool>,
}

enum Body {
    Empty,
    Text(String),
    Envelope(Arc<Vec<u8>>),
    Records(Vec<crate::membership_log::Record>),
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
            records_boot: false,
            records_bound: None,
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
            records_boot: false,
            records_bound: None,
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

/// Send `reply`, no write blocking longer than `stall` and the whole within `within`, with the
/// server's `boot` id when the reply carries one ([`Reply::records_boot`]).
fn write_reply(
    stream: &mut TcpStream,
    reply: &Reply,
    boot: Option<&str>,
    stall: Duration,
    within: Duration,
) -> io::Result<()> {
    write_reply_to(stream, reply, boot, stall, within)
}

/// The socket operations used by every reply, with a controlled clock in sender tests.
trait ReplyWriter: Write {
    fn set_wait(&mut self, wait: Duration) -> io::Result<()>;

    fn now(&self) -> Instant {
        Instant::now()
    }
}

impl ReplyWriter for TcpStream {
    fn set_wait(&mut self, wait: Duration) -> io::Result<()> {
        self.set_write_timeout(Some(wait))
    }
}

fn record_reply_len(records: &[crate::membership_log::Record]) -> io::Result<usize> {
    records.iter().try_fold(0usize, |length, record| {
        length
            .checked_add(record.bytes().len())
            .filter(|length| *length <= crate::membership_log::MAX_RECORDS_STREAM)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "record reply is too large"))
    })
}

fn write_reply_to(
    stream: &mut impl ReplyWriter,
    reply: &Reply,
    boot: Option<&str>,
    stall: Duration,
    within: Duration,
) -> io::Result<()> {
    let deadline = stream.now().checked_add(within);
    // Finish length validation before sending even the status line. The selected Records
    // own immutable wire data, so the same length remains valid throughout the send.
    let (length, content_type) = match &reply.body {
        Body::Empty => (0, "text/plain; charset=utf-8"),
        Body::Text(text) => (text.len(), "text/plain; charset=utf-8"),
        Body::Envelope(bytes) => (bytes.len(), "text/plain; charset=utf-8"),
        Body::Records(records) => (record_reply_len(records)?, "application/octet-stream"),
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
    if reply.records_boot
        && let Some(boot) = boot
    {
        head.push_str(&format!(
            "{}: {boot}\r\n",
            crate::transport::RECORDS_BOOT_HEADER
        ));
    }
    if let Some(crossed) = reply.records_bound {
        head.push_str(&format!(
            "{}: {}\r\n",
            crate::transport::RECORDS_BOUND_HEADER,
            crate::crossing::RecordsBound::here(crossed).value()
        ));
    }
    if let Some(secs) = reply.retry_after {
        head.push_str(&format!("Retry-After: {secs}\r\n"));
    }
    if reply.status != 204 {
        head.push_str(&format!("Content-Type: {content_type}\r\n"));
        head.push_str(&format!("Content-Length: {length}\r\n"));
    }
    head.push_str("\r\n");
    send(stream, head.as_bytes(), stall, deadline)?;
    if reply.status != 204 {
        match &reply.body {
            Body::Empty => {}
            Body::Text(text) => send(stream, text.as_bytes(), stall, deadline)?,
            Body::Envelope(bytes) => send(stream, bytes, stall, deadline)?,
            Body::Records(records) => {
                record_chunks(records, |bytes| send(stream, bytes, stall, deadline))?;
            }
        }
    }
    stream.flush()
}

/// Coalesce across record boundaries, keeping exactly one bounded partial chunk. Complete
/// chunks of an existing record are sent directly, without copying their immutable bytes.
fn record_chunks(
    records: &[crate::membership_log::Record],
    mut send_chunk: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<()> {
    let mut partial = [0u8; WRITE_CHUNK];
    let mut used = 0;
    for record in records {
        let mut bytes = record.bytes();
        if used != 0 {
            let take = (WRITE_CHUNK - used).min(bytes.len());
            partial[used..used + take].copy_from_slice(&bytes[..take]);
            used += take;
            bytes = &bytes[take..];
            if used == WRITE_CHUNK {
                send_chunk(&partial)?;
                used = 0;
            }
        }
        while bytes.len() >= WRITE_CHUNK {
            send_chunk(&bytes[..WRITE_CHUNK])?;
            bytes = &bytes[WRITE_CHUNK..];
        }
        if !bytes.is_empty() {
            partial[..bytes.len()].copy_from_slice(bytes);
            used = bytes.len();
        }
    }
    if used != 0 {
        send_chunk(&partial[..used])?;
    }
    Ok(())
}

/// Write all of `bytes`, no write blocking longer than `stall` and none past `deadline`.
fn send(
    stream: &mut impl ReplyWriter,
    mut bytes: &[u8],
    stall: Duration,
    deadline: Option<Instant>,
) -> io::Result<()> {
    while !bytes.is_empty() {
        let wait = match deadline {
            Some(deadline) => deadline
                .checked_duration_since(stream.now())
                .filter(|left| !left.is_zero())
                .ok_or(io::ErrorKind::TimedOut)?
                .min(stall),
            None => stall,
        };
        stream.set_wait(wait)?;
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
    observer: bool,
    no_wait: bool,
    /// What the client stated of its records ([`crate::transport::RECORDS_BOUND_HEADER`]):
    /// `None` where it sent none, sent it twice, or sent a value that is not a bound's.
    records_bound: Option<crate::crossing::RecordsBound>,
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
        Some(&shared.records_boot),
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
    let mut observer = false;
    let mut no_wait = false;
    // `Some(None)` once the bound header came twice or did not parse.
    let mut records_bound: Option<Option<crate::crossing::RecordsBound>> = None;
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
        } else if name.eq_ignore_ascii_case(crate::transport::OBSERVER_HEADER) && value == "1" {
            observer = true;
        } else if name.eq_ignore_ascii_case(crate::transport::RECORDS_BOUND_HEADER) {
            records_bound = Some(match records_bound {
                None => crate::crossing::RecordsBound::parse(value),
                Some(_) => None,
            });
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
        observer,
        no_wait,
        records_bound: records_bound.flatten(),
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
    Heard(&'a str),
}

/// `/v1/<name>`, `/v1/<name>/records`, `/v1/<name>/join`, `/v1/<name>/join/verdict` or
/// `/v1/<name>/heard`, with `<name>` a vault name.
fn route(path: &str) -> Option<Route<'_>> {
    let rest = path.strip_prefix("/v1/")?;
    let route = match rest.split_once('/') {
        None => Route::Vault(rest),
        Some((name, "records")) => Route::Records(name),
        Some((name, "join")) => Route::Join(name),
        Some((name, "join/verdict")) => Route::Verdict(name),
        Some((name, "heard")) => Route::Heard(name),
        Some(_) => return None,
    };
    let (Route::Vault(name)
    | Route::Records(name)
    | Route::Join(name)
    | Route::Verdict(name)
    | Route::Heard(name)) = route;
    envelope::valid_name(name).then_some(route)
}

/// [`answer`] `request`: a reply to one at a vault's records also states what this server holds
/// of them ([`Reply::records_bound`]), whatever its status. Where this server cannot look for
/// whether they crossed ([`Shared::crossed`]), the reply is that look's `500`, stating no bound:
/// a bound it cannot vouch for would cross a client for good, or leave one uncrossed beside a
/// crossed log's records.
fn respond(shared: &Shared, stream: &mut TcpStream, peer: IpAddr, request: Request) -> Reply {
    let records = match route(&request.path) {
        Some(Route::Records(name)) => Some(name.to_owned()),
        _ => None,
    };
    let mut reply = answer(shared, stream, peer, request);
    if let Some(name) = records {
        match shared.records_crossed(&name) {
            Ok(crossed) => reply.records_bound = Some(crossed),
            Err(failed) => reply = failed,
        }
    }
    reply
}

/// What a records request without [`crate::transport::RECORDS_BOUND_HEADER`] gets from a server
/// whose records crossed ([`crate::crossing`]): `409` and no record, which every build before the
/// fence takes for a source that failed, so it holds what it held rather than part of a batch.
fn fenced() -> Reply {
    Reply::text(
        409,
        "this server's membership log holds records a build before the crossed fence cannot take \
         whole: it serves and takes records only from a client that states its records bound \
         (X-Aterm-Records-Bound)",
    )
}

/// Refuse incompatible capacities before any record is exposed or retained.
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "SwarmRecordsCapacity",
        action = "LegacyGet",
        project = "aterm_secrets::server::crossing_tests::records_capacity_crossing"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "SwarmRecordsCapacity",
        action = "LegacyPost",
        project = "aterm_secrets::server::crossing_tests::records_capacity_crossing"
    )
)]
fn records_fence(crossed: bool, peer: Option<crate::crossing::RecordsBound>) -> Option<Reply> {
    let own = crate::crossing::RecordsBound::here(crossed);
    match peer {
        Some(peer) if !own.can_exchange(peer) => Some(Reply::text(
            409,
            format!(
                "incompatible membership records capacities: local {}, peer {}",
                own.held, peer.held
            ),
        )),
        None if crossed => Some(fenced()),
        _ => None,
    }
}

fn answer(shared: &Shared, stream: &mut TcpStream, peer: IpAddr, mut request: Request) -> Reply {
    let path = std::mem::take(&mut request.path);
    let Some(route) = route(&path) else {
        return Reply::text(
            404,
            "no such path: the vault paths are /v1/<name>, /v1/<name>/records, /v1/<name>/join, /v1/<name>/join/verdict and /v1/<name>/heard",
        );
    };
    let method = request.method.as_str();
    if !matches!(
        (&route, method),
        (Route::Vault(_), "GET" | "PUT")
            | (Route::Heard(_), "GET")
            | (
                Route::Records(_) | Route::Join(_) | Route::Verdict(_),
                "GET" | "POST"
            )
    ) {
        let allow = match route {
            Route::Vault(_) => "GET, PUT",
            Route::Heard(_) => "GET",
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
    // A crossed poster's records POST carries the one query every build before the fence
    // refuses unread ([`crate::transport::CROSSED_POST_QUERY`]), and says it crossed.
    let crossed_post = matches!((&route, method), (Route::Records(_), "POST"))
        && request.query.as_deref() == Some(crate::transport::CROSSED_POST_QUERY);
    if request.query.is_some() && !waits && !crossed_post {
        return Reply::text(400, "this path takes no query");
    }
    if crossed_post && !request.records_bound.is_some_and(|bound| bound.crossed) {
        return Reply::text(
            400,
            "a records POST carries crossed=2 only beside a records bound that says crossed",
        );
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
                request.observer,
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
            let mut reply = get_records(
                shared,
                stream,
                name,
                &RecordsGet {
                    query: request.query.as_deref(),
                    subscriber: request.subscriber.as_deref(),
                    no_wait: request.no_wait,
                    local,
                    stated: request.records_bound,
                },
            );
            reply.records_boot = true;
            reply
        }
        (Route::Records(name), _) => {
            // Before the body is read: a crossed server takes no record from a poster that
            // states no bound ([`apply_records`] asks again once it holds the store).
            match shared.records_crossed(name) {
                Ok(crossed) => {
                    if let Some(reply) = records_fence(crossed, request.records_bound) {
                        return reply;
                    }
                }
                Err(failed) => return failed,
            }
            let stated = request.records_bound;
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
            match records_input::from_http(shared, name, stream, request, length) {
                Ok(records) => apply_records(shared, name, records, local, stated),
                Err(reply) => reply,
            }
        }
        (Route::Join(name), "GET") => {
            #[cfg(test)]
            shared.join_gets.fetch_add(1, Ordering::SeqCst);
            list_joins(shared, stream, name, request.query.as_deref())
        }
        (Route::Join(name), _) => {
            match body_length(&request, MAX_JOIN_BODY)
                .and_then(|length| read_body(stream, request, length, &shared.tuning))
            {
                Ok(body) => post_join(shared, name, &body, peer),
                Err(reply) => reply,
            }
        }
        (Route::Heard(name), _) => list_heard(shared, name),
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
        records_boot: false,
        records_bound: None,
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
    #[cfg(any(test, debug_assertions))]
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
    #[cfg(any(test, debug_assertions))]
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
    observer: bool,
) -> Reply {
    let after = match after_query(query) {
        Ok(after) => after,
        Err(reply) => return reply,
    };
    let mut state = shared.lock_server_state();
    if shared.stopping.load(Ordering::SeqCst) {
        return Reply::text(503, "this server is stopping".to_owned());
    }
    take_from_store(shared, &mut state, name);
    if !state.vaults.contains_key(name) {
        return Reply::text(404, format!("no vault named {name} here"));
    }
    if subscriber.is_some() || !observer {
        let now = (shared.tuning.now)();
        if !record_heard(shared, &mut state, name, subscriber, now) {
            return Reply::text(503, "heard history is not tracking reads".to_owned());
        }
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
    if let Some(records) = state.records.get(name)
        && records.store.log().is_none()
        && records.stamp.is_some()
    {
        records.wake.notify_all();
    }
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
/// The answer is P3's kernels' ([`nle::Takeover::of`]), the compare-and-set the island's reach
/// rule rests on; this reads the facts and words the reply. The chain check is pure and cheap (a
/// comparison of at most 64 links, no signature), so it runs for every envelope, and the kernels
/// decide which refusal stands, in the order they always did: the pin, then, for a pin that was
/// not replaced, the sequence, the chain and the parents.
fn takes_over(
    envelope: &envelope::Envelope,
    body: &[u8],
    held: &Held,
    retired: &envelope::Retired,
) -> Result<Option<envelope::Pin>, Reply> {
    let followed: Vec<[u8; 32]> = held.handoff.iter().map(|(key, _)| *key).collect();
    let pin = envelope.check_pin_with(&held.signer, &followed, retired);
    let extends = envelope.check_extends(&held.signer, &held.handoff);
    let facts = nle::TakeoverFacts {
        pin_ok: pin.is_ok(),
        replaced: matches!(pin, Ok(envelope::Pin::Replaced(_))),
        not_newer: envelope.seq <= held.seq,
        same_version: envelope.seq == held.seq && held.bytes.as_slice() == body,
        chain_ok: extends.is_ok(),
        names_parents: !envelope.parents.is_empty(),
        held_parented: held.parented,
        among_parents: envelope.parents.contains(&held.head),
    };
    let refused = |error: Option<Error>| Reply::text(403, error.map_or_else(String::new, |e| e.0));
    match nle::Takeover::of(facts) {
        nle::Takeover::Takes => Ok(pin.ok()),
        nle::Takeover::Holds => Ok(None),
        nle::Takeover::PinRefused => Err(refused(pin.err())),
        nle::Takeover::BreaksChain => Err(refused(extends.err())),
        nle::Takeover::NotNewer => Err(Reply::conflict_with_head(
            held,
            format!(
                "seq {} is not newer than seq {}, which this server holds",
                envelope.seq, held.seq
            ),
        )),
        nle::Takeover::OffParents => Err(Reply::conflict_with_head(
            held,
            format!(
                "head {} at seq {} is not among the envelope's parents",
                hex32_encode(&held.head),
                held.seq
            ),
        )),
    }
}

/// Hold a newer version an admin published at the vault's store file itself (an admin whose
/// vault is that path), checked at each read of the vault or of its join queue, each slice of a
/// held long-poll, each PUT and each join request. Without it the server serves what it read at
/// launch and judges join requests by that version's seq, and the next publish built on that
/// overwrites the path's version, a revoke included. A version there is held only as a PUT of it
/// would be ([`takes_over`]); any other file changes nothing.
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
        parented: !envelope.parents.is_empty(),
        handoff: envelope.handoff.clone(),
        bytes: Arc::new(bytes),
    };
    slot.wake.notify_all();
    if let Some(records) = state.records.get(name)
        && records.store.log().is_none()
        && records.stamp.is_some()
    {
        records.wake.notify_all();
    }
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

/// Merge any verified records written directly at `<name>.records` on disk into memory
/// ([`look_at_records`]). A file the read refuses is remembered by its stamp, and the start loop
/// notes why; a read that failed leaves nothing remembered, so the next look reads again.
fn take_records_from_store(shared: &Shared, state: &mut State, name: &str) {
    let _held_as_it_was = look_for_records(shared, state, name);
}

/// [`take_records_from_store`], saying why the look read no file: a POST about to write answers
/// 500 when the read failed ([`apply_records`]), since what it held then could replace records on
/// file it never read.
///
/// # Errors
/// As [`look_at_records`].
fn look_for_records(shared: &Shared, state: &mut State, name: &str) -> Result<(), fsx::Unread> {
    #[cfg(test)]
    shared.record_looks.fetch_add(1, Ordering::SeqCst);
    take_from_store(shared, state, name);
    if vault_capacity_exceeded(state, name) {
        return Ok(());
    }
    let keys = record_keys(shared, state, name);
    look_at_records(state, name, &shared.records_path(name), keys.as_deref())
}

/// A look at `name`'s records file at `path`, its store following `keys` ([`held_keys`],
/// [`record_keys`]; `docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1 F23, rule 3). It stats the file
/// (`lstat`) and reads it only when the slot's stamp differs, or when a store with no genesis
/// now follows other keys (a new envelope may name the key that signed the file's genesis):
/// each long-poll looks on every wake and every liveness tick, and reading and comparing the
/// whole file each time cost up to [`crate::membership_log::MAX_RECORDS_STREAM`] a look. A
/// rooted store takes the file's records in place (what it refuses changes nothing); a store
/// with no genesis keeps them only when they root it. The slot keeps the
/// stamp of the file it read, and knows the file holds exactly its log (`on_file`) only when
/// the store was empty before the read and its records' bytes fill the file: then each held
/// record came from the file, and nothing else is in it. Nothing is cloned.
///
/// # Errors
/// [`fsx::Unread::Refused`]: the read refuses the file (past the bound, an owner or mode it
/// refuses, a symlink); the slot remembers its stamp anyway, so it is not read again until it
/// changes. [`fsx::Unread::Failed`]: the read failed (too many open files, an I/O error), or
/// the descriptor changed during the read, or `name`'s store cannot be made
/// ([`crate::membership_log::Store::for_vault`]); nothing is
/// remembered, so the next look reads again.
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "SwarmRecordsFile",
        action = "Restart",
        project = "aterm_secrets::server::tests::records_file_projection"
    )
)]
fn look_at_records(
    state: &mut State,
    name: &str,
    path: &Path,
    keys: Option<&[[u8; 32]]>,
) -> Result<(), fsx::Unread> {
    look_at_records_with(state, name, path, keys, || {})
}

fn look_at_records_with(
    state: &mut State,
    name: &str,
    path: &Path,
    keys: Option<&[[u8; 32]]>,
    after_decode: impl FnOnce(),
) -> Result<(), fsx::Unread> {
    if state.records.get(name).is_some_and(|slot| slot.writing) {
        return Ok(());
    }
    let entry = path;
    let data_path = crate::records_storage::data_path(path).map_err(fsx::Unread::Failed)?;
    let path = data_path.as_path();
    let Some(seen) = fsx::stamp(path) else {
        return Ok(());
    };
    if let Some(slot) = state.records.get(name)
        && slot.stamp == Some(seen)
        && (slot.store.log().is_some() || keys.is_none_or(|k| slot.store.keys() == Some(k)))
    {
        return Ok(());
    }
    let read = (|| {
        let Some(mut reader) = fsx::open_owned_reader_classified(path)? else {
            return Ok(None);
        };
        let limit = state.records.get(name).map_or_else(
            || crate::membership_log::RecordBudgets::current().held,
            |slot| slot.store.budgets().held,
        );
        let decoded = crate::membership_log::Store::read_records(reader.file(), limit)
            .map_err(fsx::Unread::Failed)?;
        after_decode();
        // The shared decoder consumes at most the client's recovery prefix. Servers refuse
        // the whole oversized file, including growth or a tail after malformed frames.
        let mut extra = [0];
        let more = loop {
            match reader.file().read(&mut extra) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => {
                    return Err(fsx::Unread::Failed(Error::new(format!(
                        "{}: {error}",
                        path.display()
                    ))));
                }
                Ok(n) => break n != 0,
            }
        };
        reader.check_unchanged_classified()?;
        crate::records_storage::data_path(entry).map_err(fsx::Unread::Failed)?;
        if more {
            return Err(fsx::Unread::Refused(Error::new(format!(
                "{}: larger than {} bytes",
                path.display(),
                crate::membership_log::MAX_RECORDS_STREAM
            ))));
        }
        Ok(Some((reader.stamp(), decoded)))
    })();
    let read = match read {
        Ok(None) => return Ok(()),
        Ok(Some(read)) => Ok(read),
        Err(fsx::Unread::Refused(e)) => Err(e),
        Err(failed @ fsx::Unread::Failed(_)) => return Err(failed),
    };
    if !state.records.contains_key(name) {
        let store = crate::membership_log::Store::for_vault(name).map_err(fsx::Unread::Failed)?;
        state
            .records
            .insert(name.to_owned(), RecordSlot::new(follow_held(store, keys)));
    }
    let Some(slot) = state.records.get_mut(name) else {
        return Ok(());
    };
    let (stamp, decoded) = match read {
        Ok(read) => read,
        Err(e) => {
            slot.stamp = Some(seen);
            slot.on_file = None;
            return Err(fsx::Unread::Refused(e));
        }
    };
    let was_empty = slot.store.is_empty();
    let heads = slot.store.log().map(crate::membership_log::Log::heads);
    let bytes_read = decoded.bytes_read();
    let store = follow_held(std::mem::take(&mut slot.store), keys);
    slot.store = decoded.apply_rooted(store);
    slot.stamp = Some(stamp);
    slot.on_file = slot
        .store
        .log()
        .filter(|log| was_empty && log.held_bytes() == bytes_read)
        .map(crate::membership_log::Log::held_count);
    if let Some(log) = slot.store.log()
        && Some(log.heads()) != heads
    {
        slot.wake.notify_all();
    }
    Ok(())
}

/// Load a records file through the server's real cold path, without listeners or preload.
/// The returned store moves out of its slot, so this seam introduces no copy of its records.
///
/// # Errors
/// The server refuses or cannot read the file, or the path supplied no records slot.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn test_cold_records(
    path: &Path,
    name: &str,
    keys: &[[u8; 32]],
) -> crate::Result<(crate::membership_log::Store, Option<usize>)> {
    let mut state = State::default();
    look_at_records(&mut state, name, path, Some(keys)).map_err(fsx::Unread::into_error)?;
    let slot = state
        .records
        .remove(name)
        .ok_or_else(|| Error::new("the path supplied no records slot"))?;
    Ok((slot.store, slot.on_file))
}

/// The most digests the server reads from a records GET's `have=`. A pull names at most
/// [`crate::membership_log::store::HAVE_IDS`], and no request head ([`MAX_HEAD`]) carries this
/// many; the parse stops here whatever the head's bound.
const MAX_HAVE_IDS: usize = 4096;

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
        if out.len() >= MAX_HAVE_IDS {
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

fn records_reply(records: Vec<crate::membership_log::Record>, records_need: bool) -> Reply {
    Reply {
        status: 200,
        body: Body::Records(records),
        etag: None,
        vault_head: None,
        allow: None,
        retry_after: None,
        records_need,
        records_boot: false,
        records_bound: None,
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

/// What a records GET asks ([`get_records`]).
struct RecordsGet<'a> {
    /// Its query (`have=…`).
    query: Option<&'a str>,
    /// The subscriber it reads for ([`crate::transport::SUBSCRIBER_HEADER`]).
    subscriber: Option<&'a str>,
    /// Whether it asks the server not to wait ([`crate::transport::NO_WAIT_HEADER`]).
    no_wait: bool,
    /// Whether it came from this host.
    local: bool,
    /// What it states of the client's records ([`crate::transport::RECORDS_BOUND_HEADER`]).
    stated: Option<crate::crossing::RecordsBound>,
}

/// Answer a records GET: the records past what it names that the store holds, at once or once
/// some arrive. A client that states no records bound gets [`fenced`] once the vault's records
/// crossed, also where they crossed while it waited, and the failed look's `500` where this
/// server cannot look for whether they crossed ([`Shared::crossed`]).
fn get_records(shared: &Shared, stream: &TcpStream, name: &str, get: &RecordsGet<'_>) -> Reply {
    let &RecordsGet {
        query,
        subscriber,
        no_wait,
        local,
        stated,
    } = get;
    let have = match have_query(query) {
        Ok(have) => have,
        Err(reply) => return reply,
    };
    let mut state = shared.lock_server_state();
    if shared.stopping.load(Ordering::SeqCst) {
        return Reply::text(503, "this server is stopping".to_owned());
    }
    take_records_from_store(shared, &mut state, name);
    let known_vault = holds_records(&state, name)
        || state.vaults.contains_key(name)
        || name == crate::ALAB_VAULT
        || shared.pinned_key(name).is_some()
        || local;
    if !known_vault {
        return Reply::text(404, format!("no vault named {name} here"));
    }
    match shared.crossed(&mut state, name) {
        Ok(crossed) => {
            // MUTANT (`crate::crossing::Faults::capped_prefix`): a crossed log's first records,
            // capped at the legacy bound, for a client that states no bound.
            #[cfg(test)]
            if crossed
                && stated.is_none()
                && crate::crossing::faults(shared.crossing_mark(name).path()).capped_prefix
            {
                let mut first = state
                    .records
                    .get(name)
                    .map_or_else(Vec::new, |slot| slot.store.missing(&have));
                first.truncate(crate::membership_log::LEGACY_RECORDS);
                return records_reply(first, false);
            }
            if let Some(reply) = records_fence(crossed, stated) {
                return reply;
            }
        }
        Err(failed) => return failed,
    }
    if let Some(subscriber) = subscriber
        && (holds_records(&state, name) || state.vaults.contains_key(name))
    {
        let now = (shared.tuning.now)();
        if !record_heard(shared, &mut state, name, Some(subscriber), now) {
            return Reply::text(503, "heard history is not tracking reads".to_owned());
        }
    }
    if let Some(slot) = state.records.get(name) {
        let missing = slot.store.missing(&have);
        if !missing.is_empty() {
            let need = server_lacks_have(&state, name, &have);
            drop(state);
            return records_reply(missing, need);
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
            .insert(name.to_owned(), RecordSlot::new(store));
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
    // The records may have crossed while it waited: what came may be a crossed log's.
    let fence = match shared.crossed(&mut state, name) {
        Ok(crossed) => records_fence(crossed, stated),
        Err(failed) => Some(failed),
    };
    let reply = if let Some(fence) = fence {
        fence
    } else {
        match state
            .records
            .get(name)
            .map(|slot| slot.store.missing(&have))
        {
            Some(missing) if !missing.is_empty() => records_reply(missing, lacks),
            _ => {
                let mut reply = Reply::empty(204);
                reply.records_need = lacks;
                reply
            }
        }
    };
    drop(wake);
    // A slot that read a records file stays though it keeps nothing: taken
    // out, the next look would read the whole file again.
    if state.records.get(name).is_some_and(|slot| {
        slot.store.is_empty() && slot.stamp.is_none() && Arc::strong_count(&slot.wake) == 1
    }) {
        state.records.remove(name);
    }
    drop(state);
    reply
}

/// The slice adapter uses the same strict staging and application as a real HTTP POST, from a
/// poster that states it has not crossed.
#[cfg(test)]
fn post_records(shared: &Shared, name: &str, body: &[u8], local: bool) -> Reply {
    post_records_stating(
        shared,
        name,
        body,
        local,
        Some(crate::crossing::RecordsBound::uncrossed()),
    )
}

/// [`post_records`] from a poster that states `stated` of its records.
#[cfg(test)]
fn post_records_stating(
    shared: &Shared,
    name: &str,
    body: &[u8],
    local: bool,
    stated: Option<crate::crossing::RecordsBound>,
) -> Reply {
    match records_input::from_bytes(shared, name, body) {
        Ok(records) => apply_records(shared, name, records, local, stated),
        Err(reply) => reply,
    }
}

/// A completed peer response keeps its original frame metadata through both candidates.
/// Selection does not bypass the current-state checks in the common application path, which
/// takes them as from a poster stating what the peer `stated` of its records.
fn apply_pulled_records(
    shared: &Shared,
    name: &str,
    received: crate::membership_log::untaken::ReceivedFrames,
    stated: Option<crate::crossing::RecordsBound>,
) -> (crate::membership_log::untaken::Sent, Reply) {
    let length = received.byte_len();
    let mut reply = match received.whole() {
        Some(records) => apply_records(
            shared,
            name,
            records_input::StagedRecords::from_verified(records),
            false,
            stated,
        ),
        None => Reply::text(400, "the peer records do not verify as a whole"),
    };
    if reply.status == 400 {
        let part = received.readable();
        let part_len: usize = part.iter().map(|record| record.bytes().len()).sum();
        if !part.is_empty() && part_len < length {
            reply = apply_records(
                shared,
                name,
                records_input::StagedRecords::from_verified(part),
                false,
                stated,
            );
        }
    }
    (received.into_sent(), reply)
}

#[derive(Clone, Copy)]
struct AppendAttempt {
    stamp: fsx::FileStamp,
    from: usize,
    len: usize,
}

/// Bound small-record write calls while keeping the final flush inside the producer result.
fn write_held_buffered(
    writer: &mut dyn Write,
    accepted: &crate::membership_log::Store,
    from: usize,
) -> io::Result<()> {
    let mut buffered = std::io::BufWriter::with_capacity(64 * 1024, writer);
    accepted.write_held_from(from, &mut buffered)?;
    buffered.flush()
}

/// The test's torn append consumes the normal writer while keeping only its prefix.
#[cfg(test)]
struct PrefixWriter<'a> {
    file: &'a mut std::fs::File,
    remaining: usize,
}

#[cfg(test)]
impl Write for PrefixWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let take = bytes.len().min(self.remaining);
        self.file.write_all(&bytes[..take])?;
        self.remaining -= take;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

struct PersistFailure {
    error: Error,
    append_left: Option<fsx::FileStamp>,
}

/// Persist only the accepted Store's complete prefix. The caller owns record_writes and
/// the Writing fence, and publishes the Store only after this operation succeeds.
fn persist_records(
    shared: &Shared,
    name: &str,
    accepted: &crate::membership_log::Store,
    stamp: Option<fsx::FileStamp>,
    on_file: Option<usize>,
) -> Result<Option<fsx::FileStamp>, Box<PersistFailure>> {
    let path = (|| {
        let crossed = shared.crossing_mark(name).stored()?;
        let large = accepted
            .log()
            .is_some_and(|log| crate::crossing::past_legacy(log.held_count()));
        crate::records_storage::write_path_if_unchanged(
            &shared.records_path(name),
            crossed || large,
            stamp,
        )
    })()
    .map_err(|error| {
        Box::new(PersistFailure {
            error,
            append_left: None,
        })
    })?;
    let tail = match (on_file, stamp) {
        (Some(from), Some(stamp)) => Some(AppendAttempt {
            stamp,
            from,
            len: accepted.held_len_from(from).map_err(|error| {
                Box::new(PersistFailure {
                    error: Error::new(error.to_string()),
                    append_left: None,
                })
            })?,
        }),
        _ => None,
    };
    let appended = tail
        .as_ref()
        .and_then(|plan| shared.append_records(&path, accepted, plan).ok());
    match appended {
        Some(left) => Ok(Some(left)),
        None => shared.write_records(&path, accepted).map_err(|error| {
            // Observe a torn append before releasing the shared writer lock. A successor
            // may change this same inode as soon as the failed persistence returns.
            let append_left = tail.and_then(|attempted| {
                fsx::stamp(&path).filter(|now| {
                    (now.dev, now.ino) == (attempted.stamp.dev, attempted.stamp.ino)
                        && now.len > attempted.stamp.len
                        && now.len <= attempted.stamp.len.saturating_add(attempted.len as u64)
                })
            });
            Box::new(PersistFailure { error, append_left })
        }),
    }
}

/// Take strictly staged records into `name`'s store and its file
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1 F23, rules 1 and 4). The file holds the log's
/// complete records only ([`crate::membership_log::Store::encode_held`]); a record waiting on a
/// parent is held in memory only, under the store's bounds. The store is tried on a copy, which
/// shares every record's bytes with the slot's ([`crate::membership_log::Record`]). A POST that
/// adds only waiting records writes nothing and wakes nobody, since a server serves complete
/// records alone. One that grows the log puts what it holds past the file on the end of it
/// ([`fsx::append_at`], already in causal order, so a torn append keeps a causally closed
/// prefix), or writes the file whole when the slot does not know the file holds exactly the
/// log's first records or the append fails. So a store at its bound that lets a waiting record
/// go for a member's complete record appends that record alone, and a `204` means every
/// complete record the store holds is on disk: while the POST writes, a look reads nothing
/// ([`RecordSlot::writing`]). When both writes fail the answer is 500 and the slot's records are
/// unchanged, so a retry adds the records again and writes them; an append cut short gives the
/// slot the file's stamp, so no look takes the part it left.
/// The snapshot's file stamp is checked under the process-shared writer lock before any
/// layout change or write. A retired server's in-flight POST therefore cannot overwrite a
/// successor's newer history; a changed file returns 500 so a retry reads and merges it.
///
/// The body is strictly framed and verified before this function refreshes a file or changes
/// a slot. Invalid input therefore returns 400 before a concurrent file or post-body admission
/// refusal; an incomplete or stalled body still takes precedence over its semantic errors.
/// Its ids are named outside every lock (rule 4.1), and a POST whose records
/// the store holds already (complete or waiting) answers 204 without copying a Store, verifying
/// a signature or writing records (rule 4.2): any LAN host may POST, and the same records come again from
/// every member pass and peer pull. The rest are verified outside every lock too (rule 4.3), so
/// the bodies the server reads at once ([`MAX_PUTS`]) verify at once rather than one after
/// another under `record_writes`. A record the body brings that the store holds waiting is
/// offered again from the store (already verified) beside the new ones: a store at its bound
/// lets its highest waiting record go for each complete one, and a new record that lands before
/// that record's parent in the same body (or another POST meanwhile) could let it go before its
/// parent lands.
///
/// `stated` is what the poster stated of its records ([`crate::transport::RECORDS_BOUND_HEADER`];
/// a peer pull's, what the peer's answer stated). Once `name`'s records crossed
/// ([`crate::crossing`]), a poster that states none gets [`fenced`] and nothing is taken; where
/// this server cannot look for whether they crossed, nothing is taken (the answer is that look's
/// `500`, or `204` to a poster that states a bound and brings only records the store holds). A
/// store that has not crossed ingests a crossed poster's records into its
/// private copy first, and where the copy's complete log grows,
/// stores its mark ([`Shared::mark_crossed`]) before the slot or the file takes the copy; it does
/// so too before it holds more than [`crate::membership_log::LEGACY_RECORDS`] records. A crossed
/// poster's batch that brings only waiting records is dropped and stores no mark. Where the
/// mark cannot be stored the answer is 500 and neither the slot nor the file takes a record.
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "SwarmRecordsFile",
        action = "PostP",
        project = "aterm_secrets::server::tests::records_file_projection"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "SwarmRecordsFile",
        action = "PostC",
        project = "aterm_secrets::server::tests::records_file_projection"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "SwarmRecordsFile",
        action = "PostJ",
        project = "aterm_secrets::server::tests::records_file_projection"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "SwarmRecordsFile",
        action = "Write",
        project = "aterm_secrets::server::tests::records_file_projection"
    )
)]
fn apply_records(
    shared: &Shared,
    name: &str,
    staged: records_input::StagedRecords,
    local: bool,
    stated: Option<crate::crossing::RecordsBound>,
) -> Reply {
    match crate::records_storage::pending(&shared.records_path(name)) {
        Ok(true) => {
            let _writing = shared
                .record_writes
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Err(error) = crate::records_storage::recover_pending(&shared.records_path(name))
            {
                return Reply::text(500, error.to_string());
            }
        }
        Err(error) => return Reply::text(500, error.to_string()),
        Ok(false) => {}
    }
    {
        let mut state = shared.lock_server_state();
        if let Some(reply) = unreadable_records(shared, &mut state, name) {
            return reply;
        }
        if let Some(reply) = refuse_unheld_records(shared, &state, name, local) {
            return reply;
        }
        match shared.crossed(&mut state, name) {
            Ok(crossed) => {
                if let Some(reply) = records_fence(crossed, stated) {
                    return reply;
                }
            }
            Err(failed) => return failed,
        }
        if state.records.get(name).is_some_and(|slot| {
            staged
                .records()
                .iter()
                .all(|record| slot.store.contains(record.id()))
        }) {
            return Reply::empty(204);
        }
    }
    let _writing = shared
        .record_writes
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let (mut copy, stamp, on_file, crossed) = {
        let mut state = shared.lock_server_state();
        if let Some(reply) = unreadable_records(shared, &mut state, name) {
            return reply;
        }
        if let Some(reply) = refuse_unheld_records(shared, &state, name, local) {
            return reply;
        }
        let crossed = match shared.crossed(&mut state, name) {
            Ok(crossed) => crossed,
            Err(failed) => return failed,
        };
        if let Some(reply) = records_fence(crossed, stated) {
            return reply;
        }
        if state.records.get(name).is_some_and(|slot| {
            staged
                .records()
                .iter()
                .all(|record| slot.store.contains(record.id()))
        }) {
            return Reply::empty(204);
        }
        let keys = record_keys(shared, &state, name);
        match state.records.get(name) {
            Some(slot) => (
                {
                    #[cfg(test)]
                    shared.post_clones.fetch_add(1, Ordering::SeqCst);
                    follow_held(slot.store.clone(), keys.as_deref())
                },
                slot.stamp,
                slot.on_file,
                crossed,
            ),
            None => match crate::membership_log::Store::for_vault(name) {
                Ok(store) => (follow_held(store, keys.as_deref()), None, None, crossed),
                Err(e) => return Reply::text(500, e.to_string()),
            },
        }
    };
    // What a crossed log brings crosses this store where the store takes a complete record of
    // it.
    let from_crossed = !crossed && stated.is_some_and(|bound| bound.crossed);
    // MUTANT (`crate::crossing::Faults::mark_on_receipt`): the mark before the copy shows it
    // takes anything.
    #[cfg(test)]
    if from_crossed
        && crate::crossing::faults(shared.crossing_mark(name).path()).mark_on_receipt
        && let Err(reply) = shared.mark_crossed(name)
    {
        return reply;
    }
    let held_before = copy.log().map_or(0, crate::membership_log::Log::held_count);
    let mut records = staged.into_records();
    records.retain(|record| !copy.holds(record.id()));
    let added = match copy.ingest_records(records) {
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
    let Some(held) = copy.log().map(crate::membership_log::Log::held_count) else {
        return Reply::text(
            422,
            "a store with no genesis takes records only with the genesis that roots them",
        );
    };
    if held == held_before {
        // Only waiting records came. A crossed log's are not taken, and store no mark: a record
        // waits for any signer that ever held a seat (`Log::insert`), a removed Mac's among
        // them, with a parent that may never come, so taking them would let such a key cross
        // this vault for good with a record that, its parent made up, is never served. An
        // honest crossed poster's records wait here only on a parent this server lost (a
        // restart) or could not take, and they come again with it when the poster next posts
        // its whole store (`client::Reposted`), the mark then stored before any of them is
        // held.
        if from_crossed {
            // MUTANT (`crate::crossing::Faults::mark_on_waiting`): the mark for them, and the
            // slot takes them.
            #[cfg(test)]
            if crate::crossing::faults(shared.crossing_mark(name).path()).mark_on_waiting {
                if let Err(reply) = mark_before_take(shared, name) {
                    return reply;
                }
                hold_waiting(shared, name, copy, stamp, on_file);
            }
            return Reply::empty(204);
        }
        hold_waiting(shared, name, copy, stamp, on_file);
        return Reply::empty(204);
    }
    // The copy holds more complete records than the slot. Where a crossed log taught them, the
    // mark before the slot or the file takes the copy, so nothing a crossed log taught this
    // server is held without the mark. Nothing has seen the copy yet, so a batch it takes none
    // of (a `400`, a `422`, records it holds, records that only wait on a parent) stores no
    // mark.
    if from_crossed && let Err(reply) = mark_before_take(shared, name) {
        return reply;
    }
    // A log past the legacy bound crosses this store: the mark before the file or the slot holds
    // it.
    let past_legacy = !crossed && !from_crossed && crate::crossing::past_legacy(held);
    if past_legacy && let Err(reply) = mark_before_take(shared, name) {
        return reply;
    }
    let _writing = Writing::mark(shared, name);
    let written = match persist_records(shared, name, &copy, stamp, on_file) {
        Ok(left) => left,
        Err(failure) => {
            note(&format!(
                "{name}: could not store records: {}",
                failure.error
            ));
            // An append cut short left part of its records on the end of the file, never
            // flushed: the slot takes the stamp captured under the writer lock so no look
            // takes them in (a POST of them would answer 204 while they are not durable), and
            // the next growth writes the file whole. A file another writer put there differs
            // from that stamp, so a look reads it.
            let mut state = shared.lock_server_state();
            if let (Some(left), Some(slot)) = (failure.append_left, state.records.get_mut(name)) {
                slot.stamp = Some(left);
                slot.on_file = None;
            }
            return Reply::text(500, "the server could not write to its store");
        }
    };
    // A whole write whose file another writer replaced at once leaves no stamp: the next look
    // reads the file, and the next growth writes it whole.
    let on_file = written.map(|_| held);
    let mut state = shared.lock_server_state();
    match state.records.get_mut(name) {
        Some(slot) => {
            // No look read the file while it was written ([`RecordSlot::writing`]).
            slot.store = copy;
            slot.stamp = written;
            slot.on_file = on_file;
            slot.writing = false;
            #[cfg(test)]
            shared.record_wakes.fetch_add(1, Ordering::SeqCst);
            slot.wake.notify_all();
        }
        None => {
            state
                .records
                .insert(name.to_owned(), RecordSlot::stamped(copy, written, on_file));
        }
    }
    drop(state);
    #[cfg(test)]
    if (from_crossed || past_legacy)
        && let Err(reply) = mutant_mark_after_take(shared, name)
    {
        return reply;
    }
    Reply::empty(204)
}

/// The slot for `name` takes `copy`, which holds no complete record past the slot's, only
/// records waiting on a parent: nothing for the file, and nothing new to serve. The slot takes
/// the `stamp` and `on_file` it had when copied, so a file a look read meanwhile is read again.
fn hold_waiting(
    shared: &Shared,
    name: &str,
    copy: crate::membership_log::Store,
    stamp: Option<fsx::FileStamp>,
    on_file: Option<usize>,
) {
    let mut state = shared.lock_server_state();
    match state.records.get_mut(name) {
        Some(slot) => {
            slot.store = copy;
            slot.stamp = stamp;
            slot.on_file = on_file;
        }
        None => {
            state
                .records
                .insert(name.to_owned(), RecordSlot::stamped(copy, stamp, on_file));
        }
    }
}

/// Store `name`'s crossed mark before its store takes records that cross it
/// ([`Shared::mark_crossed`]). In a test, the server may die right after
/// ([`crate::crossing::Faults::dies_between`]), or under the mutant that takes first
/// ([`crate::crossing::Faults::mark_after_take`]) store nothing here.
fn mark_before_take(shared: &Shared, name: &str) -> Result<(), Reply> {
    #[cfg(test)]
    let faults = crate::crossing::faults(shared.crossing_mark(name).path());
    #[cfg(test)]
    if faults.mark_after_take {
        return Ok(());
    }
    shared.mark_crossed(name)?;
    #[cfg(test)]
    if faults.dies_between {
        return Err(died_here());
    }
    Ok(())
}

/// MUTANT ([`crate::crossing::Faults::mark_after_take`]): the mark once the store took the
/// records that crossed it, the server dying first where the test says so.
#[cfg(test)]
fn mutant_mark_after_take(shared: &Shared, name: &str) -> Result<(), Reply> {
    let faults = crate::crossing::faults(shared.crossing_mark(name).path());
    if !faults.mark_after_take {
        return Ok(());
    }
    if faults.dies_between {
        return Err(died_here());
    }
    shared.mark_crossed(name)
}

/// What a test's server answers where it dies ([`crate::crossing::Faults::dies_between`]): the
/// test reads what it left on disk with a server started anew, as after a process that ended.
#[cfg(test)]
fn died_here() -> Reply {
    Reply::text(500, "a test's server died here")
}

/// A records application's look at `name`'s file ([`look_for_records`]): a 500 when the read failed,
/// since the POST would write what the store holds, and a file it could not read may hold
/// records the store lacks. A file the read refuses is replaced by the next growth.
fn unreadable_records(shared: &Shared, state: &mut State, name: &str) -> Option<Reply> {
    match look_for_records(shared, state, name) {
        Err(fsx::Unread::Failed(e)) => {
            note(&format!("{name}: could not read records: {e}"));
            Some(Reply::text(500, "the server could not read its store"))
        }
        Ok(()) | Err(fsx::Unread::Refused(_)) => None,
    }
}

/// `POST /v1/<name>/join` from `peer`: a bare request line, with a member's Allow or a join
/// password proof where it carries one, never a listing's time or LAN marker.
fn post_join(shared: &Shared, name: &str, body: &[u8], peer: IpAddr) -> Reply {
    let Ok(text) = std::str::from_utf8(body) else {
        return Reply::text(400, "a join request is one line of text");
    };
    let line = text.strip_suffix('\n').unwrap_or(text);
    if line.contains(['\n', '\r']) {
        return Reply::text(400, "a join request is one line: `age1… <host> <code>`");
    }
    let request = match parse_join_line(line) {
        Ok(request) if request.time.is_none() => request,
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

/// Decide a join request from `peer` under `server_state`, or refuse it
/// ([`Shared::join_writes`]).
fn decide_join(
    shared: &Shared,
    name: &str,
    request: JoinRequest,
    peer: IpAddr,
    now: u64,
) -> Result<Decided, Reply> {
    let mut state = shared.lock_server_state();
    // A version an admin published at the store file counts as a PUT of it does
    // ([`take_from_store`]): a request is asked at its seq, and one asked at an older seq gives
    // way.
    take_from_store(shared, &mut state, name);
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
/// a request is queued or changed or the vault takes a new version, a PUT's or one an admin
/// published at the store file ([`take_from_store`]) (`200`, the lines), or the hold runs out
/// (`204`): an admin Mac's approver hears a new request at once, with one request per hold
/// while none comes.
fn list_joins(shared: &Shared, stream: &TcpStream, name: &str, query: Option<&str>) -> Reply {
    let after = match after_query(query) {
        Ok(after) => after,
        Err(reply) => return reply,
    };
    let mut state = shared.lock_server_state();
    take_from_store(shared, &mut state, name);
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
            take_from_store(shared, state, name);
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
        records_boot: false,
        records_bound: None,
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

/// `GET /v1/<name>/heard`: each subscriber this server heard for `name` within
/// [`HEARD_TTL_SECS`], one `<tag> <unix seconds>` line each, the tag a
/// [`crate::transport::heard_tag`] of its recipient, never the key itself or a value. A member
/// reads it to tell whether a Mac with no device-key proof in the membership log (a 0.145 Mac)
/// still subscribes (`DESIGN-fluid-swarm` F20).
fn list_heard(shared: &Shared, name: &str) -> Reply {
    let mut state = shared.lock_server_state();
    if !state.vaults.contains_key(name) && !holds_records(&state, name) {
        return Reply::text(404, format!("no vault named {name} here"));
    }
    let now = (shared.tuning.now)();
    let Some(since) = state
        .heard_coverage
        .as_mut()
        .and_then(|coverage| coverage.complete_since(now))
        .filter(|_| !shared.stopping.load(Ordering::SeqCst))
    else {
        return Reply::text(
            503,
            "heard history does not yet cover the legacy window".to_owned(),
        );
    };
    let mut text = String::new();
    let mut count = 0;
    for (recipient, &at) in state
        .heard
        .get(name)
        .into_iter()
        .flat_map(|rows| rows.subscribers.iter())
    {
        if now.saturating_sub(at) > HEARD_TTL_SECS {
            continue;
        }
        if let Ok(parsed) = crate::age::Recipient::parse(recipient) {
            text.push_str(&format!("{} {at}\n", crate::transport::heard_tag(&parsed)));
            count += 1;
        }
    }
    let anonymous = state
        .heard
        .get(name)
        .and_then(|rows| rows.anonymous)
        .map_or_else(|| "-".to_owned(), |at| at.to_string());
    let text = format!("aterm-heard-v2 {since} {now} {count} {anonymous}\n{text}");
    Reply {
        status: 200,
        body: Body::Text(text),
        etag: None,
        vault_head: None,
        allow: None,
        retry_after: None,
        records_need: false,
        records_boot: false,
        records_bound: None,
    }
}

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
        records_boot: false,
        records_bound: None,
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
    aterm_codec::hex::encode_lower(bytes)
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

/// Where one registration of the vault's advert is made: the address it names and, on a Mac on
/// several networks at once, the interface it is scoped to ([`advert_targets`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The interface (`en0`) the registration is made on alone. An absent scope cannot register.
    pub scope: Option<String>,
    /// The address `<name>.local` names.
    pub ip: Ipv4Addr,
}

/// The advert registering `<name>.local` at `target`'s address and the vault service on `port`
/// (the proxy form: the host record beside the service), on `target`'s interface alone where it
/// is scoped, with `txt`'s vault name, signer fingerprint, `seq` and `head` digest beside
/// `path=/v1` once a vault is held (`path=/v1` alone while the store holds none).
///
/// # Errors
/// The scope is missing or malformed (`InvalidInput`), or names an interface this Mac does
/// not have (`NotFound`).
pub fn vault_advert(
    target: &Target,
    name: &str,
    port: u16,
    txt: Option<&VaultTxt>,
) -> io::Result<aterm_uds::lan::Advert> {
    let scope = target
        .scope
        .as_deref()
        .filter(|name| advert_interface(name))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "a vault advert needs a LAN interface",
            )
        })?;
    let interface = Some(interface_index(scope)?);
    let txt = std::iter::once("path=/v1".to_owned())
        .chain(txt.map(VaultTxt::txt_pairs).unwrap_or_default())
        .map(String::into_bytes)
        .collect();
    Ok(aterm_uds::lan::Advert {
        name: name.to_owned(),
        service: SERVICE_TYPE.to_owned(),
        domain: "local".to_owned(),
        port,
        txt,
        interface,
        host: Some(format!("{name}.local")),
        addresses: vec![IpAddr::V4(target.ip)],
    })
}

/// Whether `name` is a plain interface name, never an option or a wildcard.
fn advert_interface(name: &str) -> bool {
    name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

/// The index of the interface named `name` (`en0`), as this Mac lists its links.
fn interface_index(name: &str) -> io::Result<u32> {
    aterm_sysprobe::net_links()
        .unwrap_or_default()
        .into_iter()
        .find(|link| link.name == name && link.index != 0)
        .map(|link| link.index)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("this Mac has no interface {name}"),
            )
        })
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
/// Each registration is scoped to one network's interface and names that interface's own
/// address, including on a Mac with only one LAN. `routed`'s interface goes first, and serves
/// its own network. Each link hears an address valid on it, whichever link the default route
/// takes. Unscoped proxy records also claim loopback, where macOS's own hostname record can
/// have another address: a vault advertised under the Mac's name then conflicts with itself.
///
/// A network is the IPv4 subnet of an interface that is up and running and can broadcast and
/// multicast, so no tunnel or loopback counts. Link-local addresses are left out. An interface
/// counts once: an interface holding addresses on several subnets is counted at its lowest
/// network's address. A routed address with no eligible interface registers nothing of its own.
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
        .filter(|at| advert_interface(&at.name))
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

/// Primary IPv4 address of the default-route interface: the address this Mac sends from toward
/// TEST-NET-1 (`192.0.2.1`), which only a default route takes.
///
/// On macOS it is read from that route in the routing table
/// ([`aterm_uds::network::route_source_ipv4_asked`]), which is the address a UDP socket
/// connected there takes, and the connect is made only where the table could not be read. On a
/// Mac with no default route that connect fails, and each failure makes the kernel send an
/// `RTM_MISS` to every routing socket on the Mac; every converge pass, vault subscriber and
/// advertiser reads these routes.
pub fn primary_ipv4() -> Option<Ipv4Addr> {
    #[cfg(target_os = "macos")]
    let asked = aterm_uds::network::route_source_ipv4_asked;
    #[cfg(not(target_os = "macos"))]
    let asked = |_| None;
    primary_ipv4_by(asked, |to| source_ipv4(to, 9))
}

/// [`primary_ipv4`] over its two readers: `asked` is the routing table's word for a destination
/// (`None` where the table was not read), and `connected` the address a UDP socket connected to
/// it takes, called only where `asked` has no word.
fn primary_ipv4_by(
    asked: impl FnOnce(Ipv4Addr) -> Option<Option<Ipv4Addr>>,
    connected: impl FnOnce(Ipv4Addr) -> Option<Ipv4Addr>,
) -> Option<Ipv4Addr> {
    let to = Ipv4Addr::new(192, 0, 2, 1);
    asked(to).unwrap_or_else(|| connected(to))
}

/// LAN IPv4 address at which `<name>.local` is advertised ([`primary_ipv4`]).
///
/// On macOS it is read from the route to mDNS's group in the routing table
/// ([`aterm_uds::network::route_source_ipv4`]), never by a UDP connect to the group: that connect
/// would be a use of the local network outside the LAN broker, which macOS asks its Local Network
/// question about (every converge pass and vault subscriber reads these routes).
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

/// The IPv6 address the default route leaves by: the address a UDP socket connected to the
/// documentation prefix (`2001:db8::1`) takes, which is the kernel's own choice of source (the
/// route's own address is not it: IPv6 source selection weighs more than the route).
///
/// On macOS the routing table is asked first whether any route takes that destination
/// ([`aterm_uds::network::routed_ipv6`]), and where it says none does, no connect is made: the
/// connect would fail, and each failure makes the kernel send an `RTM_MISS` to every routing
/// socket on the Mac (a Mac with no IPv6 default route is the common case, and every converge
/// pass, vault subscriber and advertiser reads these routes). Where a route takes it, or the
/// table could not be read, the connect is made as before.
fn primary_ipv6() -> Option<Ipv6Addr> {
    #[cfg(target_os = "macos")]
    let routed = aterm_uds::network::routed_ipv6;
    #[cfg(not(target_os = "macos"))]
    let routed = |_| None;
    primary_ipv6_by(routed, source_ipv6)
}

/// [`primary_ipv6`] over its two readers: `routed` is the routing table's word on whether a
/// route takes a destination (`None` where the table was not read), and `connected` the address
/// a UDP socket connected to it takes, never called where `routed` says no route does.
fn primary_ipv6_by(
    routed: impl FnOnce(Ipv6Addr) -> Option<bool>,
    connected: impl FnOnce(Ipv6Addr) -> Option<Ipv6Addr>,
) -> Option<Ipv6Addr> {
    let to = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1);
    if routed(to) == Some(false) {
        return None;
    }
    connected(to)
}

/// The local address a UDP socket connected to `to` gets (no packet is sent).
fn source_ipv6(to: Ipv6Addr) -> Option<Ipv6Addr> {
    let socket = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((to, 9)).ok()?;
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

/// How long [`Registered::ended`] waits on a broker registration's connection for a frame: long
/// enough that the poll is asked, never a wait for something to come.
const REGISTRATION_PEEK: Duration = Duration::from_millis(1);

/// Where a registration the LAN broker makes for this server stands
/// ([`Registered::through_broker`]).
enum Brokered {
    /// mDNSResponder has not confirmed it yet: a thread of its own waits for that.
    #[cfg(target_os = "macos")]
    Starting,
    /// Confirmed, and held while this is.
    Held(aterm_uds::lan::Registration),
    /// It failed before it was confirmed, ended by itself, or was ended: nothing is held.
    Gone,
}

/// One registration of the vault's advert this server holds: the LAN broker's, which lasts while
/// it is held ([`aterm_uds::lan::register`]), or a test's stand-in.
enum Registered {
    /// The broker's registration, shared with the thread that waits for its confirmation.
    Broker(Arc<Mutex<Brokered>>),
    /// A test's stand-in: a child that is up while the registration is.
    #[cfg(test)]
    StandIn(aterm_uds::spawnfd::Child),
}

impl Registered {
    /// Start registering `advert` through the LAN broker, which holds it while this lives, and
    /// return once the broker answered its hello, not once mDNSResponder confirmed the advert:
    /// that takes its probing (the proxy form probes the host record too), and the round that
    /// starts it holds the children's lock a will-sleep waits on ([`take_for_sleep`]). A thread
    /// of its own waits for the confirmation and notes a registration that failed; one ended
    /// before it was confirmed is dropped, and so withdrawn, as the confirmation comes.
    ///
    /// # Errors
    /// The broker is unavailable (its words say what installs it), or the thread could not
    /// start; off macOS, the registration's own refusal.
    fn through_broker(advert: &aterm_uds::lan::Advert) -> io::Result<Self> {
        #[cfg(not(target_os = "macos"))]
        {
            aterm_uds::lan::register(advert)
                .map(|registration| {
                    Self::Broker(Arc::new(Mutex::new(Brokered::Held(registration))))
                })
                .map_err(aterm_uds::lan::LanError::into_io)
        }
        #[cfg(target_os = "macos")]
        {
            aterm_uds::lan::broker::hello().map_err(aterm_uds::lan::LanError::into_io)?;
            let state = Arc::new(Mutex::new(Brokered::Starting));
            let confirmed = Arc::clone(&state);
            let advert = advert.clone();
            #[cfg(any(test, debug_assertions))]
            let seam = aterm_uds::lan::seam();
            std::thread::Builder::new()
                .name("vault-register".to_owned())
                .spawn(move || {
                    let register = || aterm_uds::lan::register(&advert);
                    #[cfg(any(test, debug_assertions))]
                    let registered = aterm_uds::lan::seam_here(seam, register);
                    #[cfg(not(any(test, debug_assertions)))]
                    let registered = register();
                    let mut state = confirmed.lock().unwrap_or_else(PoisonError::into_inner);
                    if !matches!(*state, Brokered::Starting) {
                        // Ended while it started: the registration drops here, withdrawn.
                        return;
                    }
                    *state = match registered {
                        Ok(registration) => Brokered::Held(registration),
                        Err(e) => {
                            note(&format!("could not advertise {}.local: {e}", advert.name));
                            Brokered::Gone
                        }
                    };
                })?;
            Ok(Self::Broker(state))
        }
    }

    /// Whether it ended by itself: mDNSResponder gave the host name up to a device that answers
    /// to it (the broker's error frame ends the registration, or its confirmation never came),
    /// or the broker's connection ended. One still starting has not ended. Every frame already
    /// waiting is read; a zero wait would read none (an expired deadline asks no poll), so each
    /// read waits [`REGISTRATION_PEEK`] for what is already there.
    fn ended(&mut self) -> bool {
        match self {
            Self::Broker(state) => {
                let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
                let Brokered::Held(registration) = &mut *state else {
                    return matches!(*state, Brokered::Gone);
                };
                let ended = loop {
                    match registration.events().next_within(REGISTRATION_PEEK) {
                        // A later event (a rename) leaves it registered; read on.
                        Ok(Some(_)) => {}
                        Ok(None) => break false,
                        Err(_) => break true,
                    }
                };
                if ended {
                    *state = Brokered::Gone;
                }
                ended
            }
            #[cfg(test)]
            Self::StandIn(child) => matches!(child.try_wait(), Ok(Some(_))),
        }
    }

    /// End it now, at once: the broker withdraws a held advert as its connection closes, and one
    /// still starting as its confirmation comes ([`Self::through_broker`]).
    fn end(&mut self) {
        match self {
            Self::Broker(state) => {
                let ended = std::mem::replace(
                    &mut *state.lock().unwrap_or_else(PoisonError::into_inner),
                    Brokered::Gone,
                );
                drop(ended);
            }
            #[cfg(test)]
            Self::StandIn(child) => end(child),
        }
    }
}

#[cfg(test)]
impl Registered {
    /// The stand-in's child, which a test reads and ends as it would the registration.
    fn stand_in(&mut self) -> &mut aterm_uds::spawnfd::Child {
        match self {
            Self::StandIn(child) => child,
            Self::Broker(_) => panic!("a test holds stand-ins"),
        }
    }

    /// The stand-in's pid.
    fn id(&self) -> u32 {
        match self {
            Self::StandIn(child) => child.id(),
            Self::Broker(_) => panic!("a test holds stand-ins"),
        }
    }

    /// Kill the stand-in, as mDNSResponder ending the registration would.
    fn kill(&mut self) -> io::Result<()> {
        self.stand_in().kill()
    }

    /// Reap the stand-in.
    fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        self.stand_in().wait()
    }

    /// Whether the stand-in exited.
    fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        self.stand_in().try_wait()
    }
}

/// A test's stand-in registration: `program` with `args`, up while the registration would be.
#[cfg(test)]
fn quiet_advert(program: &str, args: &[String]) -> io::Result<Registered> {
    quiet(program, args).map(Registered::StandIn)
}

/// The registrations of one name ([`reregister_slot`]).
#[derive(Default)]
struct AdvertSlot {
    /// Each registration running: where, its TXT, and what holds it.
    held: Vec<(Target, Option<VaultTxt>, Registered)>,
    /// The instance label currently held by [`Self::held`].
    held_name: Option<String>,
    /// The addresses last logged as advertised, so TXT refreshes on new seqs stay quiet.
    logged: Vec<Ipv4Addr>,
    /// A registration ended by itself (mDNSResponder gives up a name another device on the
    /// network answers to), so the log said so, until every one stays up.
    lost: bool,
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
    dns: AdvertSlot,
    /// Secondary lease-followed registration for `alab-secrets.local` (§8.3).
    alab_dns: AdvertSlot,
    /// The log said a held `alab-secrets` registration lost the name ([`reregister_roles`]),
    /// so it says so again only after a later one stays up for a round.
    alab_lost_said: bool,
    /// The log said a held `alab-secrets` registration ended with no other device answering
    /// to the name, so this server registers it again ([`reregister_roles`]); it says so again
    /// only after a later one stays up for a round.
    alab_again_said: bool,
    /// A will-sleep took this server's registrations ([`take_for_sleep`]), so the next round
    /// lets the `alab-secrets` name go before anything is registered again
    /// ([`reregister_awake`]).
    slept: bool,
    /// Where this server leaves a [`HeldStamp`] if it stops while it holds `alab-secrets`
    /// ([`ALAB_HELD_FILE`] in its store; `None`: it does not advertise).
    held_file: Option<PathBuf>,
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
#[cfg(not(test))]
const PEER_PULL_BUDGET: Duration = Duration::from_secs(2);
/// A unit test's is its one-minute hang detector: the peers its servers pull from are its own
/// loopback stand-ins, whose answer is waited for rather than taken for a peer gone from the LAN
/// (`tests/` lifts it through [`start_with_peer_pull`]).
#[cfg(test)]
const PEER_PULL_BUDGET: Duration = aterm_spec::wait::MUST_HAPPEN;
/// Budget for a pull from a `0.145` peer (`txt == None`), which ignores `X-Aterm-No-Wait` and
/// parks when it holds nothing newer.
const LEGACY_PEER_PULL_BUDGET: Duration = Duration::from_millis(750);

fn is_self_addr(addr: SocketAddr, own_port: u16, here: &[Target]) -> bool {
    let ip = addr.ip().to_canonical();
    addr.port() == own_port && (ip.is_loopback() || here.iter().any(|t| IpAddr::V4(t.ip) == ip))
}

fn trimmed_dns_host(host: &str) -> &str {
    host.strip_suffix('.').unwrap_or(host)
}

fn strip_local_suffix(host: &str) -> &str {
    let host = trimmed_dns_host(host);
    host.len()
        .checked_sub(".local".len())
        .and_then(|at| host.split_at_checked(at))
        .and_then(|(stem, suffix)| suffix.eq_ignore_ascii_case(".local").then_some(stem))
        .unwrap_or(host)
}

fn is_obviously_self_url(
    uri: &aterm_http::Uri,
    own_port: u16,
    own_label: Option<&str>,
    here: &[Target],
) -> bool {
    if uri.port() != own_port {
        return false;
    }
    if let Ok(ip) = uri.host().parse::<IpAddr>() {
        let ip = ip.to_canonical();
        return ip.is_loopback() || here.iter().any(|t| IpAddr::V4(t.ip) == ip);
    }
    if let Some(label) = own_label
        && strip_local_suffix(uri.host()).eq_ignore_ascii_case(label)
    {
        return true;
    }
    false
}

/// How many peers' records a server reads at once ([`pull_from_peer`]): each body is up to
/// [`crate::membership_log::MAX_RECORDS_STREAM`]. Frames reuse exact known owners or verify as
/// they arrive, and the completed response enters [`apply_records`]. A pass asks its peers (up to
/// [`crate::discover::MAX_CANDIDATES`]) for their envelopes at once, which finds the dark ones
/// in one budget, and holds this many records bodies: one read while another is stored. A
/// client's records pass reads its peers' longer answers as many at once (`crate::client`'s
/// `fan_out::RECORDS`).
pub(crate) const RECORDS_PULLS_AT_ONCE: usize = 2;

/// How much of each peer's envelope answer a pass reads at once from all its peers
/// ([`pull_from_peer`]): a `204` and a small envelope fit. A longer answer is asked again and
/// read whole, up to [`MAX_ENVELOPE`], under one of [`ENVELOPE_PULLS_AT_ONCE`] slots. A client
/// reading many servers' envelopes at once (`crate::client`'s `fan_out::envelopes_within`) reads
/// each answer the same way.
pub(crate) const ENVELOPE_PEEK: usize = 256 << 10;

/// How many peers' whole envelopes longer than [`ENVELOPE_PEEK`] a server reads at once: each
/// is up to [`MAX_ENVELOPE`] and is parsed again where [`put_vault`] stores it, so a host that
/// advertises 16 instances of itself costs a pass this many such bodies, not 16. A client's
/// fan-out of envelope reads through `crate::client`'s `fan_out::reads_within` (an admin
/// command's open, the servers' heads read before a mirror, the sync's probes of its peers, a
/// join's probes, and a join request's check of each mirror before it is posted there,
/// `join_routing`, on a joining Mac) holds as many at once, and one
/// from each compiled-in default address, each until it has verified it, and a join request's
/// until it has also left the request with that server. Two client fan-outs that read only the
/// servers the signed swarm policy names, never one a browse named, still read each of them
/// whole at once: the check of each mirror whose join queue an admin Mac reads (`join_queues`)
/// and an admission's read-back (`ledger_publish::verify_approved_in_served_head`).
pub(crate) const ENVELOPE_PULLS_AT_ONCE: usize = 2;

/// Slots for a few pulls at once ([`RECORDS_PULLS_AT_ONCE`] records pulls,
/// [`ENVELOPE_PULLS_AT_ONCE`] whole envelopes).
pub(crate) struct PullSlots {
    /// How many pulls hold a slot at once.
    at_once: usize,
    taken: Mutex<usize>,
    freed: Condvar,
    /// Pulls waiting for a slot now, shared with a test that watches the slots a client's
    /// fan-out made (`crate::client`'s `fan_out::held`).
    #[cfg(test)]
    pub(crate) waiting: Arc<AtomicUsize>,
}

/// One pull's slot, given back when dropped.
pub(crate) struct PullSlot<'a>(&'a PullSlots);

impl PullSlots {
    /// Slots for `at_once` pulls.
    pub(crate) fn new(at_once: usize) -> PullSlots {
        PullSlots {
            at_once,
            taken: Mutex::new(0),
            freed: Condvar::new(),
            #[cfg(test)]
            waiting: Arc::default(),
        }
    }

    /// A slot, once one is free: a slot's pull ends within its budget.
    pub(crate) fn take(&self) -> PullSlot<'_> {
        let mut taken = self.taken.lock().unwrap_or_else(PoisonError::into_inner);
        #[cfg(test)]
        let queued = usize::from(*taken >= self.at_once);
        #[cfg(test)]
        self.waiting.fetch_add(queued, Ordering::SeqCst);
        while *taken >= self.at_once {
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
/// are read [`RECORDS_PULLS_AT_ONCE`] at a time, and their envelopes longer than
/// [`ENVELOPE_PEEK`] [`ENVELOPE_PULLS_AT_ONCE`] at a time. Returns how many vaults advanced.
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
///
/// A records pull states what this server holds of the vault's records, as a member's does
/// ([`crate::crossing`]): once they crossed, it takes nothing from a peer whose answer states no
/// bound, and records from a peer that states it crossed are taken as a crossed poster's
/// ([`apply_records`] stores the mark first). A vault whose crossing this server cannot look for
/// is not pulled this pass.
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
        let (mut held_seq, mut held_head, accepted_envelope, accepted_records) = {
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
            use crate::transport::{Fetched, Pulled, observe_after_nowait_within};
            // Every peer's answer is read under a small limit at once; a longer one is asked
            // again and read whole under a slot, held until it is stored.
            let mut slot = None;
            let mut pulled =
                observe_after_nowait_within(&vault_url, held_seq, budget, ENVELOPE_PEEK);
            if matches!(pulled, Ok(Pulled::Larger)) {
                slot = Some(shared.envelope_pulls.take());
                // A pull that held a slot before this one may have stored a newer version (one a
                // publish spread to several peers): ask after what is held now, so a peer holding
                // nothing newer answers with no body.
                (held_seq, held_head) = {
                    let mut state = shared.lock_server_state();
                    take_from_store(shared, &mut state, &vault_name);
                    state.vaults.get(&vault_name).map_or((None, None), |slot| {
                        (Some(slot.held.seq), Some(slot.held.head))
                    })
                };
                pulled = observe_after_nowait_within(&vault_url, held_seq, budget, MAX_ENVELOPE);
            }
            match pulled {
                Ok(Pulled::Fetched(Fetched::Body(bytes))) => {
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
                Ok(Pulled::Fetched(_)) => {}
                Ok(Pulled::Larger) | Err(_) => return stored,
            }
            drop(slot);
        }
        if accepted_records {
            let _slot = shared.records_pulls.take();
            let now = Instant::now();
            // The ids this pull names and the frontier of the store they came from: what the peer
            // sends that the store does not take is noted only against a store that still holds
            // that frontier.
            let (have_records, named_from) = {
                let state = shared.lock_server_state();
                match state.records.get(&vault_name) {
                    Some(slot) => {
                        let have = slot.store.have();
                        let named =
                            shared
                                .untaken
                                .have(&vault_name, &vault_url, &slot.store, &have, now);
                        (named.ids, slot.store.frontier())
                    }
                    None => (Vec::new(), Vec::new()),
                }
            };
            // A pull that cannot say whether the records crossed takes nothing this pass.
            let Ok(crossed) = shared.records_crossed(&vault_name) else {
                continue;
            };
            let answer = match crate::transport::fetch_record_frames_with_known(
                &vault_url,
                &have_records,
                None,
                budget,
                None,
                crate::crossing::RecordsBound::here(crossed),
                |id| records_input::known_record(shared, &vault_name, id),
            ) {
                Ok(answer) => answer,
                Err(_) => return stored,
            };
            let stated = answer.bound.stated();
            // A crossed server takes nothing from a peer of a build before the fence.
            if crossed && stated.is_none() {
                continue;
            }
            let received = answer.received;
            if received.byte_len() == 0 {
                continue;
            }
            let before_len = shared
                .lock_server_state()
                .records
                .get(&vault_name)
                .map_or(0, |slot| slot.store.len());
            let (stream, reply) = apply_pulled_records(shared, &vault_name, received, stated);
            // Only a verdict says what the store cannot take: a `204` (it took what it could), or
            // a `400` (every record refused, or none this build reads). A `500` (it could not
            // write its store), `507` or `403` judged none of it, and the same pull may land
            // next pass.
            if matches!(reply.status, 204 | 400) {
                let state = shared.lock_server_state();
                if let Some(slot) = state.records.get(&vault_name) {
                    shared.untaken.note(
                        vault_name.clone(),
                        &vault_url,
                        &stream,
                        &crate::membership_log::untaken::Noted {
                            store: &slot.store,
                            named_from: &named_from,
                            now,
                            source: None,
                        },
                    );
                }
            }
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
        && trimmed_dns_host(uri.host()).eq_ignore_ascii_case("alab-secrets.local")
    {
        let addrs =
            aterm_uds::lan::resolve(trimmed_dns_host(uri.host()), ALAB_PROBE_BUDGET).ok()?;
        let mut best: Option<u64> = None;
        for addr in addrs.into_iter().map(|ip| SocketAddr::new(ip, uri.port())) {
            if is_self_addr(addr, own_port, here) {
                continue;
            }
            let direct = format!("http://{addr}/v1/{}", crate::ALAB_VAULT);
            if let Ok(crate::transport::Fetched::Body(bytes)) =
                crate::transport::observe_after_nowait(&direct, None, ALAB_PROBE_BUDGET)
                && let Some(seq) = verify_probed_alab_envelope(&bytes, shared)
            {
                best = Some(best.map_or(seq, |prev| prev.max(seq)));
            }
        }
        return best;
    }
    let Ok(crate::transport::Fetched::Body(bytes)) =
        crate::transport::observe_after_nowait(&raw_url, None, ALAB_PROBE_BUDGET)
    else {
        return None;
    };
    if !ip_literal
        && !here.is_empty()
        && let Ok(addrs) = aterm_uds::lan::resolve(trimmed_dns_host(uri.host()), ALAB_PROBE_BUDGET)
    {
        let all_addrs: Vec<SocketAddr> = addrs
            .into_iter()
            .map(|ip| SocketAddr::new(ip, uri.port()))
            .collect();
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
        // A revoked or unavailable role cannot retain an earlier claim across a later grant.
        alab_watcher.slept();
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
        let mut probe_alab = probe_alab;
        let want_alab = alab_watcher.decide(
            &AlabNameInput {
                own_label: &name,
                own_seq: own_alab_seq,
                holders,
                now: Instant::now(),
            },
            |target| {
                if target.eq_ignore_ascii_case(&name)
                    || aterm_http::Uri::parse(target)
                        .is_some_and(|uri| is_obviously_self_url(&uri, port, Some(&name), here))
                {
                    return None;
                }
                probe_alab(target)
            },
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

/// Probe a host label or holder URL for a verified ALab envelope, as [`probe_alab_endpoint`]
/// does ([`Seams::probe`]).
type ProbeAlab = fn(&str, u16, Option<&str>, &[Target], bool, &Shared) -> Option<u64>;

/// Whether a device other than this Mac, at the addresses given, answers to `<name>.local`
/// now ([`Seams::name_answered`]).
type NameAnswered = dyn Fn(&str, &[Target]) -> bool + Send;

/// Register one advert of a name and port at a target with a TXT ([`Seams::spawn`]).
type SpawnAdvert = dyn Fn(&str, u16, &Target, Option<&VaultTxt>) -> io::Result<Registered> + Send;

/// Hear this Mac's sleep and wake until the bell given is gone, handing each to the handler
/// given; whether it hears them ([`Seams::power`]).
type ListenPower =
    dyn FnOnce(Box<dyn FnMut(Power) + Send>) -> Option<aterm_uds::power::Listening> + Send;

/// End one registration as this Mac goes to sleep ([`Seams::end_for_sleep`]).
type EndAdvert = dyn Fn(&mut Registered) + Send + Sync;

/// Start the standing browse that rings this server's bell, with its own names excluded.
/// A test returns an idle watch and observes its lifetime without using DNS.
type StartBrowse = dyn Fn(&Arc<Bell>, &[&str]) -> Arc<crate::discover::Watch> + Send;

/// How long a will-sleep waits for this server's registrations to end before it lets the Mac
/// sleep anyway ([`SleepYield`]). Ending a registration takes milliseconds; macOS itself waits at
/// most 30 s for every process it told before it sleeps.
const SLEEP_YIELD_BOUND: Duration = Duration::from_secs(2);

/// What the `vault-tend` thread reads and starts ([`Children::tend_seamed`]): this Mac's own,
/// or a test's.
struct Seams {
    /// Waits at most the time it is given for a change of this Mac's addresses or links, and
    /// says whether one came ([`crate::wake::route_changes`]). `None`: none is heard.
    routes: Option<Box<dyn FnMut(Duration) -> bool + Send>>,
    /// Start a standing `_alab-vault._tcp` browse while sharing is on; none starts no browse.
    /// The worker drops it while sharing is off or unreadable, and starts afresh once on.
    browse: Option<Box<StartBrowse>>,
    /// Where this Mac advertises now ([`lan_targets`]).
    targets: Box<dyn Fn() -> Vec<Target> + Send>,
    /// Register one advert of a name and port at a target with a TXT.
    spawn: Box<SpawnAdvert>,
    /// The longest nap between two looks.
    every: Duration,
    /// Hears this Mac's sleep and wake ([`aterm_uds::power::listen`]); a test's arm hears
    /// none, or what the test hands it.
    power: Box<ListenPower>,
    /// Ends a registration as this Mac goes to sleep ([`Registered::end`]).
    end_for_sleep: Arc<EndAdvert>,
    /// How long a will-sleep waits for that ([`SLEEP_YIELD_BOUND`]).
    sleep_bound: Duration,
    /// Probes `alab-secrets.local` and the holders heard for a verified ALab envelope
    /// ([`probe_alab_endpoint`]); a test's arm hears none.
    probe: ProbeAlab,
    /// Whether a device other than this Mac answers to a name ([`name_answered_here`]), asked
    /// when a registration of `alab-secrets` this server holds ended by itself.
    name_answered: Box<NameAnswered>,
    /// Whether `[swarm] enabled` leaves sharing on, read each round
    /// ([`crate::swarm::enabled_live`]): off, or unreadable, the round advertises nothing, ends
    /// what it registered and pulls from or probes no other server
    /// (`docs/DESIGN-alab-swarm-2026-10-27.md` §9).
    sharing: Box<dyn FnMut() -> bool + Send>,
}

impl Seams {
    /// This Mac's own: its route socket, its standing browse, its interfaces, the LAN broker's
    /// registrations and its root power domain.
    fn this_mac() -> Seams {
        Seams {
            routes: crate::wake::route_changes()
                .map(|changes| Box::new(changes) as Box<dyn FnMut(Duration) -> bool + Send>),
            browse: Some(Box::new(|bell, own| {
                Arc::new(crate::discover::Watch::start(bell, own))
            })),
            targets: Box::new(lan_targets),
            spawn: Box::new(|name, port, target, txt| {
                Registered::through_broker(&vault_advert(target, name, port, txt)?)
            }),
            every: ADVERTISE_EVERY,
            power: Box::new(aterm_uds::power::listen),
            end_for_sleep: Arc::new(Registered::end),
            sleep_bound: SLEEP_YIELD_BOUND,
            probe: probe_alab_endpoint,
            name_answered: Box::new(|name, own| name_answered_here(name, own, NAME_CHECK_BUDGET)),
            sharing: Box::new(|| {
                #[cfg(unix)]
                {
                    crate::swarm::enabled_live() == Ok(true)
                }
                // No `[swarm]` switch is read off unix: the advert is macOS-only anyway.
                #[cfg(not(unix))]
                {
                    true
                }
            }),
        }
    }
}

/// How long [`name_answered_here`] waits for the LAN broker to resolve a name: a device that
/// answers to it does within milliseconds, and the check runs only on a round after a
/// registration of `alab-secrets` ended by itself.
const NAME_CHECK_BUDGET: Duration = Duration::from_secs(1);

/// How long [`probe_alab_endpoint`] gives each of its acts: a resolve of the name it probes
/// (through the LAN broker for a local name, where a sleeping holder's mDNS answer can take
/// seconds) and each fetch of the vault there.
const ALAB_PROBE_BUDGET: Duration = Duration::from_secs(3);

/// Whether a device other than this Mac (whose addresses are `own`) answers to `<name>.local`
/// now: the LAN broker resolves the name within `budget` to an address that is not this Mac's. A
/// lookup that fails (no broker included), or has no answer in time, finds none.
fn name_answered_here(name: &str, own: &[Target], budget: Duration) -> bool {
    let Ok(addrs) = aterm_uds::lan::resolve(&format!("{name}.local"), budget) else {
        return false;
    };
    addrs
        .iter()
        .any(|ip| !ip.is_loopback() && !own.iter().any(|t| IpAddr::V4(t.ip) == *ip))
}

/// Whether a registration of `alab-secrets` this server holds ended by itself
/// ([`Registered::ended`]). The `vault-tend` round then asks whether another device answers to
/// the name ([`Seams::name_answered`]) before it takes the children's lock for
/// [`reregister_roles`]: a will-sleep waits on that lock ([`take_for_sleep`]).
fn alab_registration_ended(set: &Mutex<ChildSet>) -> bool {
    let mut set = set.lock().unwrap_or_else(PoisonError::into_inner);
    let slot = &mut set.alab_dns;
    slot.held_name.as_deref() == Some(crate::alab_advertised())
        && slot.held.iter_mut().any(|(_, _, held)| held.ended())
}

/// What the server does as the system announces sleep and wake ([`Seams::power`],
/// `docs/DESIGN-fluid-swarm-2026-10-28.md` §4.2). A Mac about to sleep hands its mDNS records
/// to a Bonjour Sleep Proxy on the LAN (an Apple TV, a HomePod), which then answers for them
/// while the Mac sleeps. A holder's `alab-secrets` record answered that way can keep the name
/// from every other server (each probes it and stays off while it answers, and a registration
/// of the name loses to the proxy's), while nothing serves `:7484`. So a will-sleep ends every registration this server holds, its own
/// name's and `alab-secrets`', before the Mac is let sleep, and a wake rings the advert's bell: its next
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
fn take_for_sleep(set: &Mutex<ChildSet>, asleep: &AtomicBool) -> Vec<(Option<String>, Registered)> {
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
        slot.lost = false;
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
                note("--advertise is macOS only (Bonjour); ignored");
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
        let held_file = advertise
            .as_ref()
            .map(|(_, _, _, shared)| shared.store.join(ALAB_HELD_FILE));
        held.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .held_file
            .clone_from(&held_file);
        let pid = std::process::id();
        let bell = Arc::new(Bell::default());
        if let Some(routes) = seams.routes.take() {
            crate::wake::hear(&bell, routes, Wake::Route);
        }
        // A server that advertises ends its registrations before this Mac sleeps.
        let asleep = Arc::new(AtomicBool::new(false));
        // The listener hears while the tending thread runs: it moves there, and ends with it.
        let power = if advertise.is_some() {
            let listen = std::mem::replace(&mut seams.power, Box::new(|_| None));
            let yielding = SleepYield {
                set: Arc::clone(&held),
                asleep: Arc::clone(&asleep),
                bell: Arc::downgrade(&bell),
                end: Arc::clone(&seams.end_for_sleep),
                bound: seams.sleep_bound,
            };
            let hears = listen(Box::new(move |power| yielding.hear(power)));
            if hears.is_none() && cfg!(target_os = "macos") {
                note(
                    "could not hear this Mac's sleep and wake, so this server's advertisements \
                     stay registered while it sleeps",
                );
            }
            hears
        } else {
            None
        };
        let browse_bell = Arc::clone(&bell);
        let napping = Arc::clone(&bell);
        let shared_for_wake = advertise
            .as_ref()
            .map(|(_, _, _, shared)| Arc::clone(shared));
        let spawned = std::thread::Builder::new()
            .name("vault-tend".to_owned())
            .spawn(move || {
                let _power = power;
                let mut watch = None;
                // Where a server that held `alab-secrets` stopped, this one takes it back.
                let mut alab_watcher = AlabNameWatcher::resume(
                    held_file.as_deref().and_then(take_held_stamp),
                    aterm_time::unix_millis().unwrap_or(0),
                    Instant::now(),
                    aterm_uds::process::last_sleep_ms()
                        .and_then(|(slept, _)| u64::try_from(slept).ok()),
                );
                let probe = seams.probe;
                let mut why = None;
                let mut held_said = false;
                loop {
                    let sharing = (seams.sharing)();
                    if !sharing {
                        // Dropping the watch ends its standing DNS child, not just this round's
                        // resolves. A later on round gets fresh discoveries and wakes.
                        watch = None;
                    } else if watch.is_none()
                        && advertise.is_some()
                        && let Some(start) = seams.browse.as_ref()
                    {
                        // Its own names' Adds come back with each registration it starts.
                        let own: Vec<&str> = advertise
                            .iter()
                            .flat_map(|(_, names, _, _)| names.iter().map(String::as_str))
                            .chain([crate::alab_advertised()])
                            .collect();
                        watch = Some(start(&browse_bell, &own));
                    }
                    if sharing == held_said {
                        held_said = !sharing;
                        note(if sharing {
                            "sharing is on again ([swarm] enabled): advertising this vault"
                        } else {
                            "sharing is off ([swarm] enabled = false in aterm.toml, or it cannot \
                             be read): this server advertises nothing on the local network until \
                             it is on"
                        });
                    }
                    let here = if sharing {
                        (seams.targets)()
                    } else {
                        Vec::new()
                    };
                    let (peers, holders) = match &watch {
                        Some(w) if sharing => (
                            w.peer_vaults(PEER_RESOLVE_BUDGET),
                            w.alab_holders(PEER_RESOLVE_BUDGET),
                        ),
                        _ => (Vec::new(), Vec::new()),
                    };
                    let role_targets = advertise.as_mut().map(|(decide, _, port, shared)| {
                        if !sharing {
                            // Ends both registrations ([`reregister_roles`]) and decides nothing.
                            return (
                                None,
                                Some((crate::alab_advertised().to_owned(), *port, None)),
                            );
                        }
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
                            |target| probe(target, port, None, &here, holding, shared),
                        );
                        if let (Some((name, _, _)), Some(w)) = (&targets.0, &watch) {
                            w.note_own(name);
                        }
                        targets
                    });
                    let alab_answered = (role_targets.is_some() && alab_registration_ended(&held))
                        .then(|| (seams.name_answered)(crate::alab_advertised(), &here));
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
                                alab_answered,
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
/// by itself while still wanted, while another device answers to the name, lost the name to
/// that device ([`AlabNameWatcher::lost_name`]): this round ends it and starts none in its
/// place, where [`reregister_slot`] alone would start one on every round, each losing the same
/// way. One that ended with no other device answering (`alab_answered` is `Some(false)`:
/// mDNSResponder restarting ends every registration) lost nothing, and this round registers the
/// name again. `alab_answered` is what the round heard when it asked
/// ([`alab_registration_ended`]); `None`, not asked, reads as a lost name. The log says each
/// once, and again only after a later hold of the name stays up for a round.
fn reregister_roles(
    set: &mut ChildSet,
    (primary, secondary): RoleTargets,
    alab_watcher: &mut AlabNameWatcher,
    why: Option<Wake>,
    spawn: &SpawnAdvert,
    alab_answered: Option<bool>,
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
        Some(true) if alab_answered == Some(false) => {
            // The ended registrations were reaped as they were looked at; the rest stay, and
            // `reregister_slot` starts the missing ones afresh.
            set.alab_dns.held.retain_mut(|(_, _, held)| !held.ended());
            if !std::mem::replace(&mut set.alab_again_said, true) {
                note(&format!(
                    "the registration of {alab_name}.local ended and no other device answers to \
                     the name, so this server registers it again"
                ));
            }
        }
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
        Some(false) => {
            set.alab_lost_said = false;
            set.alab_again_said = false;
        }
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
    alab_answered: Option<bool>,
) {
    if std::mem::take(&mut set.slept) {
        alab_watcher.slept();
        return;
    }
    if asleep.load(Ordering::SeqCst) {
        alab_watcher.slept();
        return;
    }
    reregister_roles(set, targets, alab_watcher, why, spawn, alab_answered);
}

/// Whether one of `slot`'s registrations that `want` still wants under `name` (its target and
/// TXT, as [`reregister_slot`] keeps one) ended by itself: `None` when it holds none so wanted.
fn wanted_ended(
    slot: &mut AdvertSlot,
    name: &str,
    want: Option<&(Vec<Target>, Option<VaultTxt>)>,
) -> Option<bool> {
    let (targets, txt) = want.filter(|_| slot.held_name.as_deref() == Some(name))?;
    let mut ended = None;
    for (target, held_txt, child) in &mut slot.held {
        if targets.contains(target) && txt == held_txt {
            let gone = child.ended();
            ended = Some(ended.unwrap_or(false) || gone);
        }
    }
    ended
}

/// Keep `slot`'s registrations (each started by `spawn`) at `want`'s targets and
/// TXT: a live registration at a target still wanted with the same TXT stays, and every other
/// one is replaced cleanly. One that ended by itself gave the name up to another device on the
/// network, which new Macs may then reach in place of this vault: the log says so once, and
/// again once every registration stays up. A round that starts one names `why` it ran (`None`:
/// the first, as the server started) in its log line, and keeps it ([`AdvertSlot::started_by`]).
fn reregister_slot(
    slot: &mut AdvertSlot,
    name: &str,
    want: Option<(Vec<Target>, Option<VaultTxt>)>,
    why: Option<Wake>,
    spawn: &dyn Fn(&Target, Option<&VaultTxt>) -> io::Result<Registered>,
) {
    let same_name = slot.held_name.as_deref() == Some(name);
    let mut gave_up = false;
    let mut kept = Vec::new();
    for (target, held_txt, mut held) in std::mem::take(&mut slot.held) {
        let wanted = same_name
            && want
                .as_ref()
                .is_some_and(|(targets, txt)| targets.contains(&target) && *txt == held_txt);
        if wanted {
            if !held.ended() {
                kept.push((target, held_txt, held));
                continue;
            }
            gave_up = true;
        }
        held.end();
    }
    slot.held = kept;
    if !same_name {
        slot.logged.clear();
        slot.lost = false;
    }
    if gave_up && !slot.lost {
        slot.lost = true;
        note(&format!(
            "mDNSResponder gave up advertising {name}.local: another device on this network answers \
             to that name, so new Macs may reach it in place of this vault (--advertise <name> \
             picks another); trying again every {}s",
            ADVERTISE_EVERY.as_secs()
        ));
    }
    let Some((targets, txt)) = want else {
        slot.held_name = None;
        slot.logged.clear();
        slot.lost = false;
        return;
    };
    let missing: Vec<&Target> = targets
        .iter()
        .filter(|target| !slot.held.iter().any(|(held, _, _)| held == *target))
        .collect();
    if missing.is_empty() {
        if !gave_up && std::mem::take(&mut slot.lost) {
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
            Err(e) => note(&format!("could not advertise {name}.local: {e}")),
        }
    }
    slot.held_name = (!slot.held.is_empty()).then(|| name.to_owned());
    slot.started_by = Some(why);
    let ips: Vec<Ipv4Addr> = slot.held.iter().map(|(target, _, _)| target.ip).collect();
    if !slot.lost && !ips.is_empty() && slot.logged != ips {
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
        // A holder that stops (a new program, a move of its job) says so to the server that
        // starts in its place ([`AlabNameWatcher::resume`]).
        if let Some(path) = set.held_file.take()
            && let Some(stamp) =
                held_alab(&mut set.alab_dns, aterm_time::unix_millis().unwrap_or(0))
        {
            leave_held_stamp(&path, stamp);
        }
        if let Some(mut child) = set.caffeinate.take() {
            end(&mut child);
        }
        for slot in [&mut set.dns, &mut set.alab_dns] {
            for (_, _, mut held) in slot.held.drain(..) {
                held.end();
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

/// Host for `SwarmRecordsFile`'s environment markers, so `spec_xref_closure` accounts for
/// every model action: a torn append, a failed write and another writer are what the disk and
/// the Mac do to the server, never what it chooses.
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SwarmRecordsFile",
        action = "Tear",
        reason = "Environment: an append cut short by a crash or a full disk leaves a torn frame"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SwarmRecordsFile",
        action = "Fail",
        reason = "Environment: the disk refuses both the append and the whole write"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SwarmRecordsFile",
        action = "Foreign",
        reason = "Environment: another writer renames a copy of the file's readable part over it"
    )
)]
#[doc(hidden)]
pub fn records_file_environment() {}

/// Host for `SwarmRecordsCapacity`'s environment markers, so `spec_xref_closure` accounts for
/// every model action: its first action picks which of the three it is (a member's log under
/// its budgets, a vault server holding a log at the legacy line, or a Mac holding one), as a
/// test's fixture does.
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SwarmRecordsCapacity",
        action = "Budgets",
        reason = "Environment: a member's log as the genesis, a Node record and an Admit leave it"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SwarmRecordsCapacity",
        action = "Crossing",
        reason = "Environment: a vault server holding a log three records short of the legacy bound"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SwarmRecordsCapacity",
        action = "Mac",
        reason = "Environment: a Mac whose records file holds a log three records short of the legacy bound"
    )
)]
#[doc(hidden)]
pub fn records_capacity_environment() {}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
