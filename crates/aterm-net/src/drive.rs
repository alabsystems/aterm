// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The network drive — both ends of "one aterm drives another over the network",
//! composed from the tested pieces: [`tls`](crate::tls) (channel) +
//! [`verify_capability`]/[`present_capability`] (the channel-bound capability) +
//! [`relay`](crate::tls::relay) (the byte bridge).
//!
//! * **Listener** ([`accept_and_relay`], [`serve`]) — the host being driven.
//!   Accepts TLS with an operator cert, verifies the dialer's channel-bound
//!   capability, then relays the connection to its **local control socket**. The
//!   remote driver thereafter speaks the ordinary control protocol; the TLS
//!   capability is the network-specific gate that replaces the local same-uid
//!   `SO_PEERCRED` check (which has no network analog).
//! * **Driver** ([`dial_and_relay_pinned`]) — the host doing the driving. Dials a pinned
//!   endpoint, presents the channel-bound capability, then relays a local control
//!   client to the remote. The network analog of `proxy::connect_and_relay`.
//!
//! **Secure-default-OFF**: nothing here runs unless a caller explicitly stands up
//! a listener with an operator-provided cert+key and a capability lookup. There is
//! no implicit bind, no default port, no ambient authority.

use std::io;
use std::net::{IpAddr, Ipv6Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use aterm_session::EdgeToken;
use aterm_uds::CtlStream;
use rustls::{ClientConfig, ServerConfig};

use crate::tls::{self, TlsTransport};
use crate::{Granted, RemoteEndpoint, present_capability, verify_capability};

/// What happened on the listener — surfaced to the caller's logger so the network
/// listener has an auditable trail (mirrors the local socket's `log_denial`).
/// Every connection event names the peer's address. Never carries a secret.
#[derive(Clone, Debug)]
pub enum NetEvent {
    /// A dialer's capability verified: its relay to the local control socket
    /// starts now.
    Admitted {
        /// The dialer's address.
        peer: SocketAddr,
        /// What its capability authorizes.
        granted: Granted,
    },
    /// An admitted dialer's relay ended: `error` is `None` for an ordinary close.
    Closed {
        /// The dialer's address.
        peer: SocketAddr,
        /// Why the relay failed, when it did.
        error: Option<String>,
    },
    /// A connection was turned away before any relay. `reason` is a
    /// non-sensitive clause (`wrong drive token`, `no handshake within 10s`, …).
    Refused {
        /// The dialer's address.
        peer: SocketAddr,
        /// Why it was not admitted.
        reason: String,
    },
    /// The listener stopped serving: nothing more is accepted.
    Stopped(String),
}

/// The deadline shape a socket read or write takes when its timer runs out.
fn is_deadline(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// Total wall-clock deadline on the UNAUTHENTICATED phase of an accepted
/// connection — the TLS handshake plus the `AUTH` line read. Enforced two ways
/// (see [`accept_and_relay_inner`]): a per-syscall `SO_RCVTIMEO` floor bounds a
/// fully-idle read, and a watchdog force-closes the socket at this total deadline
/// so a peer that DRIBBLES bytes (which would keep resetting the per-syscall
/// timer) is still cut off. Neither pins a thread past this bound; the deadline is
/// lifted once the capability verifies, before the long-lived relay.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum connections [`serve`] keeps in flight at once (handshaking + relaying):
/// the bound on its threads and file descriptors, each connection holding a fixed
/// handful of each. A connection beyond it is refused unless it may take the slot
/// of one that has not authenticated ([`serve`]'s rule); an admitted relay's slot
/// is never taken.
const MAX_INFLIGHT: usize = 64;

/// How long a connection that has not authenticated keeps its slot against a
/// newcomer from an address holding no more unauthenticated connections than
/// its own ([`serve`]'s rule 2). A newcomer from a heavier address never takes
/// it, at any age. The TLS 1.3 handshake plus the `AUTH` line is two round
/// trips, which fit in it over most paths, though not every one: a lost
/// segment costs an initial retransmission timeout of a second, and a
/// geostationary round trip is about 600 ms. Far below [`HANDSHAKE_TIMEOUT`],
/// which idle sockets were renewed against.
const PREAUTH_GRACE: Duration = Duration::from_secs(2);

/// How often [`serve`]'s accept loop wakes to re-check its `running` flag, so the
/// kill-switch is observed promptly instead of only when the next peer connects.
const ACCEPT_POLL: Duration = Duration::from_millis(200);

#[cfg(any(unix, windows))]
fn poll_timeout_millis(timeout: Duration) -> i32 {
    // poll/WSAPoll take whole milliseconds. Round UP so a sub-millisecond
    // remainder cannot turn a positive shutdown budget into a busy zero-timeout
    // poll; saturate because -1 has the special meaning "wait forever".
    let millis = timeout.as_millis().saturating_add(u128::from(
        !timeout.subsec_nanos().is_multiple_of(1_000_000),
    ));
    i32::try_from(millis).unwrap_or(i32::MAX)
}

/// Wait until a non-blocking listener may accept, or until `timeout` expires.
/// `Ok(true)` means the OS reported an event (including an error/hangup, which
/// the following `accept` surfaces); `Ok(false)` is an idle timeout.
#[cfg(unix)]
fn wait_listener_readable(listener: &TcpListener, timeout: Duration) -> io::Result<bool> {
    use std::os::fd::AsRawFd as _;

    let started = Instant::now();
    let mut remaining = timeout;
    loop {
        let mut descriptor = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `descriptor` is one initialized pollfd valid for the call;
        // `listener` keeps its borrowed descriptor open for the whole wait.
        let result = unsafe {
            libc::poll(
                std::ptr::from_mut(&mut descriptor),
                1,
                poll_timeout_millis(remaining),
            )
        };
        if result >= 0 {
            return Ok(result > 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
        // Signals cannot restart the full shutdown interval: recompute from the
        // original wait start, then give the caller its running-flag check.
        remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Ok(false);
        }
    }
}

#[cfg(windows)]
fn wait_listener_readable(listener: &TcpListener, timeout: Duration) -> io::Result<bool> {
    use std::os::windows::io::AsRawSocket as _;
    use windows_sys::Win32::Networking::WinSock::{
        POLLIN, SOCKET_ERROR, WSAGetLastError, WSAPOLLFD, WSAPoll,
    };

    let mut descriptor = WSAPOLLFD {
        // std's RawSocket is the pointer-width unsigned integer WinSock names
        // SOCKET; windows-sys spells that same ABI type as `usize`.
        fd: listener.as_raw_socket() as usize,
        events: POLLIN,
        revents: 0,
    };
    // SAFETY: std initialized Winsock before it created `listener`;
    // `descriptor` names that live borrowed socket for the duration of the call.
    let result = unsafe {
        WSAPoll(
            std::ptr::from_mut(&mut descriptor),
            1,
            poll_timeout_millis(timeout),
        )
    };
    if result == SOCKET_ERROR {
        // `last_os_error` reads GetLastError, while Winsock APIs require their
        // own thread-local error slot.
        // SAFETY: WSAGetLastError takes no arguments and only reads that slot.
        return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
    }
    Ok(result > 0)
}

#[cfg(not(any(unix, windows)))]
fn wait_listener_readable(_listener: &TcpListener, timeout: Duration) -> io::Result<bool> {
    // Native shipping targets use poll/WSAPoll above. Retain a compiling,
    // bounded fallback for other std targets rather than claiming readiness.
    std::thread::sleep(timeout);
    Ok(false)
}

/// The address a connection is counted against ([`serve`]'s rule 1): its IPv4
/// address, or its IPv6 /64 — the block one site is given, inside which a single
/// host mints addresses at will. An IPv4-mapped peer (a dual-stack listener)
/// counts as its IPv4 address.
fn source_of(peer: IpAddr) -> IpAddr {
    match peer.to_canonical() {
        IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & !u128::from(u64::MAX))),
        v4 @ IpAddr::V4(_) => v4,
    }
}

/// A held slot as [`pick_displaced`] weighs it.
#[derive(Clone, Copy, Debug)]
struct Held {
    /// The address it is counted against ([`source_of`]).
    source: IpAddr,
    /// When its connection was given the slot.
    since: Instant,
    /// Its connection has neither authenticated nor been cut off.
    unauthenticated: bool,
}

/// Which held slot a newcomer from `source` may take at `now`, every slot being
/// held: `None` refuses the newcomer. Only an unauthenticated connection's
/// slot is ever named — rule 1, then rule 2, of [`serve`]'s docs — and only
/// one from an address holding at least as many unauthenticated connections
/// as the newcomer's.
fn pick_displaced(held: &[Held], source: IpAddr, now: Instant) -> Option<usize> {
    let pending = || held.iter().enumerate().filter(|(_, h)| h.unauthenticated);
    let count = |of: IpAddr| pending().filter(|(_, h)| h.source == of).count();
    let oldest = |eligible: &dyn Fn(&Held) -> bool| {
        pending()
            .filter(|(_, h)| eligible(h))
            .min_by_key(|(_, h)| h.since)
            .map(|(i, _)| i)
    };
    let mine = count(source);
    // 1. An address crowding the newcomer's out (two or more ahead of it)
    // gives up its oldest. Said as a difference, which cannot overflow.
    let most = pending().map(|(_, h)| count(h.source)).max()?;
    if most.saturating_sub(mine) >= 2 {
        return oldest(&|h| count(h.source) == most);
    }
    // 2. Otherwise whichever has gone PREAUTH_GRACE without authenticating,
    // from an address no lighter than the newcomer's: a heavier address's
    // reconnects never cut a lighter one's slow handshake.
    oldest(&|h| count(h.source) >= mine && now.saturating_duration_since(h.since) >= PREAUTH_GRACE)
}

/// One connection [`serve`] holds a slot for, handshaking or relaying.
struct Slot {
    /// Which connection: its [`SlotTenure`] frees the slot by this.
    id: u64,
    /// The address it is counted against ([`source_of`]).
    source: IpAddr,
    /// When it was given the slot.
    since: Instant,
    /// Its unauthenticated phase, which a newcomer may end ([`pick_displaced`]).
    phase: Evictor,
}

/// [`serve`]'s slots, one per connection in flight: the bound on its threads
/// and descriptors. Only the accept loop takes a slot or displaces a
/// connection; each connection's handler thread frees its own as it ends.
#[derive(Default)]
struct Slots {
    held: Mutex<Vec<Slot>>,
    /// Signalled whenever a slot is freed.
    freed: Condvar,
}

impl Slots {
    fn lock(&self) -> MutexGuard<'_, Vec<Slot>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether a newcomer from `source` gets a slot: a free one, or one
    /// [`pick_displaced`] lets it take. A displaced connection is cut off at
    /// once, and its threads end before the newcomer's start, so making room
    /// never exceeds the bound; one that has not let go within [`ACCEPT_POLL`]
    /// leaves the newcomer refused rather than the loop stalled.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "NetSlotDisplacement",
            action = "Refuse",
            project = "drive::tests::serve_slots_conform_to_net_slot_displacement"
        )
    )]
    fn make_room(&self, source: IpAddr) -> bool {
        let held = self.lock();
        if held.len() < MAX_INFLIGHT {
            return true;
        }
        let now = Instant::now();
        loop {
            let view: Vec<Held> = held
                .iter()
                .map(|slot| Held {
                    source: slot.source,
                    since: slot.since,
                    unauthenticated: slot.phase.unauthenticated(),
                })
                .collect();
            let Some(displaced) = pick_displaced(&view, source, now) else {
                return false;
            };
            if held[displaced].phase.evict() {
                break;
            }
            // It authenticated, or met its deadline, since the scan: it is no
            // longer unauthenticated, so the next scan passes it over.
        }
        let (held, _) = self
            .freed
            .wait_timeout_while(held, ACCEPT_POLL, |held| held.len() >= MAX_INFLIGHT)
            .unwrap_or_else(PoisonError::into_inner);
        held.len() < MAX_INFLIGHT
    }

    /// Give connection `id`, armed with `phase`, a slot, freed when the
    /// returned tenure drops.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "NetSlotDisplacement",
            action = "Accept",
            project = "drive::tests::serve_slots_conform_to_net_slot_displacement"
        )
    )]
    fn take(self: &Arc<Self>, id: u64, source: IpAddr, phase: Evictor) -> SlotTenure {
        self.lock().push(Slot {
            id,
            source,
            since: Instant::now(),
            phase,
        });
        SlotTenure {
            slots: Arc::clone(self),
            id,
        }
    }

    /// Free connection `id`'s slot as its handler thread ends: a cut
    /// connection's (`Release`) or a relay's (`Close`).
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "NetSlotDisplacement",
            action = "Release",
            project = "drive::tests::serve_slots_conform_to_net_slot_displacement"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "NetSlotDisplacement",
            action = "Close",
            project = "drive::tests::serve_slots_conform_to_net_slot_displacement"
        )
    )]
    fn free(&self, id: u64) {
        self.lock().retain(|slot| slot.id != id);
        self.freed.notify_all();
    }
}

/// Frees its connection's slot when the connection's handler thread ends
/// (including on panic), so a refused, displaced or finished connection always
/// frees its slot.
struct SlotTenure {
    slots: Arc<Slots>,
    id: u64,
}

impl Drop for SlotTenure {
    fn drop(&mut self) {
        self.slots.free(self.id);
    }
}

// Unauthenticated-phase watchdog state. A per-syscall `SO_RCVTIMEO` only bounds an
// IDLE stall — a peer that dribbles one byte just under the timeout keeps resetting
// it. So a watchdog thread enforces a TOTAL WALL-CLOCK deadline: after it elapses it
// force-closes the socket unless the handshake+AUTH already finished. The state
// leaves `WD_RUNNING` exactly once (CAS) — to `WD_AUTHED` by the handshake, to
// `WD_FIRED` by the deadline, or to `WD_EVICTED` by [`serve`] giving the slot to a
// newer connection — so neither the watchdog nor the listener ever shuts a socket
// the relay is about to use. This claim, the slots [`serve`] holds and the
// newcomer that waits for one are the ty model
// `aterm_spec::derive::net_slot_displacement_model` (`NetSlotDisplacement`,
// 2026-10-10), whose actions the `#[refines]` anchors here name.
const WD_RUNNING: u8 = 0;
const WD_AUTHED: u8 = 1;
const WD_FIRED: u8 = 2;
const WD_EVICTED: u8 = 3;

/// How a connection's unauthenticated phase ended: the claim that won.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Claim {
    /// The handshake finished first: the connection is the caller's.
    Authed,
    /// The wall-clock deadline elapsed first; the socket was force-closed.
    Deadline,
    /// [`serve`] gave the connection's slot to a newer one; the socket was
    /// force-closed.
    Evicted,
}

/// Why a connection [`serve`] displaced was not admitted (its `Refused` reason).
const DISPLACED: &str = "a newer connection took its slot before it authenticated";

/// The listener's hold on one connection's unauthenticated phase: it can end
/// that phase early, through the same exactly-once claim the deadline makes.
#[derive(Clone)]
struct Evictor {
    /// The watchdog's state.
    state: Arc<AtomicU8>,
    /// The parked watchdog thread, woken to force-close.
    watchdog: std::thread::Thread,
}

impl Evictor {
    /// Whether the connection has neither authenticated nor been cut off yet.
    fn unauthenticated(&self) -> bool {
        self.state.load(Ordering::Acquire) == WD_RUNNING
    }

    /// Claim [`WD_EVICTED`] iff the phase is still running, and wake the
    /// watchdog to force-close the socket. `false`: the handshake (or the
    /// deadline) claimed it first, and the connection is left alone.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "NetSlotDisplacement",
            action = "Evict",
            project = "drive::tests::serve_slots_conform_to_net_slot_displacement"
        )
    )]
    fn evict(&self) -> bool {
        let won = self
            .state
            .compare_exchange(WD_RUNNING, WD_EVICTED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if won {
            self.watchdog.unpark();
        }
        won
    }
}

/// The watchdog's claim at its deadline: [`WD_FIRED`] iff the phase is still
/// running. `false`: the handshake or an eviction claimed it first.
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "NetSlotDisplacement",
        action = "Deadline",
        project = "drive::tests::serve_slots_conform_to_net_slot_displacement"
    )
)]
fn claim_deadline(state: &AtomicU8) -> bool {
    state
        .compare_exchange(WD_RUNNING, WD_FIRED, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

/// The unauthenticated-phase watchdog: one thread holding a CLONE of the socket,
/// armed for a total wall-clock `handshake_timeout`, which force-closes that clone
/// if the deadline elapses before the handshaking thread claims [`WD_AUTHED`].
/// Both ends of the drive ([`accept_and_relay_inner`], [`dial_and_relay_pinned_inner`])
/// arm one, and the dialer arms one more over each exchange it makes before the
/// relay ([`within_wall_clock`]); keeping it in a single type is what stops the
/// copies drifting.
///
/// **Event-driven, not polled.** The thread parks for the WHOLE remaining deadline
/// in one go and [`finish`](Self::finish) `unpark`s it the instant the handshake
/// claims `WD_AUTHED`, so a connection that authenticates early (the normal case)
/// never waits out a poll interval before the join returns: a 50 ms poll step was
/// ~99% of loopback connection setup.
///
/// **The deadline is not weakened by the wake.** Every wake — `unpark`, or one of
/// `park_timeout`'s permitted spurious wakeups — re-reads the tri-state and
/// recomputes the remaining time from the arming instant, so the force-close still
/// happens at exactly `handshake_timeout` after arming and no wake can shorten,
/// lengthen or starve it. Exactly one of {authed, fired, evicted} wins the CAS,
/// so the watchdog never shuts a socket the relay is about to use.
struct HandshakeWatchdog {
    /// The state claimed by exactly one of the handshake, the watchdog and an
    /// [`Evictor`].
    state: Arc<AtomicU8>,
    /// The parked watchdog thread — its `Thread` handle is the wake channel.
    thread: std::thread::JoinHandle<()>,
    /// The deadline it was armed with, for the words of a timeout.
    timeout: Duration,
    /// Set by the thread each time it is about to wait — tests only, so a test
    /// can time `finish` against a thread that is really inside its wait.
    #[cfg(test)]
    waiting: Arc<std::sync::atomic::AtomicBool>,
}

impl HandshakeWatchdog {
    /// Arm the deadline on a clone of `tcp`. The clone MUST be taken here, before
    /// the caller moves `tcp` into the TLS handshake.
    ///
    /// # Errors
    /// If the socket cannot be cloned or the watchdog thread cannot start (the
    /// caller then never starts the handshake).
    fn arm(tcp: &TcpStream, handshake_timeout: Duration) -> io::Result<Self> {
        let state = Arc::new(AtomicU8::new(WD_RUNNING));
        let wd_sock = tcp.try_clone()?;
        let wd_state = Arc::clone(&state);
        #[cfg(test)]
        let waiting = Arc::new(std::sync::atomic::AtomicBool::new(false));
        #[cfg(test)]
        let wd_waiting = Arc::clone(&waiting);
        // Fallible spawn: [`serve`] arms on its accept loop, which a refused
        // thread must not take down.
        let thread = std::thread::Builder::new().spawn(move || {
            let start = Instant::now();
            let cut_off = loop {
                match wd_state.load(Ordering::Acquire) {
                    WD_RUNNING => {}
                    // The listener gave the slot away: force-close now.
                    WD_EVICTED => break true,
                    _ => break false, // finished early — nothing to cut off
                }
                // Remaining time measured from ARMING, so a spurious wake or an
                // `unpark` that raced a state store cannot move the deadline.
                let remaining = handshake_timeout.saturating_sub(start.elapsed());
                if remaining.is_zero() {
                    // Deadline reached: claim FIRED iff still running. A lost
                    // claim is re-read above: the handshake's leaves the socket
                    // alone, an eviction's still closes it.
                    if claim_deadline(&wd_state) {
                        break true;
                    }
                    continue;
                }
                #[cfg(test)]
                wd_waiting.store(true, Ordering::Release);
                std::thread::park_timeout(remaining);
            };
            // Force-close so the blocked handshake/AUTH read errors out.
            if cut_off {
                let _ = wd_sock.shutdown(std::net::Shutdown::Both);
            }
        })?;
        Ok(Self {
            state,
            thread,
            timeout: handshake_timeout,
            #[cfg(test)]
            waiting,
        })
    }

    /// The listener's handle for ending this phase early ([`Evictor::evict`]).
    fn evictor(&self) -> Evictor {
        Evictor {
            state: Arc::clone(&self.state),
            watchdog: self.thread.thread().clone(),
        }
    }

    /// Claim [`WD_AUTHED`] iff no deadline or eviction claimed the phase first,
    /// wake the watchdog, and JOIN it — so its socket clone is dropped before the
    /// caller touches the connection again. Returns the claim that won: anything
    /// but [`Claim::Authed`] means the socket was force-closed and the caller
    /// must tear the connection down.
    ///
    /// The state is published by the `AcqRel` compare-exchange BEFORE the wake, so
    /// the watchdog's re-check on waking always observes it; and `unpark` issued
    /// before the thread parks leaves a token that makes its next `park_timeout`
    /// return at once, so the wake can never be lost to that race.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "NetSlotDisplacement",
            action = "Authenticate",
            project = "drive::tests::serve_slots_conform_to_net_slot_displacement"
        )
    )]
    fn finish(self) -> Claim {
        let claim = match self.state.compare_exchange(
            WD_RUNNING,
            WD_AUTHED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Claim::Authed,
            Err(WD_EVICTED) => Claim::Evicted,
            Err(_) => Claim::Deadline,
        };
        self.thread.thread().unpark();
        let _ = self.thread.join();
        claim
    }
}

/// Arm both deadlines on an accepted connection's unauthenticated phase: the
/// per-syscall floor, which bounds a single fully-idle read or write (defense in
/// depth), and the wall-clock [`HandshakeWatchdog`], which a peer that dribbles
/// bytes cannot reset.
fn arm_unauthenticated(
    tcp: &TcpStream,
    handshake_timeout: Duration,
) -> io::Result<HandshakeWatchdog> {
    tcp.set_read_timeout(Some(handshake_timeout))?;
    tcp.set_write_timeout(Some(handshake_timeout))?;
    HandshakeWatchdog::arm(tcp, handshake_timeout)
}

/// Listener side, ONE connection: complete the TLS handshake on `tcp` with the
/// operator `config`, verify the dialer's channel-bound capability via `lookup`,
/// and on success relay the connection to the local control socket from
/// `connect_local`. The capability is checked BEFORE the local socket is dialed,
/// so an unauthorized peer never reaches it.
///
/// `lookup(src, op)` returns the [`EdgeToken`] the host minted for that driver+op
/// (`None` ⇒ no such grant ⇒ denied). `connect_local` dials the host's own
/// control socket (e.g. `CtlStream::connect(sock_path)`).
///
/// # Errors
/// On a TLS/handshake failure, a denied capability, or a local-socket dial error
/// — in every case before (or without) any relay.
pub fn accept_and_relay<F, G>(
    tcp: TcpStream,
    config: Arc<ServerConfig>,
    lookup: F,
    connect_local: G,
) -> io::Result<Granted>
where
    F: FnOnce(&str, &str) -> Option<EdgeToken>,
    G: FnOnce() -> io::Result<CtlStream>,
{
    accept_and_relay_inner(
        tcp,
        config,
        lookup,
        connect_local,
        HANDSHAKE_TIMEOUT,
        |_| {},
    )
}

/// [`accept_and_relay`] with the unauthenticated-phase deadline injected (tests
/// use a short value to exercise the slow-loris timeout without a real wait),
/// and `on_admit` called the moment the capability verifies — before the local
/// socket is dialed — so [`serve`] can log a connection when it starts, not
/// only when it ends.
fn accept_and_relay_inner<F, G>(
    tcp: TcpStream,
    config: Arc<ServerConfig>,
    lookup: F,
    connect_local: G,
    handshake_timeout: Duration,
    on_admit: impl FnOnce(&Granted),
) -> io::Result<Granted>
where
    F: FnOnce(&str, &str) -> Option<EdgeToken>,
    G: FnOnce() -> io::Result<CtlStream>,
{
    let watchdog = arm_unauthenticated(&tcp, handshake_timeout)?;
    accept_and_relay_armed(tcp, watchdog, config, lookup, connect_local, on_admit)
}

/// [`accept_and_relay_inner`] on a connection whose deadlines are already
/// armed ([`arm_unauthenticated`]) — by [`serve`]'s accept loop, which keeps
/// the watchdog's [`Evictor`] to give the slot away if the connection has not
/// authenticated when a newer one needs it.
fn accept_and_relay_armed<F, G>(
    tcp: TcpStream,
    watchdog: HandshakeWatchdog,
    config: Arc<ServerConfig>,
    lookup: F,
    connect_local: G,
    on_admit: impl FnOnce(&Granted),
) -> io::Result<Granted>
where
    F: FnOnce(&str, &str) -> Option<EdgeToken>,
    G: FnOnce() -> io::Result<CtlStream>,
{
    let handshake_timeout = watchdog.timeout;
    // The unauthenticated phase. Any error here (incl. the watchdog's force-close)
    // tears the connection down.
    let unauth = (|| -> io::Result<(TlsTransport<rustls::ServerConnection>, Granted)> {
        let mut transport = tls::accept(tcp, config)?;
        let exporter = transport.exporter().to_vec();
        let granted = verify_capability(transport.stream(), &exporter, lookup)?;
        Ok((transport, granted))
    })();

    // A deadline — the watchdog's force-close (which the blocked read sees as an
    // end of stream) or the per-syscall timer — is said as one, never as the
    // `unexpected end of file` it surfaces as; so is an eviction.
    let (mut transport, granted) = match (watchdog.finish(), unauth) {
        (Claim::Authed, Ok(v)) => v,
        (Claim::Authed, Err(e)) if !is_deadline(&e) => return Err(e),
        (Claim::Evicted, _) => {
            return Err(io::Error::new(io::ErrorKind::ConnectionAborted, DISPLACED));
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("no handshake within {handshake_timeout:?}"),
            ));
        }
    };

    // Authenticated: drop the handshake deadline before the (long-lived) relay,
    // which installs its own non-blocking poll on the same socket.
    {
        let sock = transport.stream().get_mut();
        sock.set_read_timeout(None)?;
        sock.set_write_timeout(None)?;
    }
    on_admit(&granted);
    // NOW (and only now) dial the local control socket and bridge the two. Its
    // failure is this instance's, and the words say so: bare, it reads as the
    // peer's (`<peer> disconnected: No such file or directory`).
    let local = connect_local().map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("could not reach this aterm's control socket: {e}"),
        )
    })?;
    tls::relay(transport, local)?;
    Ok(granted)
}

/// Serve a bound `TcpListener` until `running` is cleared: one thread per
/// connection, each running [`accept_and_relay`]. A per-connection failure is
/// reported via `on_event` (audit) and never stops the loop — a hostile peer
/// cannot take the listener down by failing a handshake.
///
/// **DoS bounds.** At most [`MAX_INFLIGHT`] connections are in flight at once,
/// handshaking or relaying, which bounds the listener's threads and descriptors.
/// Each connection's unauthenticated phase ends within [`HANDSHAKE_TIMEOUT`]
/// ([`accept_and_relay`]'s two deadlines), so a slow-loris holds one handler
/// thread for no longer than that. By the rule below its slot can go to a
/// newcomer sooner: at once to one from an address lighter by two, when the
/// slow-loris's address holds the most unauthenticated connections; after
/// [`PREAUTH_GRACE`] to one from an address no heavier than its own.
///
/// **Who gets a slot when every slot is held** (ruling 2026-10-10, r13 NET-1;
/// rule 2's address bound added 2026-10-10 on review). Refusing the newest
/// connection let one peer's idle sockets — no TLS byte, each renewed as its
/// deadline cut it — hold every slot and turn away every token holder for as
/// long as it kept them coming. Now a newcomer takes the slot of a connection
/// that has NOT authenticated, if one of these allows it, and is refused
/// otherwise:
///
/// 1. the oldest unauthenticated connection from the address holding the most
///    of them, when that address holds at least two more than the newcomer's
///    (an IPv4 address or an IPv6 /64, [`source_of`]). However fast one
///    address reconnects, a dialer from another is let in at once; the margin
///    of two keeps the address that lost a slot from taking it straight back.
/// 2. otherwise the oldest that has gone [`PREAUTH_GRACE`] without
///    authenticating, from an address holding at least as many unauthenticated
///    connections as the newcomer's. Idle sockets yield to the next dialer from
///    their own address after `PREAUTH_GRACE`, where they used to be renewed
///    forever.
///
/// Both rules give a connection's slot only to a newcomer from an address
/// holding no more unauthenticated connections than the connection's own. So
/// one address never crowds out another, however fast it reconnects, and a
/// lighter address's handshake is never cut for a heavier address's newcomer,
/// however long it takes inside [`HANDSHAKE_TIMEOUT`]. Without that bound on
/// rule 2, a 127.0.0.1 peer's own reconnects cut every handshake from ::1 that
/// took longer than `PREAUTH_GRACE` (0 of 4 slow dials admitted, measured).
///
/// An admitted relay's slot is never taken — the listener's claim on a
/// connection ([`Evictor`]) succeeds only before the handshake's — so relays
/// still fill the listener where they did: they are token holders the operator
/// trusts. The displaced connection's threads end before the newcomer's start,
/// so making room never exceeds the bound. Those three laws — the bound, the
/// exactly-once claim, a relay never cut — are the ty-checked
/// `NetSlotDisplacement` model's invariants, and
/// `tests::serve_slots_conform_to_net_slot_displacement` holds these slots to
/// it.
///
/// What no rule that sees only addresses can do: tell a dialer from a peer that
/// shares its address (a NAT, the same host, the same /64) before the
/// handshake. Such a peer that re-takes each slot the instant it turns
/// `PREAUTH_GRACE` old can still out-race the dialer, and so can one with more
/// addresses than there are slots, each holding as few unauthenticated
/// connections as the dialer's (rule 2 then also cuts a dialer's handshake
/// once it is `PREAUTH_GRACE` old). Rejected: a separate, smaller pre-auth pool
/// (one peer fills it the same way) and a hard per-address cap (it refuses a
/// dialer behind the peer's address outright, where rule 2 lets it in).
///
/// **Shutdown.** The listener is set non-blocking and its readiness wait is
/// bounded by [`ACCEPT_POLL`], so clearing `running` stops the loop promptly (a
/// plain blocking `accept` would only notice on the next connection). A new
/// connection wakes the wait immediately; setup is never paced by that bound.
///
/// `lookup` and `connect_local` are cloned per connection, so wrap shared state in
/// `Arc` on the caller side.
pub fn serve<F, G, E>(
    listener: &TcpListener,
    config: &Arc<ServerConfig>,
    lookup: F,
    connect_local: G,
    running: &Arc<AtomicBool>,
    on_event: E,
) where
    F: Fn(&str, &str) -> Option<EdgeToken> + Send + Sync + Clone + 'static,
    G: Fn() -> io::Result<CtlStream> + Send + Sync + Clone + 'static,
    E: Fn(NetEvent) + Send + Sync + Clone + 'static,
{
    // Non-blocking accept plus bounded readiness gives both immediate connection
    // wakeups and a running-flag shutdown bound. If mode setup fails, a blocking
    // accept would violate the latter, so fail closed rather than entering it.
    if let Err(error) = listener.set_nonblocking(true) {
        on_event(NetEvent::Stopped(format!(
            "could not make the listener non-blocking: {error}"
        )));
        return;
    }
    let slots = Arc::new(Slots::default());
    let mut next_id: u64 = 0;

    while running.load(Ordering::Relaxed) {
        let (tcp, peer) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                // The common idle path blocks in the kernel with no polling CPU,
                // but wakes at once when a connection reaches the accept queue.
                // A readiness error gets the same persistent-error backoff as an
                // accept error below; it must not become a new busy-spin path.
                if wait_listener_readable(listener, ACCEPT_POLL).is_err()
                    && running.load(Ordering::Relaxed)
                {
                    std::thread::sleep(ACCEPT_POLL);
                }
                continue;
            }
            // A single accept error is not fatal — but back off, so a persistent
            // error that is NOT WouldBlock (e.g. EMFILE/ENFILE under fd pressure)
            // cannot busy-spin this loop at 100% CPU until an fd frees.
            Err(_) => {
                std::thread::sleep(ACCEPT_POLL);
                continue;
            }
        };
        // An accepted socket does NOT inherit the listener's non-blocking flag on
        // POSIX, but make it explicit: `accept_and_relay` needs blocking mode for
        // its SO_RCVTIMEO handshake deadline to take effect.
        let _ = tcp.set_nonblocking(false);

        // Concurrency cap: a free slot, or one an unauthenticated connection
        // gives up (the rule above); otherwise refuse rather than spawn
        // unbounded work.
        let source = source_of(peer.ip());
        if !slots.make_room(source) {
            on_event(NetEvent::Refused {
                peer,
                reason: "the listener is at capacity".to_owned(),
            });
            continue; // dropping `tcp` closes the connection
        }
        // Armed here, not on the handler thread, so the slot holds the
        // connection's evictor from the moment it is taken.
        let watchdog = match arm_unauthenticated(&tcp, HANDSHAKE_TIMEOUT) {
            Ok(watchdog) => watchdog,
            Err(e) => {
                on_event(NetEvent::Refused {
                    peer,
                    reason: e.to_string(),
                });
                continue;
            }
        };
        next_id = next_id.wrapping_add(1);
        let slot = slots.take(next_id, source, watchdog.evictor());

        let config = Arc::clone(config);
        let lookup = lookup.clone();
        let connect_local = connect_local.clone();
        let on_event = on_event.clone();
        std::thread::spawn(move || {
            let _slot = slot; // released (slot freed) when this thread ends
            let admitted = std::cell::Cell::new(false);
            let result =
                accept_and_relay_armed(tcp, watchdog, config, lookup, connect_local, |granted| {
                    admitted.set(true);
                    on_event(NetEvent::Admitted {
                        peer,
                        granted: granted.clone(),
                    });
                });
            let ev = match result {
                Ok(_) => NetEvent::Closed { peer, error: None },
                Err(e) if admitted.get() => NetEvent::Closed {
                    peer,
                    error: Some(e.to_string()),
                },
                Err(e) => NetEvent::Refused {
                    peer,
                    reason: e.to_string(),
                },
            };
            on_event(ev);
        });
    }
}

/// Driver side: dial the pinned remote `addr` over TLS (the cert is pinned by
/// `config` — see [`tls::client_config`]), present the channel-bound capability
/// for `(src, op)` keyed by `token`, then relay the local control client `local`
/// to the remote control server. The network analog of `proxy::connect_and_relay`:
/// past a successful present, bytes flow both ways until either side closes.
///
/// The TLS SNI is [`tls::fixed_server_name`] — identity is by cert fingerprint,
/// not name. This stays protocol-agnostic: it presents the capability, forwards
/// `prebuffer` (any bytes a pipelined client already sent past its request line —
/// NOT a secret), then relays raw bytes. The LISTENER authenticates its own local
/// control socket (so the raw token never crosses the wire — only the
/// channel-bound HMAC does).
///
/// **Identity pinning.** The server CERTIFICATE fingerprint is enforced by
/// `config` (from [`tls::client_config`]): the handshake fails unless the peer
/// presents the pinned cert AND proves key possession, so a redirected/MITM
/// endpoint fails before any secret crosses the wire. The session-NONCE half of
/// the rebind guard ([`RemoteEndpoint::matches`](crate::RemoteEndpoint::matches))
/// is enforced when `pin` is `Some`: after the capability is presented and BEFORE
/// any control bytes relay, the remote's live launch nonce is read
/// ([`observe_launch_nonce`]: one `sessions bridge` over the relay, which the
/// listener has already authenticated at Owner scope) and `matches` is required to
/// hold. `pin == None` presents and relays with no nonce read.
///
/// **Fail-closed.** A pinned session the remote does not report — relaunched,
/// closed, or a roster that could not be read — relays nothing rather than relay
/// unverified: a pin the operator asked for is never silently skipped.
///
/// # Errors
/// On a connect/TLS/handshake failure (incl. cert-pin mismatch) or a denied
/// capability (before any relay); when `pin` is `Some`, a nonce-mismatch or
/// unreported-session error (also before any relay), so a relaunched/rebound
/// session is never driven by a stale pin. A relay-stage I/O error after a
/// successful present is returned too, but by then the capability HAS been
/// accepted and bytes may have flowed.
#[allow(
    clippy::too_many_arguments,
    reason = "the full dial contract (endpoint/TLS/audit-identity/token/prebuffer/stream/pin); bundling into a struct only relocates the argument list"
)]
pub fn dial_and_relay_pinned<A: ToSocketAddrs>(
    addr: A,
    config: Arc<ClientConfig>,
    src: &str,
    op: &str,
    token: &EdgeToken,
    prebuffer: &[u8],
    local: CtlStream,
    pin: Option<RemoteEndpoint>,
) -> io::Result<()> {
    dial_and_relay_pinned_inner(
        addr,
        config,
        src,
        op,
        token,
        prebuffer,
        local,
        HANDSHAKE_TIMEOUT,
        pin.clone(),
        None,
        move |transport| match &pin {
            Some(pin) => observe_launch_nonce(pin, transport),
            None => Ok(None),
        },
    )
}

/// A dialer's CONNECT PROBE: one request the remote's control server answers at
/// once, sent after the capability is accepted and before any of the caller's
/// bytes, whose one-line reply must arrive within `within`. The reply is
/// consumed, never relayed. It proves the remote is SERVING the connection —
/// not merely that its TLS listener answered while its own control socket
/// queues connections nobody accepts — so a dial into such a remote fails in
/// seconds, by name, instead of holding the caller for a verb's deadline.
///
/// The drive stays protocol-agnostic: the caller supplies the request and says
/// which replies are refusals rather than answers.
#[derive(Clone, Copy, Debug)]
pub struct ConnectProbe {
    /// The request line, newline included.
    pub request: &'static [u8],
    /// How long the one-line reply may take.
    pub within: Duration,
    /// Whether a reply (newline stripped) refuses the connection.
    pub is_refusal: fn(&str) -> bool,
}

/// [`dial_and_relay_pinned`], proving with `probe` that the remote serves the
/// connection before `prebuffer` or any relayed byte is sent.
///
/// # Errors
/// Every error of [`dial_and_relay_pinned`], plus: no reply to the probe
/// within its bound (`ErrorKind::TimedOut`, "the remote accepted no connection
/// within Ns"), the remote closing first, or a reply `probe` calls a refusal —
/// all before any of the caller's bytes were sent.
#[allow(
    clippy::too_many_arguments,
    reason = "dial_and_relay_pinned's contract plus the probe"
)]
pub fn dial_and_relay_probed<A: ToSocketAddrs>(
    addr: A,
    config: Arc<ClientConfig>,
    src: &str,
    op: &str,
    token: &EdgeToken,
    prebuffer: &[u8],
    local: CtlStream,
    pin: Option<RemoteEndpoint>,
    probe: ConnectProbe,
) -> io::Result<()> {
    dial_and_relay_pinned_inner(
        addr,
        config,
        src,
        op,
        token,
        prebuffer,
        local,
        HANDSHAKE_TIMEOUT,
        pin.clone(),
        Some(probe),
        move |transport| match &pin {
            Some(pin) => observe_launch_nonce(pin, transport),
            None => Ok(None),
        },
    )
}

/// The probe's reply did not arrive within its bound.
fn probe_not_accepted(probe: ConnectProbe) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "the remote accepted no connection within {}s",
            probe.within.as_secs().max(1)
        ),
    )
}

/// Send `probe` and read its one-line reply within its bound (see
/// [`ConnectProbe`]). Byte at a time, so nothing past the line is consumed.
/// The per-read timeout below bounds an idle remote; the wall-clock bound
/// against one that dribbles is [`within_wall_clock`], which the dial wraps
/// this in.
fn expect_probe_answer(
    transport: &mut TlsTransport<rustls::ClientConnection>,
    probe: ConnectProbe,
) -> io::Result<()> {
    use std::io::{Read, Write};
    let stream = transport.stream();
    stream.write_all(probe.request)?;
    stream.flush()?;
    let deadline = Instant::now() + probe.within;
    let not_accepted = || probe_not_accepted(probe);
    let mut line = Vec::with_capacity(160);
    let mut byte = [0u8; 1];
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(not_accepted());
        }
        // One deadline across every read: a remote that dribbles cannot
        // stretch it.
        stream.get_mut().set_read_timeout(Some(deadline - now))?;
        match stream.read(&mut byte) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the remote closed the connection before answering",
                ));
            }
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) if line.len() >= 4096 => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "runaway probe reply",
                ));
            }
            Ok(_) => line.push(byte[0]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(not_accepted());
            }
            Err(e) => return Err(e),
        }
    }
    stream.get_mut().set_read_timeout(None)?;
    let line = String::from_utf8_lossy(&line);
    let line = line.trim_end_matches('\r');
    if (probe.is_refusal)(line) {
        return Err(io::Error::other(format!("the remote answered {line:?}")));
    }
    Ok(())
}

/// The most sessions, and the longest row, [`observe_launch_nonce`] reads.
const ROSTER_MAX_ROWS: usize = 4096;
const ROSTER_MAX_LINE: usize = 512;

/// Read the remote's live launch nonce for the [`RemoteEndpoint::matches`] rebind
/// check: `sessions bridge` over the authenticated relay (the listener AUTHs the
/// local control socket at Owner scope before relaying), whose answer is `OK <n>`
/// then one `<local> <sid> nonce=<hex32>` row per session. The pinned `sid`'s row
/// names its live nonce; a pin with no `sid` is observed when ANY session still
/// carries the pinned nonce. `None` when the remote reports no such session — a
/// relaunched or closed one — which the enforcement point refuses. The read is
/// bounded by the handshake deadline — per read here, and in wall-clock time
/// by [`within_wall_clock`], which the dial wraps this in — so a remote that
/// never answers, or answers a byte at a time, fails the dial instead of
/// hanging it.
fn observe_launch_nonce(
    pin: &RemoteEndpoint,
    transport: &mut TlsTransport<rustls::ClientConnection>,
) -> io::Result<Option<String>> {
    use std::io::Write as _;
    // `read_line`'s own end-of-stream words name the capability line, which
    // this exchange is past.
    let roster = |e: io::Error| {
        let said = if e.kind() == io::ErrorKind::UnexpectedEof {
            "the remote closed the connection before sending its session roster".to_owned()
        } else {
            format!("reading the remote's session roster: {e}")
        };
        io::Error::new(e.kind(), format!("{said}; nothing was relayed"))
    };
    let stream = transport.stream();
    stream.get_mut().set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    stream.write_all(b"sessions bridge\n")?;
    stream.flush()?;
    let head = crate::read_line(stream, ROSTER_MAX_LINE).map_err(roster)?;
    let n = head
        .strip_prefix("OK ")
        .and_then(|n| n.trim().parse::<usize>().ok())
        .filter(|n| *n <= ROSTER_MAX_ROWS)
        .ok_or_else(|| {
            io::Error::other(format!(
                "the remote refused `sessions bridge` ({:?}), so expect_nonce cannot be checked; \
                 nothing was relayed",
                head.chars().take(80).collect::<String>()
            ))
        })?;
    let mut rows = Vec::with_capacity(n);
    for _ in 0..n {
        rows.push(crate::read_line(stream, ROSTER_MAX_LINE).map_err(roster)?);
    }
    stream.get_mut().set_read_timeout(None)?;
    Ok(roster_nonce(pin, &rows))
}

/// The live nonce [`observe_launch_nonce`] reports for `pin` among `sessions
/// bridge` rows (module doc there).
fn roster_nonce(pin: &RemoteEndpoint, rows: &[String]) -> Option<String> {
    let mut parsed = rows.iter().filter_map(|row| {
        let mut fields = row.split(' ');
        let (_local, sid, nonce) = (fields.next()?, fields.next()?, fields.next()?);
        Some((sid, nonce.strip_prefix("nonce=")?))
    });
    if pin.sid.is_empty() {
        parsed
            .any(|(_, nonce)| nonce == pin.nonce)
            .then(|| pin.nonce.clone())
    } else {
        parsed
            .find(|(sid, _)| *sid == pin.sid)
            .map(|(_, nonce)| nonce.to_string())
    }
}

/// Run one pre-relay exchange with the authenticated remote under a TOTAL
/// wall-clock `bound`, the same way the unauthenticated phase is bounded.
///
/// A per-read socket timeout is not enough here either: rustls reads a whole
/// TLS record before a read returns, across as many socket reads as it takes,
/// so a remote that sends a record a byte at a time — slow, buggy, hostile, or
/// paced by anything on the path — restarts that timer with every byte and
/// holds the dial (and the caller behind it) for as long as it likes. The
/// [`HandshakeWatchdog`] force-closes the socket at `bound`, which ends the
/// exchange's blocked read whatever the remote does; past the bound the
/// result is `late()`, never the end-of-stream the force-close surfaces as.
fn within_wall_clock<T>(
    transport: &mut TlsTransport<rustls::ClientConnection>,
    bound: Duration,
    exchange: impl FnOnce(&mut TlsTransport<rustls::ClientConnection>) -> io::Result<T>,
    late: impl FnOnce() -> io::Error,
) -> io::Result<T> {
    let watchdog = HandshakeWatchdog::arm(transport.stream().get_ref(), bound)?;
    let result = exchange(transport);
    if watchdog.finish() == Claim::Authed {
        result
    } else {
        Err(late())
    }
}

/// [`dial_and_relay_pinned`] with the unauthenticated-phase deadline AND the
/// launch-nonce observer injected. `observe_nonce` yields the remote's live launch
/// nonce (`None` ⇒ the pinned session is not reported ⇒ a configured pin fails
/// closed); production passes [`observe_launch_nonce`], and tests may inject a
/// matching/mismatching nonce to exercise the guard alone. When
/// `pin` is `None` the observer is never called and the relay is byte-identical to
/// the un-pinned path.
#[allow(clippy::too_many_arguments)]
fn dial_and_relay_pinned_inner<A: ToSocketAddrs, N>(
    addr: A,
    config: Arc<ClientConfig>,
    src: &str,
    op: &str,
    token: &EdgeToken,
    prebuffer: &[u8],
    local: CtlStream,
    handshake_timeout: Duration,
    pin: Option<RemoteEndpoint>,
    probe: Option<ConnectProbe>,
    observe_nonce: N,
) -> io::Result<()>
where
    N: FnOnce(&mut TlsTransport<rustls::ClientConnection>) -> io::Result<Option<String>>,
{
    // Bound the CONNECT: a blackholed/unreachable host must fail fast, not hang for
    // the kernel SYN-retransmit window. Try EVERY resolved address (multi-homed
    // fallback, like std `TcpStream::connect`) so a host whose first address is
    // unreachable — commonly an IPv6 AAAA on a box with no working IPv6 path — still
    // connects via a later one; each attempt keeps its own connect deadline. The
    // empty-iterator case stays an explicit error.
    let mut tcp = None;
    let mut last_err = None;
    for sockaddr in addr.to_socket_addrs()? {
        match TcpStream::connect_timeout(&sockaddr, handshake_timeout) {
            Ok(s) => {
                tcp = Some(s);
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    let tcp = tcp.ok_or_else(|| {
        last_err.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "no socket address resolved")
        })
    })?;

    // Bound the unauthenticated phase on the DIALER too (symmetric with the
    // listener). Per-syscall floor: bounds a single fully-idle read/write.
    tcp.set_read_timeout(Some(handshake_timeout))?;
    tcp.set_write_timeout(Some(handshake_timeout))?;

    // Total wall-clock deadline on the TLS handshake + the OK/DENIED verdict read,
    // which a peer that dribbles bytes cannot reset ([`HandshakeWatchdog`]). Armed
    // before `tcp` moves into `tls::connect`.
    let watchdog = HandshakeWatchdog::arm(&tcp, handshake_timeout)?;

    // The unauthenticated phase: complete TLS, then present the channel-bound
    // capability and read the verdict. Any error here (incl. the watchdog's
    // force-close) tears the connection down.
    let unauth = (|| -> io::Result<TlsTransport<rustls::ClientConnection>> {
        let mut transport = tls::connect(tcp, tls::fixed_server_name(), config)?;
        let exporter = transport.exporter().to_vec();
        present_capability(transport.stream(), &exporter, src, op, token)?;
        Ok(transport)
    })();
    let authed_in_time = watchdog.finish() == Claim::Authed;

    // A deadline — the watchdog's force-close (which the blocked read sees as an
    // end of stream) or the per-syscall timer — is said as one, never as the
    // `unexpected end of file` it surfaces as.
    let mut transport = match unauth {
        Ok(t) if authed_in_time => t,
        Err(e) if authed_in_time && !is_deadline(&e) => return Err(e),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("the remote did not finish the handshake within {handshake_timeout:?}"),
            ));
        }
    };

    // Authenticated: drop the handshake deadline before the (long-lived) relay,
    // which installs its own non-blocking poll on the same socket.
    {
        let sock = transport.stream().get_mut();
        sock.set_read_timeout(None)?;
        sock.set_write_timeout(None)?;
    }

    // Enforce the session-NONCE rebind guard BEFORE any control bytes relay, if the
    // caller configured a pin. The cert-fingerprint half is already TLS-enforced (a
    // non-pinned cert never completes the handshake above), so the observed
    // fingerprint provably equals `pin.fingerprint`; only the launch-nonce half
    // remains, read here via `observe_nonce`. A pinned session the remote does not
    // report (`None`) FAILS CLOSED — an operator-requested guard is never silently
    // skipped.
    if let Some(pin) = &pin {
        // TCP, TLS and the token are done by now: only the relay is refused.
        let sid = pin.sid.escape_debug();
        let observed = within_wall_clock(&mut transport, handshake_timeout, observe_nonce, || {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "the remote sent no session roster within {handshake_timeout:?}; nothing was \
                     relayed"
                ),
            )
        })?;
        let refusal = match observed {
            Some(nonce) if pin.matches(&pin.fingerprint, &nonce) => None,
            Some(_) if !pin.sid.is_empty() => Some(format!(
                "session {sid} was relaunched: its launch nonce no longer matches expect_nonce; \
                 nothing was relayed"
            )),
            None if !pin.sid.is_empty() => Some(format!(
                "the remote has no session {sid}; nothing was relayed"
            )),
            Some(_) | None => Some(
                "no remote session carries expect_nonce (relaunched or closed); nothing was \
                 relayed"
                    .to_owned(),
            ),
        };
        if let Some(refusal) = refusal {
            return Err(io::Error::other(refusal));
        }
    }

    // Prove the remote serves the connection before any of the caller's bytes.
    if let Some(probe) = probe {
        within_wall_clock(
            &mut transport,
            probe.within,
            |transport| expect_probe_answer(transport, probe),
            || probe_not_accepted(probe),
        )?;
    }

    if !prebuffer.is_empty() {
        use std::io::Write;
        transport.stream().write_all(prebuffer)?;
        transport.stream().flush()?;
    }
    tls::relay(transport, local)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tls::{cert_fingerprint, client_config, server_config};
    use aterm_spec::wait::MUST_HAPPEN;
    use std::io::{Read, Write};
    use std::net::Ipv4Addr;
    use std::sync::atomic::AtomicUsize;

    const TEST_CERT_DER: &[u8] = include_bytes!("testdata/cert.der");
    const TEST_KEY_DER: &[u8] = include_bytes!("testdata/key.pkcs8.der");

    /// How many bytes the slow-loris peers dribble, one per 80 ms: over five
    /// minutes of a per-syscall timer kept alive, so only the wall-clock
    /// watchdog ends the handshake inside the minute's hang detector. A peer
    /// stops at the first byte the cut connection refuses.
    const DRIBBLE_BYTES: usize = 4_000;

    /// Echo whatever arrives on `s` back to it, until EOF. Stands in for the
    /// host's local control socket.
    fn spawn_echo(mut s: CtlStream) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if s.write_all(&buf[..n]).and_then(|()| s.flush()).is_err() {
                            break;
                        }
                    }
                }
            }
        })
    }

    #[test]
    fn real_listener_readiness_wakes_before_the_shutdown_interval() {
        // Deliberately much longer than production's 200 ms bound: a
        // sleep-based implementation pays the whole five minutes and fails by a
        // wide margin. The wake ceiling is a minute's hang detector (AGENTS.md),
        // never a latency budget, so it still distinguishes that negative
        // control and no scheduler starvation of a loaded gate reaches it.
        const TEST_WAIT: Duration = Duration::from_secs(300);
        const MAX_READY_WAKE: Duration = MUST_HAPPEN;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let waiting_listener = listener.try_clone().unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
        let waiter = std::thread::spawn(move || {
            // Rendezvous immediately before the readiness wait. If this path
            // regresses to sleeping for the requested interval, the connection
            // below arrives just after that sleep starts and waits out all of it.
            entered_tx.send(()).unwrap();
            wait_listener_readable(&waiting_listener, TEST_WAIT)
        });
        entered_rx.recv().unwrap();

        let started = Instant::now();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        assert!(waiter.join().unwrap().unwrap());
        let elapsed = started.elapsed();
        assert!(
            elapsed < MAX_READY_WAKE,
            "a queued real connection must wake readiness instead of waiting out \
             the {TEST_WAIT:?} test interval ({elapsed:?})"
        );
        let (accepted, _) = listener
            .accept()
            .expect("the readiness event corresponds to an accept-ready connection");
        drop((accepted, client));
    }

    #[test]
    fn idle_listener_readiness_wait_is_bounded_without_spinning() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let started = Instant::now();
        assert!(!wait_listener_readable(&listener, ACCEPT_POLL).unwrap());
        let elapsed = started.elapsed();
        assert!(
            elapsed >= ACCEPT_POLL - Duration::from_millis(40),
            "an idle readiness wait returned too early to replace sleep polling ({elapsed:?})"
        );
        assert!(
            elapsed < MUST_HAPPEN,
            "the readiness timeout must retain the {ACCEPT_POLL:?} shutdown bound ({elapsed:?})"
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn driver_dials_listener_presents_capability_and_relays_to_the_local_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let ccfg = client_config(cert_fingerprint(TEST_CERT_DER));
        let token = EdgeToken::generate();

        // Host: its "local control socket" is an echo over a CtlStream pair.
        let (svc_a, svc_b) = CtlStream::pair().unwrap();
        let echo = spawn_echo(svc_b);
        let svc_a = Arc::new(Mutex::new(Some(svc_a)));

        let host = std::thread::spawn({
            let scfg = Arc::clone(&scfg);
            let svc_a = Arc::clone(&svc_a);
            move || {
                let (tcp, _) = listener.accept().unwrap();
                accept_and_relay(
                    tcp,
                    scfg,
                    |src, op| (src == "driver-1" && op == "drive").then_some(token),
                    || Ok(svc_a.lock().unwrap().take().unwrap()),
                )
            }
        });

        // Driver: relays its local control client (the test holds the peer). A
        // PREBUFFER (pipelined bytes) is forwarded to the remote before the relay;
        // the echo mirrors it back first, proving order.
        let (drv_local, mut drv_client) = CtlStream::pair().unwrap();
        let driver = std::thread::spawn(move || {
            dial_and_relay_pinned(
                addr, ccfg, "driver-1", "drive", &token, b"PRE\n", drv_local, None,
            )
        });

        {
            let mut pre = [0u8; 4];
            drv_client.read_exact(&mut pre).unwrap();
            assert_eq!(
                &pre, b"PRE\n",
                "the prebuffer is forwarded to the remote before relaying"
            );
        }

        // Drive a control exchange end-to-end: client -> driver -> TLS -> host ->
        // local socket (echo) -> back.
        drv_client.write_all(b"screen\n").unwrap();
        drv_client.flush().unwrap();
        let mut got = [0u8; 7];
        drv_client.read_exact(&mut got).unwrap();
        assert_eq!(
            &got, b"screen\n",
            "the remote drive round-tripped a control verb"
        );

        // Close the local client -> both relays tear down, both ends return.
        drv_client.shutdown(std::net::Shutdown::Both).unwrap();
        drop(drv_client);

        let granted = host.join().unwrap().unwrap();
        assert_eq!(
            granted,
            Granted {
                src: "driver-1".into(),
                op: "drive".into()
            }
        );
        driver.join().unwrap().ok();
        echo.join().ok();
    }

    /// Stand up a one-connection listener whose "local control socket" is
    /// `service`, and dial it with `probe`. Returns the dial's result and the
    /// driver's local client end.
    fn dial_probed(
        service: CtlStream,
        probe: ConnectProbe,
        prebuffer: &'static [u8],
    ) -> (
        std::thread::JoinHandle<io::Result<()>>,
        CtlStream,
        std::thread::JoinHandle<io::Result<Granted>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let ccfg = client_config(cert_fingerprint(TEST_CERT_DER));
        let token = EdgeToken::generate();
        let service = Arc::new(Mutex::new(Some(service)));
        let host = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            accept_and_relay(
                tcp,
                scfg,
                |src, op| (src == "driver-1" && op == "drive").then_some(token),
                || Ok(service.lock().unwrap().take().unwrap()),
            )
        });
        let (drv_local, drv_client) = CtlStream::pair().unwrap();
        let driver = std::thread::spawn(move || {
            dial_and_relay_probed(
                addr, ccfg, "driver-1", "drive", &token, prebuffer, drv_local, None, probe,
            )
        });
        (driver, drv_client, host)
    }

    /// THE WEDGED REMOTE. The remote's TLS listener accepts and verifies, but
    /// its local control socket never answers (queued, never accepted). The
    /// probed dial reports it within the probe's bound, by name, and never sends
    /// the caller's bytes.
    #[test]
    fn a_probed_dial_into_a_remote_that_serves_nothing_fails_fast() {
        let (svc_a, svc_b) = CtlStream::pair().unwrap();
        let probe = ConnectProbe {
            request: b"version\n",
            within: Duration::from_millis(400),
            is_refusal: |_| false,
        };
        let started = Instant::now();
        let (driver, drv_client, host) = dial_probed(svc_a, probe, b"secret-verb\n");
        let error = driver
            .join()
            .unwrap()
            .expect_err("nothing served the probe");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
        assert!(
            error.to_string().contains("accepted no connection"),
            "{error}"
        );
        // A hang detector (AGENTS.md): without the probe's bound the read has no
        // timeout at all, and the wedged remote never answers.
        assert!(
            started.elapsed() < MUST_HAPPEN,
            "the probe bound held: {:?}",
            started.elapsed()
        );
        // The remote's local socket saw the probe and nothing of the caller's.
        // (Read before the host is joined: its relay ends only once this
        // local end closes too.)
        svc_b
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let mut heard = Vec::new();
        let _ = (&svc_b).read_to_end(&mut heard);
        assert_eq!(heard, b"version\n", "only the probe reached the remote");
        drop(svc_b);
        drop(drv_client);
        let _ = host.join();
    }

    /// The served case (the negative control): the remote answers the probe,
    /// the answer is consumed rather than relayed, and the caller's bytes then
    /// flow as before. A reply the probe calls a refusal fails the dial.
    #[test]
    fn a_probed_dial_consumes_the_answer_and_honours_a_refusal() {
        let (svc_a, svc_b) = CtlStream::pair().unwrap();
        let echo = spawn_echo(svc_b);
        let probe = ConnectProbe {
            request: b"PING\n",
            within: MUST_HAPPEN,
            is_refusal: |line| line.starts_with("ERR"),
        };
        let (driver, mut drv_client, host) = dial_probed(svc_a, probe, b"PRE\n");
        let mut pre = [0u8; 4];
        drv_client.read_exact(&mut pre).unwrap();
        assert_eq!(&pre, b"PRE\n", "the probe's answer is not relayed");
        drv_client.shutdown(std::net::Shutdown::Both).unwrap();
        drop(drv_client);
        driver.join().unwrap().ok();
        let _ = host.join();
        echo.join().ok();

        let (svc_a, svc_b) = CtlStream::pair().unwrap();
        let echo = spawn_echo(svc_b);
        let refused = ConnectProbe {
            request: b"ERR control server busy; retry\n",
            within: MUST_HAPPEN,
            is_refusal: |line| line.starts_with("ERR control server busy"),
        };
        let (driver, drv_client, host) = dial_probed(svc_a, refused, b"PRE\n");
        let error = driver
            .join()
            .unwrap()
            .expect_err("a refusal fails the dial");
        assert!(error.to_string().contains("busy"), "{error}");
        drop(drv_client);
        let _ = host.join();
        echo.join().ok();
    }

    #[test]
    fn listener_authenticates_its_local_socket_so_the_token_never_crosses_the_wire() {
        // The production pattern: the LISTENER's connect_local injects `AUTH <tok>`
        // into the local control socket, so the dialer never sends the raw token —
        // only the channel-bound HMAC. Here the local "control socket" requires the
        // AUTH line as its first line, then echoes; the dialer sends ONLY a verb.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let ccfg = client_config(cert_fingerprint(TEST_CERT_DER));
        let token = EdgeToken::generate();
        let auth_line = format!("AUTH {}\n", token.to_hex());

        // The local control socket: assert it receives AUTH first, then echo.
        let (svc_a, mut svc_b) = CtlStream::pair().unwrap();
        let expect_auth = auth_line.clone();
        let svc = std::thread::spawn(move || {
            let mut got = vec![0u8; expect_auth.len()];
            svc_b.read_exact(&mut got).unwrap();
            assert_eq!(
                got,
                expect_auth.as_bytes(),
                "listener must inject AUTH first"
            );
            // then echo whatever the driver sends
            let mut buf = [0u8; 256];
            loop {
                match svc_b.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if svc_b
                            .write_all(&buf[..n])
                            .and_then(|()| svc_b.flush())
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
        let svc_a = Arc::new(Mutex::new(Some(svc_a)));

        let host = std::thread::spawn({
            let svc_a = Arc::clone(&svc_a);
            let auth_line = auth_line.clone();
            move || {
                let (tcp, _) = listener.accept().unwrap();
                accept_and_relay(
                    tcp,
                    scfg,
                    |_s, op| (op == "drive").then_some(token),
                    move || {
                        // Inject the inner AUTH (the dialer never sees/sends the token).
                        let mut s = svc_a.lock().unwrap().take().unwrap();
                        s.write_all(auth_line.as_bytes())?;
                        s.flush()?;
                        Ok(s)
                    },
                )
            }
        });

        // Dialer: present the capability, then send ONLY a verb (no token).
        let (drv_local, mut drv_client) = CtlStream::pair().unwrap();
        let driver = std::thread::spawn(move || {
            dial_and_relay_pinned(addr, ccfg, "dial", "drive", &token, b"", drv_local, None)
        });
        drv_client.write_all(b"screen\n").unwrap();
        drv_client.flush().unwrap();
        let mut got = [0u8; 7];
        drv_client.read_exact(&mut got).unwrap();
        assert_eq!(
            &got, b"screen\n",
            "the verb round-tripped through the authenticated relay"
        );

        drv_client.shutdown(std::net::Shutdown::Both).unwrap();
        drop(drv_client);
        host.join().unwrap().unwrap();
        driver.join().unwrap().ok();
        svc.join().unwrap();
    }

    #[test]
    fn a_stalled_pre_auth_peer_is_dropped_by_the_handshake_deadline() {
        // A peer that opens TCP and then sends NOTHING must not pin the handler
        // thread: the unauthenticated-phase deadline fires and accept_and_relay
        // returns an error WITHOUT ever dialing the local socket.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let token = EdgeToken::generate();
        let dialed_local = Arc::new(AtomicBool::new(false));

        let host = std::thread::spawn({
            let dialed_local = Arc::clone(&dialed_local);
            move || {
                let (tcp, _) = listener.accept().unwrap();
                // 300ms deadline (vs the 10s default) so the test is fast.
                accept_and_relay_inner(
                    tcp,
                    scfg,
                    |_s, _o| Some(token),
                    || {
                        dialed_local.store(true, Ordering::SeqCst);
                        CtlStream::pair().map(|(a, _b)| a)
                    },
                    Duration::from_millis(300),
                    |_| {},
                )
            }
        });

        // Connect, then stall (send nothing, hold the socket open).
        let _stalled = TcpStream::connect(addr).unwrap();
        let started = Instant::now();
        let res = host.join().unwrap();
        let Err(err) = res else {
            panic!("a stalled pre-auth peer must be dropped, not relayed");
        };
        // Said as the deadline it is, not as the raw end of stream or EAGAIN
        // the force-close or the socket timer surfaces as.
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
        assert_eq!(err.to_string(), "no handshake within 300ms");
        assert!(
            !dialed_local.load(Ordering::SeqCst),
            "a peer that never authenticated must NEVER dial the local socket"
        );
        // A hang detector (AGENTS.md): the stalled peer holds the socket open
        // for as long as the test runs.
        assert!(
            started.elapsed() < MUST_HAPPEN,
            "the handshake deadline must fire promptly (~300ms), not hang"
        );
    }

    #[test]
    fn a_dribbling_pre_auth_peer_is_cut_off_by_the_wall_clock_deadline() {
        // The HARDER slow-loris: a peer that keeps the per-syscall timer alive by
        // dribbling bytes must still be cut off by the TOTAL wall-clock deadline.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let token = EdgeToken::generate();
        let dialed_local = Arc::new(AtomicBool::new(false));

        let host = std::thread::spawn({
            let dialed_local = Arc::clone(&dialed_local);
            move || {
                let (tcp, _) = listener.accept().unwrap();
                accept_and_relay_inner(
                    tcp,
                    scfg,
                    |_s, _o| Some(token),
                    || {
                        dialed_local.store(true, Ordering::SeqCst);
                        CtlStream::pair().map(|(a, _b)| a)
                    },
                    Duration::from_millis(300),
                    |_| {},
                )
            }
        });

        // A valid TLS record HEADER (handshake, len=4096) so rustls keeps waiting
        // for the body — then dribble it 1 byte / 80ms, well under the 300ms
        // per-syscall timeout, so ONLY the wall-clock watchdog can stop it. It
        // dribbles for over five minutes ([`DRIBBLE_BYTES`]), so a handler the
        // watchdog does not cut is held far past the hang detector below.
        let mut peer = TcpStream::connect(addr).unwrap();
        let started = Instant::now();
        let dribbler = std::thread::spawn(move || {
            let _ = peer.write_all(&[0x16, 0x03, 0x03, 0x10, 0x00]);
            let _ = peer.flush();
            for _ in 0..DRIBBLE_BYTES {
                if peer.write_all(&[0x00]).and_then(|()| peer.flush()).is_err() {
                    break; // server force-closed -> our writes start failing
                }
                std::thread::sleep(Duration::from_millis(80));
            }
        });

        let res = host.join().unwrap();
        let elapsed = started.elapsed();
        // The watchdog's force-close reaches the blocked read as an end of
        // stream; it is still said as the deadline.
        assert_eq!(
            res.as_ref().map_err(io::Error::kind).err(),
            Some(io::ErrorKind::TimedOut),
            "{:?}",
            res.as_ref().err()
        );
        assert!(
            res.is_err(),
            "a dribbling pre-auth peer must be cut off, not relayed"
        );
        assert!(
            !dialed_local.load(Ordering::SeqCst),
            "a dribbling unauthenticated peer must NEVER dial the local socket"
        );
        assert!(
            elapsed >= Duration::from_millis(200),
            "the cut-off must be the wall-clock watchdog (~300ms), not an instant reject ({elapsed:?})"
        );
        assert!(
            // Upper bound: a minute's hang detector (AGENTS.md), never a latency
            // budget. Without the watchdog the dribble keeps the per-syscall
            // timer alive for its whole five minutes, so this still fails loudly
            // on 'runs unbounded'.
            elapsed < MUST_HAPPEN,
            "the wall-clock deadline must bound the dribble, not run unbounded ({elapsed:?})"
        );
        let _ = dribbler.join();
    }

    #[test]
    fn a_dribbling_server_is_cut_off_by_the_dialers_wall_clock_deadline() {
        // The DIALER mirror of the listener slow-loris test: a pinned-but-buggy
        // server that completes the TCP connect then DRIBBLES the TLS handshake
        // byte-by-byte keeps the dialer's per-syscall SO_RCVTIMEO alive forever, so
        // only the TOTAL wall-clock watchdog can cut it off — otherwise the dial
        // hangs for hours instead of failing fast.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let ccfg = client_config(cert_fingerprint(TEST_CERT_DER));
        let token = EdgeToken::generate();

        // The "server": accept the TCP connection, send a valid TLS record HEADER
        // (handshake, len=4096) so the dialer's rustls keeps waiting for the body,
        // then dribble it 1 byte / 80ms — well under the 300ms per-syscall timeout,
        // so ONLY the wall-clock watchdog can stop the dial. For over five minutes
        // ([`DRIBBLE_BYTES`]): a dial the watchdog does not cut is held far past
        // the hang detector below.
        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            let _ = peer.write_all(&[0x16, 0x03, 0x03, 0x10, 0x00]);
            let _ = peer.flush();
            for _ in 0..DRIBBLE_BYTES {
                if peer.write_all(&[0x00]).and_then(|()| peer.flush()).is_err() {
                    break; // dialer force-closed -> our writes start failing
                }
                std::thread::sleep(Duration::from_millis(80));
            }
        });

        let (drv_local, _drv_client) = CtlStream::pair().unwrap();
        let started = Instant::now();
        // 300ms deadline (vs the 10s default) so the test is fast.
        let res = dial_and_relay_pinned_inner(
            addr,
            ccfg,
            "dial",
            "drive",
            &token,
            b"",
            drv_local,
            Duration::from_millis(300),
            None,
            None,
            |_t| Ok(None),
        );
        let elapsed = started.elapsed();
        let Err(err) = res else {
            panic!("a dribbling server must be cut off, not left to hang the dial");
        };
        // Said as the deadline, not as the `unexpected end of file` the
        // watchdog's force-close surfaces as in the blocked handshake read.
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
        assert_eq!(
            err.to_string(),
            "the remote did not finish the handshake within 300ms"
        );
        assert!(
            elapsed >= Duration::from_millis(200),
            "the cut-off must be the wall-clock watchdog (~300ms), not an instant reject ({elapsed:?})"
        );
        assert!(
            // Upper bound: a minute's hang detector (AGENTS.md), never a latency
            // budget. Without the watchdog the dribble keeps the per-syscall
            // timer alive for its whole five minutes, so this still fails loudly
            // on 'runs unbounded'.
            elapsed < MUST_HAPPEN,
            "the wall-clock deadline must bound the dribble, not run unbounded ({elapsed:?})"
        );
        let _ = server.join();
    }

    /// A pinned listener that grants the capability, reads `request` (the
    /// dialer's first post-grant exchange), and then answers it with one TLS
    /// record sent a byte every 80 ms — the ciphertext a slow, buggy or hostile
    /// remote (or anything on the path pacing its segments) delivers. Each byte
    /// restarts a per-read timer, and rustls reads the whole record before it
    /// returns, so only a wall-clock bound ends the dialer's wait.
    fn granting_listener_that_dribbles_after(
        request: &'static [u8],
    ) -> (SocketAddr, EdgeToken, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let token = EdgeToken::generate();
        let server = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let _ = tcp.set_read_timeout(Some(MUST_HAPPEN));
            let Ok(mut transport) = tls::accept(tcp, scfg) else {
                return;
            };
            let exporter = transport.exporter().to_vec();
            if verify_capability(transport.stream(), &exporter, |_, _| Some(token)).is_err() {
                return;
            }
            let mut heard = vec![0u8; request.len()];
            if transport.stream().read_exact(&mut heard).is_err() || heard != request {
                return;
            }
            // An application-data record claiming 16 KiB, then its body.
            let sock = transport.stream().get_mut();
            if sock.write_all(&[0x17, 0x03, 0x03, 0x40, 0x00]).is_err() {
                return;
            }
            for _ in 0..DRIBBLE_BYTES {
                if sock.write_all(&[0x00]).and_then(|()| sock.flush()).is_err() {
                    break; // the dialer gave up
                }
                std::thread::sleep(Duration::from_millis(80));
            }
        });
        (addr, token, server)
    }

    /// Run `dial` on a thread and wait for its result, at most a minute's hang
    /// detector.
    fn dial_result(
        what: &str,
        dial: impl FnOnce() -> io::Result<()> + Send + 'static,
    ) -> io::Result<()> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(dial());
        });
        rx.recv_timeout(MUST_HAPPEN)
            .unwrap_or_else(|_| panic!("{what}: the dial was still waiting after {MUST_HAPPEN:?}"))
    }

    #[test]
    fn a_probe_answer_dribbled_by_the_remote_is_cut_off_at_the_probe_bound() {
        let (addr, token, server) = granting_listener_that_dribbles_after(b"version\n");
        let ccfg = client_config(cert_fingerprint(TEST_CERT_DER));
        let (drv_local, drv_client) = CtlStream::pair().unwrap();
        let probe = ConnectProbe {
            request: b"version\n",
            within: Duration::from_millis(400),
            is_refusal: |_| false,
        };
        let error = dial_result("the probe", move || {
            dial_and_relay_probed(
                addr, ccfg, "driver-1", "drive", &token, b"", drv_local, None, probe,
            )
        })
        .expect_err("a probe answer that never completes cannot pass the probe");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
        assert!(
            error.to_string().contains("accepted no connection"),
            "{error}"
        );
        drop(drv_client);
        let _ = server.join();
    }

    #[test]
    fn a_session_roster_dribbled_by_the_remote_is_cut_off_at_the_handshake_bound() {
        let (addr, token, server) = granting_listener_that_dribbles_after(b"sessions bridge\n");
        let ccfg = client_config(cert_fingerprint(TEST_CERT_DER));
        let (drv_local, drv_client) = CtlStream::pair().unwrap();
        let pin = RemoteEndpoint {
            host: addr.to_string(),
            sid: "s1".into(),
            nonce: "0011".into(),
            fingerprint: "pinned".into(),
        };
        let error = dial_result("the roster", move || {
            dial_and_relay_pinned_inner(
                addr,
                ccfg,
                "driver-1",
                "drive",
                &token,
                b"",
                drv_local,
                Duration::from_millis(300),
                Some(pin.clone()),
                None,
                move |transport| observe_launch_nonce(&pin, transport),
            )
        })
        .expect_err("a roster that never completes cannot satisfy the pin");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
        drop(drv_client);
        let _ = server.join();
    }

    /// The watchdog's wake, with no TLS and no second end: `finish` on a
    /// handshake that authenticated early must not wait for the watchdog
    /// thread to come round on its own. A thread that sleep-polls in steps is
    /// JOINED only when its current step expires, so `finish` would pay the
    /// rest of that step in every sample; the event-driven thread is woken by
    /// the `unpark` and pays one scheduling latency. The deadline is armed at 60 s
    /// so no sample can end by it, and the minimum of ten filters a stalled
    /// wake without letting a poll step through.
    ///
    /// NON-VACUITY: `finish` is timed only once the thread has entered its
    /// wait. Called before that, it finds the state already claimed and
    /// returns at once whatever the wait is.
    #[test]
    fn finishing_an_early_handshake_does_not_wait_out_a_wake_interval() {
        const MAX_FINISH: Duration = Duration::from_millis(20);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let accepting = std::thread::spawn(move || listener.accept().unwrap().0);
        let tcp = TcpStream::connect(addr).unwrap();
        let _peer = accepting.join().unwrap();
        let fastest = (0..10)
            .map(|_| {
                let watchdog = HandshakeWatchdog::arm(&tcp, Duration::from_secs(60)).unwrap();
                let entered = Instant::now() + MUST_HAPPEN;
                while !watchdog.waiting.load(Ordering::Acquire) {
                    assert!(
                        Instant::now() < entered,
                        "the watchdog never began its wait"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                let started = Instant::now();
                assert_eq!(
                    watchdog.finish(),
                    Claim::Authed,
                    "an early handshake beats a 60 s deadline"
                );
                started.elapsed()
            })
            .min()
            .unwrap();
        assert!(
            fastest < MAX_FINISH,
            "finish() waited {fastest:?} at best for a watchdog it had just woken — \
             the watchdog is being waited out on a wake interval, not woken"
        );
    }

    #[test]
    fn spurious_wakes_neither_shorten_nor_starve_the_watchdog_deadline() {
        // The hazard the event-driven watchdog introduces, tested head-on. Its wake
        // channel is `Thread::unpark`, and `park_timeout` is also allowed to return
        // spuriously. Either could break the deadline in one of two directions:
        //   * SHORTEN it — the watchdog concludes on a wake and force-closes early,
        //     killing a connection that was still inside its budget; or
        //   * STARVE it — a wake restarts the wait and the deadline never arrives, so
        //     a slow-loris peer pins the handler forever. A watchdog that can be
        //     starved of its deadline is a hang.
        // So: arm a short deadline on a real socket, never claim WD_AUTHED, and
        // hammer the wake channel every 3 ms — for far LONGER than the deadline, and
        // longer than the ceiling asserted below, because a starvation bug only
        // shows while the wakes keep coming. The force-close must still land at the
        // deadline: not before it, and not pushed out by the hammering. A correct
        // watchdog ends the hammering at once: it stops at the force-close.
        const DEADLINE: Duration = Duration::from_millis(150);
        /// Must exceed `CEILING`, or a watchdog whose deadline restarts on every
        /// wake would simply fire once the hammering stopped and escape the test.
        const HAMMER_BUDGET: Duration = Duration::from_secs(180);
        /// A minute's hang detector (AGENTS.md), never a latency budget: far
        /// under `HAMMER_BUDGET`, and out of reach of any scheduling noise.
        const CEILING: Duration = MUST_HAPPEN;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let accepting = std::thread::spawn(move || listener.accept().unwrap().0);
        let tcp = TcpStream::connect(addr).unwrap();
        // Hold the peer open and silent: the ONLY thing that can end our read is the
        // watchdog force-closing our own socket.
        let _peer = accepting.join().unwrap();
        tcp.set_read_timeout(Some(Duration::from_millis(5)))
            .unwrap();

        let started = Instant::now();
        let watchdog = HandshakeWatchdog::arm(&tcp, DEADLINE).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let waker = watchdog.thread.thread().clone();
        let hammer = std::thread::spawn({
            let stop = Arc::clone(&stop);
            move || {
                while !stop.load(Ordering::SeqCst) && started.elapsed() < HAMMER_BUDGET {
                    waker.unpark();
                    std::thread::sleep(Duration::from_millis(3));
                }
            }
        });

        let mut probe = &tcp;
        let mut buf = [0u8; 1];
        let closed_after = loop {
            match probe.read(&mut buf) {
                Ok(0) => break started.elapsed(), // force-closed by the watchdog
                Ok(_) => panic!("the silent peer cannot have sent anything"),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    assert!(
                        started.elapsed() < MUST_HAPPEN,
                        "the deadline was STARVED outright: the watchdog never force-closed"
                    );
                }
                Err(_) => break started.elapsed(), // also a close
            }
        };
        stop.store(true, Ordering::SeqCst);
        hammer.join().unwrap();

        assert!(
            closed_after >= DEADLINE.mul_f64(0.8),
            "a wake must not SHORTEN the deadline: force-closed after {closed_after:?}, \
             but the deadline is {DEADLINE:?}"
        );
        assert!(
            closed_after < CEILING,
            "a wake must not STARVE the deadline: the deadline is {DEADLINE:?} but the \
             force-close only landed after {closed_after:?} of continuous wakes"
        );
        assert_eq!(
            watchdog.finish(),
            Claim::Deadline,
            "the watchdog claimed the deadline, so the handshake must lose the CAS"
        );
    }

    /// CONFORMANCE (Tier-1) of the REAL listener to
    /// `aterm_spec::derive::net_dial_after_grant_model` (`NetDialAfterGrant`):
    /// `accept_and_relay` dials its local control socket only AFTER the
    /// channel-bound capability was granted.
    ///
    /// Both halves of the latch pair are observed on the real wire, never read
    /// off the code. `granted` is the verdict the listener sent the dialer
    /// (`verify_capability` writes `OK` before it returns); `local_dialed` is the
    /// listener calling `connect_local`. To order them, the `connect_local`
    /// closure records what the dialer had been TOLD at the moment it is called
    /// — it waits for the dialer's verdict, which a listener that grants first
    /// has already sent. A valid and a forged presentation are each driven, and
    /// every step they took (`Verify`, `DialLocal`, in the order observed) must
    /// be one the committed model admits, ending in a `DialImpliesGranted` state.
    ///
    /// NEGATIVE CONTROL: the premature dial is the step only `Buggy = 1` admits,
    /// and the real runs are checked to never take it. Call `connect_local()`
    /// before `verify_capability` in `accept_and_relay_inner` and the dial is
    /// observed with no verdict yet sent — a `DialLocal` from `granted = 0`,
    /// which the committed model rejects: this test fails.
    #[test]
    fn accept_and_relay_conforms_to_net_dial_after_grant() {
        use aterm_spec::derive::net_dial_after_grant_model;
        use aterm_spec::verify::validate_transition_tiered;
        use std::collections::BTreeMap;

        let model = net_dial_after_grant_model();
        let at = |granted: bool, dialed: bool| -> BTreeMap<&'static str, i64> {
            BTreeMap::from([
                ("granted", i64::from(granted)),
                ("local_dialed", i64::from(dialed)),
            ])
        };
        // The model's own negative control: from the start, only the mutant dials.
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        assert!(!model.action_enabled("DialLocal", &model.init_state()));
        assert!(buggy.action_enabled("DialLocal", &buggy.init_state()));

        // One real listener run: the dialer's verdict, and — if the listener
        // dialed its local socket — whether the dialer had been granted by then.
        let run = |presented: EdgeToken, minted: EdgeToken| -> (bool, Option<bool>) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
            let ccfg = client_config(cert_fingerprint(TEST_CERT_DER));
            let (told_tx, told_rx) = std::sync::mpsc::channel::<bool>();
            let dial = Arc::new(Mutex::new(None::<bool>));
            let host = std::thread::spawn({
                let dial = Arc::clone(&dial);
                move || {
                    let (tcp, _) = listener.accept().unwrap();
                    accept_and_relay(
                        tcp,
                        scfg,
                        |src, op| (src == "driver-1" && op == "drive").then_some(minted),
                        move || {
                            let granted = told_rx.recv_timeout(MUST_HAPPEN) == Ok(true);
                            *dial.lock().unwrap() = Some(granted);
                            // The local peer hangs up at once, so the relay ends.
                            CtlStream::pair().map(|(local, _peer)| local)
                        },
                    )
                }
            });
            let tcp = TcpStream::connect(addr).unwrap();
            tcp.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
            let mut dialer = tls::connect(tcp, tls::fixed_server_name(), ccfg).unwrap();
            let exporter = dialer.exporter().to_vec();
            let verdict =
                present_capability(dialer.stream(), &exporter, "driver-1", "drive", &presented)
                    .is_ok();
            let _ = told_tx.send(verdict);
            drop(dialer);
            let _ = host.join().unwrap();
            let dialed = dial.lock().unwrap().take();
            (verdict, dialed)
        };

        let token = EdgeToken::generate();
        for (what, presented) in [("valid", token), ("forged", EdgeToken::generate())] {
            let (verdict, dialed) = run(presented, token);
            // The steps in the order the wire showed them.
            let steps: Vec<&str> = match dialed {
                Some(true) => vec!["Verify", "DialLocal"],
                Some(false) if verdict => vec!["DialLocal", "Verify"],
                Some(false) => vec!["DialLocal"],
                None if verdict => vec!["Verify"],
                None => vec![],
            };
            let (mut granted, mut local_dialed) = (false, false);
            for step in &steps {
                let before = at(granted, local_dialed);
                match *step {
                    "Verify" => granted = true,
                    _ => local_dialed = true,
                }
                let after = at(granted, local_dialed);
                let (ok, why) = validate_transition_tiered(
                    &model,
                    &[],
                    &before,
                    &after,
                    Some(step),
                    "NetDialAfterGrant(accept_and_relay)",
                );
                assert!(
                    ok,
                    "{what}: the real listener took `{step}` from {before:?}, which the \
                     model does not admit (observed order {steps:?}) — {why}"
                );
            }
            assert!(
                model.check_invariant("DialImpliesGranted", &at(granted, local_dialed)),
                "{what}: the listener dialed its local socket without a grant"
            );
            // Pin the outcome too: a listener that never dials satisfies the
            // invariant vacuously, so the valid run must reach its local socket.
            assert_eq!(
                (granted, local_dialed),
                (what == "valid", what == "valid"),
                "{what}: observed order {steps:?}"
            );
        }
    }

    #[test]
    fn a_denied_capability_never_reaches_the_local_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let ccfg = client_config(cert_fingerprint(TEST_CERT_DER));
        let host_token = EdgeToken::generate();
        let driver_token = EdgeToken::generate(); // WRONG token -> forgery

        // If the capability is denied, `connect_local` must NEVER be called.
        let dialed_local = Arc::new(AtomicBool::new(false));
        let host = std::thread::spawn({
            let scfg = Arc::clone(&scfg);
            let dialed_local = Arc::clone(&dialed_local);
            move || {
                let (tcp, _) = listener.accept().unwrap();
                accept_and_relay(
                    tcp,
                    scfg,
                    |src, op| (src == "driver-1" && op == "drive").then_some(host_token),
                    || {
                        dialed_local.store(true, Ordering::SeqCst);
                        CtlStream::pair().map(|(a, _b)| a)
                    },
                )
            }
        });

        let (drv_local, _drv_client) = CtlStream::pair().unwrap();
        let driver = std::thread::spawn(move || {
            dial_and_relay_pinned(
                addr,
                ccfg,
                "driver-1",
                "drive",
                &driver_token,
                b"",
                drv_local,
                None,
            )
        });

        assert!(
            host.join().unwrap().is_err(),
            "a forged capability is rejected"
        );
        assert!(
            driver.join().unwrap().is_err(),
            "the dialer sees the denial"
        );
        assert!(
            !dialed_local.load(Ordering::SeqCst),
            "a denied capability must NEVER dial the local control socket"
        );
    }

    /// Stand up a loopback-TLS listener that grants `op == "drive"` and echoes its
    /// local socket, then dial it with `dial_and_relay_pinned_inner` carrying `pin`
    /// and an observer that reports `observed` as the remote's live launch nonce
    /// (`None` ⇒ the remote reports no such session). Returns the
    /// DIALER's result — the rebind guard runs BEFORE any relay, so this is enough to
    /// prove accept/reject without driving a verb. The listener side is joined.
    fn dial_with_pin(pin: RemoteEndpoint, observed: Option<&'static str>) -> io::Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let ccfg = client_config(cert_fingerprint(TEST_CERT_DER));
        let token = EdgeToken::generate();

        let (svc_a, svc_b) = CtlStream::pair().unwrap();
        let echo = spawn_echo(svc_b);
        let svc_a = Arc::new(Mutex::new(Some(svc_a)));
        let host = std::thread::spawn({
            let svc_a = Arc::clone(&svc_a);
            move || {
                let (tcp, _) = listener.accept().unwrap();
                accept_and_relay(
                    tcp,
                    scfg,
                    |_s, op| (op == "drive").then_some(token),
                    || Ok(svc_a.lock().unwrap().take().unwrap()),
                )
            }
        });

        let (drv_local, drv_client) = CtlStream::pair().unwrap();
        let driver = std::thread::spawn(move || {
            dial_and_relay_pinned_inner(
                addr,
                ccfg,
                "dial",
                "drive",
                &token,
                b"",
                drv_local,
                HANDSHAKE_TIMEOUT,
                Some(pin),
                None,
                move |_t| Ok(observed.map(str::to_owned)),
            )
        });
        // On a rejected pin the dialer never relays; closing our client end lets the
        // listener's own relay (started once the capability verified) reach EOF and
        // return, so neither thread hangs.
        let res = driver.join().unwrap();
        drop(drv_client);
        let _ = host.join().unwrap();
        echo.join().ok();
        res
    }

    #[test]
    fn a_matching_launch_nonce_pin_passes_and_relays_a_verb() {
        // The observer supplies the MATCHING live nonce (standing in for the
        // roster read, which `the_roster_observer_*` drives for real), so the rebind guard holds
        // and the relay carries a control verb end-to-end exactly as the un-pinned
        // path does.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let ccfg = client_config(cert_fingerprint(TEST_CERT_DER));
        let token = EdgeToken::generate();

        let (svc_a, svc_b) = CtlStream::pair().unwrap();
        let echo = spawn_echo(svc_b);
        let svc_a = Arc::new(Mutex::new(Some(svc_a)));
        let host = std::thread::spawn({
            let svc_a = Arc::clone(&svc_a);
            move || {
                let (tcp, _) = listener.accept().unwrap();
                accept_and_relay(
                    tcp,
                    scfg,
                    |_s, op| (op == "drive").then_some(token),
                    || Ok(svc_a.lock().unwrap().take().unwrap()),
                )
            }
        });

        let pin = RemoteEndpoint {
            host: addr.to_string(),
            sid: "s-1".into(),
            nonce: "nonce-live".into(),
            // The fingerprint half is TLS-enforced; `matches` self-compares it (the
            // observed value provably equals the pin), so only the nonce is at issue.
            fingerprint: "fp".into(),
        };
        let (drv_local, mut drv_client) = CtlStream::pair().unwrap();
        let driver = std::thread::spawn(move || {
            dial_and_relay_pinned_inner(
                addr,
                ccfg,
                "dial",
                "drive",
                &token,
                b"",
                drv_local,
                HANDSHAKE_TIMEOUT,
                Some(pin),
                None,
                move |_t| Ok(Some("nonce-live".to_owned())),
            )
        });

        drv_client.write_all(b"screen\n").unwrap();
        drv_client.flush().unwrap();
        let mut got = [0u8; 7];
        drv_client.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"screen\n", "a matching pin relays end-to-end");

        drv_client.shutdown(std::net::Shutdown::Both).unwrap();
        drop(drv_client);
        host.join().unwrap().unwrap();
        driver.join().unwrap().ok();
        echo.join().ok();
    }

    #[test]
    fn a_mismatching_launch_nonce_pin_is_rejected_before_relay() {
        let pin = RemoteEndpoint {
            host: "ignored".into(),
            sid: "s-1".into(),
            nonce: "nonce-pinned".into(),
            fingerprint: "fp".into(),
        };
        // The remote reports a DIFFERENT live nonce (a relaunched/rebound session):
        // the rebind guard must refuse to relay.
        let res = dial_with_pin(pin, Some("nonce-DIFFERENT"));
        let err = res.expect_err("a mismatching launch nonce must be rejected");
        assert_eq!(
            err.to_string(),
            "session s-1 was relaunched: its launch nonce no longer matches expect_nonce; \
             nothing was relayed"
        );
    }

    #[test]
    fn a_pin_with_no_observable_nonce_fails_closed() {
        // The remote reports no session with the pinned identity (`None`). A
        // configured pin must FAIL CLOSED, never relay unverified — the same posture
        // the production `dial_and_relay_pinned` takes.
        let pin = RemoteEndpoint {
            host: "ignored".into(),
            sid: "s-1".into(),
            nonce: "nonce-pinned".into(),
            fingerprint: "fp".into(),
        };
        let res = dial_with_pin(pin, None);
        let err = res.expect_err("an unverifiable pin must fail closed");
        assert_eq!(
            err.to_string(),
            "the remote has no session s-1; nothing was relayed"
        );
    }

    /// A fake remote control server behind the listener: it answers `sessions
    /// bridge` with `reply` and echoes every other line, so a verb relayed after
    /// the roster read comes back unchanged.
    fn spawn_roster_server(s: CtlStream, reply: &'static str) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            use std::io::BufRead as _;
            let mut w = s.try_clone().unwrap();
            let mut r = std::io::BufReader::new(s);
            let mut line = String::new();
            while r.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
                let out = if line == "sessions bridge\n" {
                    reply
                } else {
                    line.as_str()
                };
                if w.write_all(out.as_bytes())
                    .and_then(|()| w.flush())
                    .is_err()
                {
                    break;
                }
                line.clear();
            }
        })
    }

    /// Dial a loopback-TLS listener fronting [`spawn_roster_server`] through the
    /// PRODUCTION [`dial_and_relay_pinned`] (its real observer), then relay one
    /// verb. `Ok` carries what the driver's local client read back.
    fn dial_roster(pin: RemoteEndpoint, reply: &'static str) -> io::Result<Vec<u8>> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let ccfg = client_config(cert_fingerprint(TEST_CERT_DER));
        let token = EdgeToken::generate();
        let (svc_a, svc_b) = CtlStream::pair().unwrap();
        let server = spawn_roster_server(svc_b, reply);
        let svc_a = Arc::new(Mutex::new(Some(svc_a)));
        let host = std::thread::spawn({
            let svc_a = Arc::clone(&svc_a);
            move || {
                let (tcp, _) = listener.accept().unwrap();
                accept_and_relay(
                    tcp,
                    scfg,
                    |_s, op| (op == "drive").then_some(token),
                    || Ok(svc_a.lock().unwrap().take().unwrap()),
                )
            }
        });
        let (drv_local, mut drv_client) = CtlStream::pair().unwrap();
        let driver = std::thread::spawn(move || {
            dial_and_relay_pinned(
                addr,
                ccfg,
                "dial",
                "drive",
                &token,
                b"",
                drv_local,
                Some(pin),
            )
        });
        drv_client.write_all(b"screen\n").unwrap();
        drv_client.flush().unwrap();
        // A liveness bound, not a measurement: sized for a starved parallel run.
        drv_client
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        let mut got = [0u8; 7];
        let read = drv_client.read_exact(&mut got);
        drv_client.shutdown(std::net::Shutdown::Both).ok();
        drop(drv_client);
        let dialed = driver.join().unwrap();
        let _ = host.join().unwrap();
        drop(svc_a);
        server.join().ok();
        match (read, dialed) {
            (Ok(()), _) => Ok(got.to_vec()),
            (Err(_), Err(e)) => Err(e),
            (Err(e), Ok(())) => Err(e),
        }
    }

    const ROSTER: &str = "OK 2\n1 s-a nonce=aaaa\n2 s-b nonce=bbbb\n";

    /// THE LAUNCH-NONCE PIN, observed over the real wire: the dialer asks the
    /// remote's `sessions bridge` (the listener has already authenticated its
    /// control socket at Owner scope) and compares the pinned session's live
    /// nonce. The roster exchange never reaches the
    /// driver's own client: the verb it sends after comes back alone.
    #[test]
    fn the_roster_observer_passes_a_live_pin_and_refuses_a_stale_one() {
        let pin = |sid: &str, nonce: &str| RemoteEndpoint {
            host: "loopback".into(),
            sid: sid.into(),
            nonce: nonce.into(),
            fingerprint: "fp".into(),
        };
        let got = dial_roster(pin("s-b", "bbbb"), ROSTER).expect("the pinned session is live");
        assert_eq!(
            got, b"screen\n",
            "the relay carries the verb, not the roster"
        );
        // A pin without a sid holds while any session carries the nonce.
        assert_eq!(
            dial_roster(pin("", "aaaa"), ROSTER).expect("nonce-only pin"),
            b"screen\n"
        );
        // A relaunched session: its sid answers with a fresh nonce.
        let err = dial_roster(pin("s-b", "old0"), ROSTER).expect_err("a stale nonce");
        assert!(
            err.to_string().starts_with("session s-b was relaunched"),
            "{err}"
        );
        // A closed session: the remote reports no such sid, or no such nonce —
        // each said as the case it is.
        let err = dial_roster(pin("s-gone", "bbbb"), ROSTER).expect_err("an unreported sid");
        assert_eq!(
            err.to_string(),
            "the remote has no session s-gone; nothing was relayed"
        );
        let err = dial_roster(pin("", "old0"), ROSTER).expect_err("an unreported nonce");
        assert_eq!(
            err.to_string(),
            "no remote session carries expect_nonce (relaunched or closed); nothing was relayed"
        );
        // A remote that refuses the roster fails closed too.
        let err = dial_roster(pin("s-b", "bbbb"), "ERR scope\n").expect_err("a refused roster");
        assert_eq!(
            err.to_string(),
            "the remote refused `sessions bridge` (\"ERR scope\"), so expect_nonce cannot be \
             checked; nothing was relayed"
        );
    }

    /// THE LISTENER'S TRAIL names who connected and when: `serve` reports an
    /// admitted dialer as it starts (not only when its relay ends), each event
    /// carries the dialer's address, and a refused one says why in words — here a
    /// wrong drive token, whose dialer hears a typed refusal.
    #[test]
    fn serve_names_each_peer_and_says_why_one_was_turned_away() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let token = EdgeToken::generate();
        let running = Arc::new(AtomicBool::new(true));
        let (tx, rx) = std::sync::mpsc::channel::<NetEvent>();
        let server = std::thread::spawn({
            let running = Arc::clone(&running);
            move || {
                serve(
                    &listener,
                    &scfg,
                    move |_src, op| (op == "drive").then_some(token),
                    || {
                        let (a, b) = CtlStream::pair()?;
                        spawn_echo(b);
                        Ok(a)
                    },
                    &running,
                    move |ev| {
                        let _ = tx.send(ev);
                    },
                );
            }
        });
        // A liveness bound, not a measurement.
        let next = || {
            rx.recv_timeout(Duration::from_secs(60))
                .expect("the listener reports the connection")
        };

        // A dialer holding the right token: admitted while it is still connected.
        let (drv_local, drv_client) = CtlStream::pair().unwrap();
        let driver = std::thread::spawn(move || {
            dial_and_relay_pinned(
                addr,
                client_config(cert_fingerprint(TEST_CERT_DER)),
                "dial",
                "drive",
                &token,
                b"",
                drv_local,
                None,
            )
        });
        let NetEvent::Admitted { peer, .. } = next() else {
            panic!("the first event is the admission");
        };
        assert_eq!(peer.ip(), addr.ip(), "the event names the dialer's address");
        drv_client.shutdown(std::net::Shutdown::Both).unwrap();
        drop(drv_client);
        driver.join().unwrap().ok();
        match next() {
            NetEvent::Closed { peer: closed, .. } => assert_eq!(closed, peer),
            other => panic!("the relay's end is reported: {other:?}"),
        }

        // A dialer holding a wrong token: refused, and each end says so.
        let (drv_local, _drv_client) = CtlStream::pair().unwrap();
        let refused = dial_and_relay_pinned(
            addr,
            client_config(cert_fingerprint(TEST_CERT_DER)),
            "dial",
            "drive",
            &EdgeToken::generate(),
            b"",
            drv_local,
            None,
        )
        .expect_err("a wrong token is refused");
        assert!(crate::is_token_refused(&refused), "{refused}");
        match next() {
            NetEvent::Refused { peer, reason } => {
                assert_eq!(peer.ip(), addr.ip());
                assert_eq!(reason, "wrong drive token");
            }
            other => panic!("the refusal is reported: {other:?}"),
        }

        running.store(false, Ordering::Relaxed);
        server.join().unwrap();
    }

    /// An admitted dialer whose relay cannot start because THIS instance's
    /// control socket is gone: the close names the local socket, never reads as
    /// the peer's failure.
    #[test]
    fn an_unreachable_local_socket_is_said_as_this_instances() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let token = EdgeToken::generate();
        let running = Arc::new(AtomicBool::new(true));
        let (tx, rx) = std::sync::mpsc::channel::<NetEvent>();
        let server = std::thread::spawn({
            let running = Arc::clone(&running);
            move || {
                serve(
                    &listener,
                    &scfg,
                    move |_src, op| (op == "drive").then_some(token),
                    || {
                        Err(io::Error::new(
                            io::ErrorKind::NotFound,
                            "No such file or directory (os error 2)",
                        ))
                    },
                    &running,
                    move |ev| {
                        let _ = tx.send(ev);
                    },
                );
            }
        });
        // A liveness bound, not a measurement.
        let next = || {
            rx.recv_timeout(Duration::from_secs(60))
                .expect("the listener reports the connection")
        };
        let (drv_local, drv_client) = CtlStream::pair().unwrap();
        let driver = std::thread::spawn(move || {
            dial_and_relay_pinned(
                addr,
                client_config(cert_fingerprint(TEST_CERT_DER)),
                "dial",
                "drive",
                &token,
                b"",
                drv_local,
                None,
            )
        });
        assert!(matches!(next(), NetEvent::Admitted { .. }));
        match next() {
            NetEvent::Closed {
                error: Some(error), ..
            } => assert_eq!(
                error,
                "could not reach this aterm's control socket: No such file or directory (os \
                 error 2)"
            ),
            other => panic!("the failed relay is reported: {other:?}"),
        }
        // The failed relay may already have dropped its end of the pair, and a
        // shutdown then answers ENOTCONN.
        drv_client.shutdown(std::net::Shutdown::Both).ok();
        drop(drv_client);
        driver.join().unwrap().ok();
        running.store(false, Ordering::Relaxed);
        server.join().unwrap();
    }

    /// What a [`serve`] under test said, counted as it says it.
    #[derive(Default)]
    struct Heard {
        /// Newcomers turned away with every slot held.
        at_capacity: AtomicUsize,
        /// Unauthenticated connections whose slot went to a newcomer.
        displaced: AtomicUsize,
        /// Dialers admitted.
        admitted: AtomicUsize,
        /// Why each connection from ::1 was turned away, in order: the
        /// dual-stack tests' token holder dials from there, and nothing else.
        refused_v6: Mutex<Vec<String>>,
    }

    /// [`serve`] on `listener` with the test certificate, granting `token` the
    /// drive op and relaying each admitted dialer to an echo.
    fn serve_counting(
        listener: TcpListener,
        token: EdgeToken,
        running: &Arc<AtomicBool>,
    ) -> (Arc<Heard>, std::thread::JoinHandle<()>) {
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let heard = Arc::new(Heard::default());
        let server = std::thread::spawn({
            let running = Arc::clone(running);
            let heard = Arc::clone(&heard);
            move || {
                serve(
                    &listener,
                    &scfg,
                    move |_src, op| (op == "drive").then_some(token),
                    || {
                        let (a, b) = CtlStream::pair()?;
                        spawn_echo(b);
                        Ok(a)
                    },
                    &running,
                    move |ev| match ev {
                        NetEvent::Refused { peer, reason } => {
                            if reason == "the listener is at capacity" {
                                heard.at_capacity.fetch_add(1, Ordering::SeqCst);
                            } else if reason == DISPLACED {
                                heard.displaced.fetch_add(1, Ordering::SeqCst);
                            }
                            if peer.ip() == IpAddr::V6(Ipv6Addr::LOCALHOST) {
                                heard.refused_v6.lock().unwrap().push(reason);
                            }
                        }
                        NetEvent::Admitted { .. } => {
                            heard.admitted.fetch_add(1, Ordering::SeqCst);
                        }
                        _ => {}
                    },
                );
            }
        });
        (heard, server)
    }

    /// A token holder's dial: TCP, the pinned TLS handshake and the
    /// channel-bound capability, read and write bounded by a minute's hang
    /// detector. `Ok` is the admitted connection, whose relay lives until it is
    /// dropped.
    fn token_holder(
        addr: SocketAddr,
        token: &EdgeToken,
    ) -> io::Result<TlsTransport<rustls::ClientConnection>> {
        token_holder_after(addr, token, Duration::ZERO)
    }

    /// [`token_holder`], waiting `late` between its TCP connect and its
    /// ClientHello: a handshake over a slow or lossy path.
    fn token_holder_after(
        addr: SocketAddr,
        token: &EdgeToken,
        late: Duration,
    ) -> io::Result<TlsTransport<rustls::ClientConnection>> {
        let tcp = TcpStream::connect_timeout(&addr, MUST_HAPPEN)?;
        tcp.set_read_timeout(Some(MUST_HAPPEN))?;
        tcp.set_write_timeout(Some(MUST_HAPPEN))?;
        std::thread::sleep(late);
        let mut transport = tls::connect(
            tcp,
            tls::fixed_server_name(),
            client_config(cert_fingerprint(TEST_CERT_DER)),
        )?;
        let exporter = transport.exporter().to_vec();
        present_capability(transport.stream(), &exporter, "dial", "drive", token)?;
        Ok(transport)
    }

    /// One line through an admitted connection's relay, answered by the echo.
    fn echoes(conn: &mut TlsTransport<rustls::ClientConnection>) -> bool {
        let mut back = [0u8; 5];
        conn.stream().write_all(b"ping\n").is_ok()
            && conn.stream().flush().is_ok()
            && conn.stream().read_exact(&mut back).is_ok()
            && &back == b"ping\n"
    }

    /// [`MAX_INFLIGHT`] idle sockets to one address, each sending nothing and
    /// reconnected the moment the listener closes it, until dropped. Stops
    /// them however the test ends, so a failure does not leave them hammering
    /// the rest of the binary.
    struct IdleSockets {
        stop: Arc<AtomicBool>,
        threads: Vec<std::thread::JoinHandle<()>>,
    }

    impl IdleSockets {
        fn hold(target: SocketAddr) -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let threads = (0..MAX_INFLIGHT)
                .map(|_| {
                    let stop = Arc::clone(&stop);
                    std::thread::spawn(move || {
                        while !stop.load(Ordering::SeqCst) {
                            let Ok(mut idle) = TcpStream::connect_timeout(&target, MUST_HAPPEN)
                            else {
                                std::thread::sleep(Duration::from_millis(5));
                                continue;
                            };
                            let _ = idle.set_read_timeout(Some(Duration::from_millis(50)));
                            let mut byte = [0u8; 1];
                            while !stop.load(Ordering::SeqCst) {
                                match idle.read(&mut byte) {
                                    Ok(0) => break,
                                    Err(e) if !is_deadline(&e) => break,
                                    Ok(_) | Err(_) => {}
                                }
                            }
                        }
                    })
                })
                .collect();
            Self { stop, threads }
        }
    }

    impl Drop for IdleSockets {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            for thread in self.threads.drain(..) {
                let _ = thread.join();
            }
        }
    }

    /// Once the listener turned a connection from `target` away at capacity,
    /// idle sockets hold every slot: the attack is in force.
    fn until_turned_away(heard: &Heard, target: SocketAddr) {
        let started = Instant::now();
        while heard.at_capacity.load(Ordering::SeqCst) == 0 {
            assert!(
                started.elapsed() < MUST_HAPPEN,
                "the idle sockets never filled the listener"
            );
            drop(TcpStream::connect_timeout(&target, MUST_HAPPEN));
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A listener on `[::]` that also takes IPv4, and the port it took. It
    /// sees a 127.0.0.1 peer as `::ffff:127.0.0.1` ([`source_of`]: 127.0.0.1)
    /// and a ::1 peer as `::/64`: two addresses on loopback with no second
    /// one configured, which a listener on 127.0.0.1 cannot see (lo0 carries
    /// no other usable IPv4 address without an alias, which needs root).
    /// `None`, said, on a host whose loopback has no IPv6 or whose `[::]`
    /// listener is IPv6-only by default (Windows): the test cannot run there.
    fn dual_stack_listener(test: &str) -> Option<(TcpListener, u16)> {
        let not_run = |why: String| {
            eprintln!("{test}: NOT RUN, no dual-stack loopback here ({why})");
            None
        };
        let listener = match TcpListener::bind("[::]:0") {
            Ok(listener) => listener,
            Err(e) => return not_run(format!("binding [::]: {e}")),
        };
        let port = listener.local_addr().unwrap().port();
        let mut sources = Vec::new();
        for ip in [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ] {
            let probe = match TcpStream::connect_timeout(&SocketAddr::new(ip, port), MUST_HAPPEN) {
                Ok(probe) => probe,
                Err(e) => return not_run(format!("{ip} cannot reach [::]: {e}")),
            };
            // The connect completed, so the backlog holds it: no wait.
            let (accepted, peer) = listener.accept().unwrap();
            drop((probe, accepted));
            sources.push(source_of(peer.ip()));
        }
        assert_ne!(
            sources[0], sources[1],
            "127.0.0.1 and ::1 are two addresses"
        );
        Some((listener, port))
    }

    /// NET-1 (r13, 2026-10-10): a peer holding every slot with idle TCP
    /// sockets — no TLS byte, each reconnected the moment the listener closes
    /// it — must not lock a token holder out. Refusing the NEWEST connection at
    /// capacity did: every slot stayed with a socket that never authenticated,
    /// renewed each time its handshake deadline cut it, and the token holder was
    /// turned away for as long as the sockets kept coming.
    ///
    /// The attack is in force before the token holder dials (a connection was
    /// turned away at capacity), and the dial must then be admitted within a
    /// minute's hang detector — by taking an idle socket's slot, which the
    /// listener reports — and relay. All peers share 127.0.0.1, so this is the
    /// same-address path: rule 2, once an idle socket is [`PREAUTH_GRACE`] old.
    /// A dialer from another address is the next test's.
    #[test]
    fn idle_unauthenticated_sockets_never_lock_a_token_holder_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let token = EdgeToken::generate();
        let running = Arc::new(AtomicBool::new(true));
        let (heard, server) = serve_counting(listener, token, &running);

        // Control: with no attacker the token holder is admitted and relays.
        let mut control = token_holder(addr, &token).expect("an unloaded listener admits");
        assert!(echoes(&mut control), "the control's relay answers");
        drop(control);

        let idle = IdleSockets::hold(addr);
        until_turned_away(&heard, addr);

        let started = Instant::now();
        let mut admitted = loop {
            match token_holder(addr, &token) {
                Ok(conn) => break conn,
                Err(e) => {
                    assert!(
                        started.elapsed() < MUST_HAPPEN,
                        "{MAX_INFLIGHT} idle sockets locked the token holder out for \
                         {MUST_HAPPEN:?} (last refusal: {e}; {} turned away at capacity, \
                         {} admitted)",
                        heard.at_capacity.load(Ordering::SeqCst),
                        heard.admitted.load(Ordering::SeqCst),
                    );
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        };
        assert!(
            echoes(&mut admitted),
            "the admitted token holder's relay answers"
        );
        assert!(
            heard.displaced.load(Ordering::SeqCst) >= 1,
            "the token holder got in by taking an idle socket's slot, and the \
             listener said so"
        );

        drop(idle);
        drop(admitted);
        running.store(false, Ordering::Relaxed);
        server.join().unwrap();
    }

    /// NET-1 across addresses (r13 review, 2026-10-10): idle sockets from one
    /// address — every slot held, each reconnected the moment the listener
    /// closes it — never turn away a token holder from another. Rule 1 lets
    /// it in on its FIRST dial, with no retry and no wait for an idle socket
    /// to age past [`PREAUTH_GRACE`] (rule 2, which this dial comes too early
    /// for: the attack is milliseconds old). Refusing the newest connection
    /// turned it away for as long as the sockets kept coming.
    #[test]
    fn idle_sockets_from_one_address_never_turn_away_a_dialer_from_another() {
        let test = "idle_sockets_from_one_address_never_turn_away_a_dialer_from_another";
        let Some((listener, port)) = dual_stack_listener(test) else {
            return;
        };
        let token = EdgeToken::generate();
        let running = Arc::new(AtomicBool::new(true));
        let (heard, server) = serve_counting(listener, token, &running);
        let attacker = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let dialer = SocketAddr::from((Ipv6Addr::LOCALHOST, port));

        let idle = IdleSockets::hold(attacker);
        until_turned_away(&heard, attacker);

        let mut admitted = token_holder(dialer, &token).unwrap_or_else(|e| {
            panic!(
                "idle sockets from 127.0.0.1 turned the token holder from ::1 away: {e} \
                 (the listener said {:?}; {} turned away at capacity, {} displaced)",
                heard.refused_v6.lock().unwrap(),
                heard.at_capacity.load(Ordering::SeqCst),
                heard.displaced.load(Ordering::SeqCst),
            )
        });
        assert!(
            echoes(&mut admitted),
            "the admitted token holder's relay answers"
        );
        assert!(
            heard.displaced.load(Ordering::SeqCst) >= 1,
            "the token holder got in by taking an idle socket's slot, and the \
             listener said so"
        );

        drop(idle);
        drop(admitted);
        running.store(false, Ordering::Relaxed);
        server.join().unwrap();
    }

    /// Rule 2's address bound (r13 review, 2026-10-10): a token holder from
    /// another address whose handshake outlasts [`PREAUTH_GRACE`] — a lossy or
    /// long path, here a ClientHello sent `PREAUTH_GRACE` + 500 ms after its
    /// connect — is never cut for the idle sockets' own reconnects. Without
    /// the bound, the next idle socket from 127.0.0.1 took its slot the moment
    /// it turned `PREAUTH_GRACE` old, and the handshake met a closed socket.
    #[test]
    fn a_slow_handshake_is_never_cut_for_a_heavier_address() {
        let test = "a_slow_handshake_is_never_cut_for_a_heavier_address";
        let Some((listener, port)) = dual_stack_listener(test) else {
            return;
        };
        let token = EdgeToken::generate();
        let running = Arc::new(AtomicBool::new(true));
        let (heard, server) = serve_counting(listener, token, &running);
        let attacker = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let dialer = SocketAddr::from((Ipv6Addr::LOCALHOST, port));

        let idle = IdleSockets::hold(attacker);
        until_turned_away(&heard, attacker);

        let late = PREAUTH_GRACE + Duration::from_millis(500);
        let mut admitted = token_holder_after(dialer, &token, late).unwrap_or_else(|e| {
            panic!(
                "a handshake from ::1 sent {late:?} after its connect was cut while idle \
                 sockets from 127.0.0.1 held the other slots: {e} (the listener said {:?})",
                heard.refused_v6.lock().unwrap(),
            )
        });
        assert!(
            echoes(&mut admitted),
            "the admitted token holder's relay answers"
        );
        assert!(
            heard.refused_v6.lock().unwrap().is_empty(),
            "nothing from ::1 was turned away"
        );

        drop(idle);
        drop(admitted);
        running.store(false, Ordering::Relaxed);
        server.join().unwrap();
    }

    /// [`serve`]'s rule, case by case, on synthetic addresses. The real-socket
    /// tests reach two sources through a dual-stack listener (127.0.0.1 and
    /// ::1), which is enough for rule 1 and rule 2's address bound end to end;
    /// the margin of two, ties, relays and ages are pinned here, where any
    /// number of addresses is free. Slot `i` was taken `i` ms after `t0`, so
    /// lower is older.
    #[test]
    fn a_newcomer_takes_only_an_unauthenticated_slot_by_the_rule() {
        let t0 = Instant::now();
        let a: IpAddr = "198.51.100.7".parse().unwrap();
        let b: IpAddr = "203.0.113.9".parse().unwrap();
        let c: IpAddr = "192.0.2.1".parse().unwrap();
        let slots = |of: &[(IpAddr, bool)]| -> Vec<Held> {
            (0u64..)
                .zip(of)
                .map(|(i, &(source, unauthenticated))| Held {
                    source,
                    since: t0 + Duration::from_millis(i),
                    unauthenticated,
                })
                .collect()
        };
        let fresh = t0 + Duration::from_millis(500);
        let grown = t0 + PREAUTH_GRACE + Duration::from_millis(500);

        // One address holds every slot, unauthenticated and fresh.
        let crowded = slots(&[(a, true); MAX_INFLIGHT]);
        // Rule 1: another address takes the crowding address's oldest, at
        // any age, however fast that address reconnects.
        assert_eq!(pick_displaced(&crowded, b, fresh), Some(0));
        // The crowding address itself waits out PREAUTH_GRACE (rule 2) ...
        assert_eq!(pick_displaced(&crowded, a, fresh), None);
        // ... and then takes its own oldest.
        assert_eq!(pick_displaced(&crowded, a, grown), Some(0));

        // The margin of two: a holds 2, b holds 1, the rest are relays. b
        // cannot take a's slot (it would only swap who is heavier); c can.
        let mut mixed = vec![(c, false); MAX_INFLIGHT - 3];
        mixed.extend([(b, true), (a, true), (a, true)]);
        let mixed = slots(&mixed);
        assert_eq!(pick_displaced(&mixed, b, fresh), None);
        assert_eq!(pick_displaced(&mixed, c, fresh), Some(MAX_INFLIGHT - 2));
        // Rule 2 takes the oldest unauthenticated one, never an older relay.
        assert_eq!(pick_displaced(&mixed, b, grown), Some(MAX_INFLIGHT - 3));

        // Two addresses tie for most: the oldest of either.
        let tied = slots(&[(b, true), (a, true), (a, true), (b, true)]);
        assert_eq!(pick_displaced(&tied, c, fresh), Some(0));

        // Every slot an admitted relay, however old: nothing to take.
        let relays = slots(&[(a, false); MAX_INFLIGHT]);
        for newcomer in [a, b, c] {
            assert_eq!(pick_displaced(&relays, newcomer, grown), None);
        }

        // Rule 2's address bound (2026-10-10, on review): b's one handshake
        // is older than PREAUTH_GRACE, a's are fresh and fill every other
        // slot. a's own newcomer may not cut b's slow handshake, however old
        // it is: it waits for one of a's to age, and then takes that one.
        let mut slow = vec![Held {
            source: b,
            since: t0,
            unauthenticated: true,
        }];
        slow.extend((1u64..).take(MAX_INFLIGHT - 1).map(|i| Held {
            source: a,
            since: t0 + PREAUTH_GRACE + Duration::from_millis(i),
            unauthenticated: true,
        }));
        let b_aged = t0 + PREAUTH_GRACE + Duration::from_millis(500);
        assert_eq!(pick_displaced(&slow, a, b_aged), None);
        let all_aged = t0 + PREAUTH_GRACE * 2 + Duration::from_millis(500);
        assert_eq!(pick_displaced(&slow, a, all_aged), Some(1));
        // A newcomer from an address as light as b's may take it, once old.
        assert_eq!(pick_displaced(&slow, c, b_aged), Some(1), "rule 1 first");
        let mut even = slow.clone();
        even.truncate(2);
        even.push(Held {
            source: c,
            since: t0 + PREAUTH_GRACE,
            unauthenticated: true,
        });
        assert_eq!(pick_displaced(&even, c, b_aged), Some(0));
    }

    #[test]
    fn an_address_is_an_ipv4_address_or_an_ipv6_slash_64() {
        let ip = |s: &str| -> IpAddr { s.parse().unwrap() };
        assert_eq!(
            source_of(ip("2001:db8:1:2::1")),
            source_of(ip("2001:db8:1:2:ffff:ffff:ffff:ffff")),
            "one /64 is one address, however many hosts it mints"
        );
        assert_ne!(
            source_of(ip("2001:db8:1:2::1")),
            source_of(ip("2001:db8:1:3::1"))
        );
        assert_eq!(
            source_of(ip("::ffff:198.51.100.7")),
            ip("198.51.100.7"),
            "an IPv4-mapped peer counts as its IPv4 address"
        );
        assert_ne!(source_of(ip("198.51.100.7")), source_of(ip("198.51.100.8")));
    }

    /// The listener's claim on a connection ([`Evictor::evict`]) cuts one that
    /// has not authenticated — at once, not at its deadline — and never one
    /// whose handshake claimed it first.
    #[test]
    fn an_eviction_cuts_only_a_connection_that_has_not_authenticated() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let pair = || {
            let accepting = {
                let listener = listener.try_clone().unwrap();
                std::thread::spawn(move || listener.accept().unwrap().0)
            };
            let peer = TcpStream::connect(addr).unwrap();
            let tcp = accepting.join().unwrap();
            tcp.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
            (tcp, peer)
        };

        // Unauthenticated: the eviction wins and the socket is cut at once,
        // inside a deadline far longer than the hang detector.
        let (tcp, _peer) = pair();
        let watchdog = HandshakeWatchdog::arm(&tcp, MUST_HAPPEN * 5).unwrap();
        let evictor = watchdog.evictor();
        assert!(evictor.unauthenticated());
        let started = Instant::now();
        assert!(evictor.evict());
        let mut byte = [0u8; 1];
        assert!(
            matches!((&tcp).read(&mut byte), Ok(0) | Err(_)),
            "the evicted socket is force-closed"
        );
        assert!(
            started.elapsed() < MUST_HAPPEN,
            "cut at once, not at the deadline"
        );
        assert!(!evictor.unauthenticated());
        assert!(!evictor.evict(), "a connection is evicted once");
        assert_eq!(watchdog.finish(), Claim::Evicted);

        // Authenticated first: the eviction loses and the socket stays open.
        let (tcp, mut peer) = pair();
        let watchdog = HandshakeWatchdog::arm(&tcp, MUST_HAPPEN * 5).unwrap();
        let evictor = watchdog.evictor();
        assert_eq!(watchdog.finish(), Claim::Authed);
        assert!(!evictor.unauthenticated());
        assert!(
            !evictor.evict(),
            "an authenticated connection is never evicted"
        );
        peer.write_all(b"x").unwrap();
        assert_eq!(
            (&tcp).read(&mut byte).unwrap(),
            1,
            "and its socket still carries bytes"
        );
    }

    /// CONFORMANCE (Tier-1) of the REAL listener slots to
    /// `aterm_spec::derive::net_slot_displacement_model` (`NetSlotDisplacement`)
    /// at the production capacity (`Cap` overridden to [`MAX_INFLIGHT`]).
    ///
    /// The test plays both halves of [`serve`] over real loopback sockets: its
    /// accept loop (`Slots::make_room`, [`arm_unauthenticated`]'s watchdog,
    /// `Slots::take`) and each connection's handler thread (the handshake's
    /// [`HandshakeWatchdog::finish`], then dropping the slot). After every
    /// step it projects the real state onto the model — each held slot's
    /// watchdog claim (`pending`, `authed`, `cut`), whether a newcomer's
    /// `make_room` is still unresolved (`waiting`), the connections given a
    /// slot and the claims they show, and whether an authenticated
    /// connection's socket was closed under it (read at the dialer's end) —
    /// and validates the transition against the model, then its invariants.
    /// Steps: fill every slot; authenticate one; evict and refuse (no slot
    /// freed within `ACCEPT_POLL`); a deadline; evict and admit (the slot
    /// freed while the newcomer waits); a relay's close.
    ///
    /// NEGATIVE CONTROL: the real [`Evictor::evict`] on the authenticated
    /// connection loses its claim and changes nothing, and the step a store
    /// would have made there (`authed` to `cut`) is one the committed model
    /// rejects. Make `evict` store `WD_EVICTED` instead of its
    /// compare-and-swap and the projection moves: this test fails.
    #[test]
    fn serve_slots_conform_to_net_slot_displacement() {
        use aterm_spec::derive::net_slot_displacement_model;
        use aterm_spec::interp::with_consts;
        use aterm_spec::verify::validate_transition_tiered;
        use std::collections::BTreeMap;

        type State = BTreeMap<&'static str, i64>;

        /// One real connection: the listener's end (kept open, so only the
        /// watchdog ever closes it), the dialer's end, the watchdog's claim,
        /// and the slot its handler holds.
        struct Conn {
            _tcp: TcpStream,
            peer: TcpStream,
            evictor: Evictor,
            watchdog: Option<HandshakeWatchdog>,
            slot: Option<SlotTenure>,
        }
        impl Conn {
            fn claim(&self) -> u8 {
                self.evictor.state.load(Ordering::Acquire)
            }
            /// The dialer's end reads the listener's close.
            fn closed(&self) -> bool {
                self.peer.set_nonblocking(true).unwrap();
                let mut byte = [0u8; 1];
                let closed = match (&self.peer).read(&mut byte) {
                    Ok(0) => true,
                    Ok(_) => false,
                    Err(e) => !is_deadline(&e),
                };
                self.peer.set_nonblocking(false).unwrap();
                closed
            }
        }

        let n = |count: usize| i64::try_from(count).unwrap();
        let project = |slots: &Slots, conns: &[Conn], waiting: bool| -> State {
            let held = slots.lock();
            let holding = |claims: &[u8]| {
                n(held
                    .iter()
                    .filter(|slot| claims.contains(&slot.phase.state.load(Ordering::Acquire)))
                    .count())
            };
            BTreeMap::from([
                ("pending", holding(&[WD_RUNNING])),
                ("authed", holding(&[WD_AUTHED])),
                ("cut", holding(&[WD_FIRED, WD_EVICTED])),
                ("waiting", i64::from(waiting)),
                ("arrivals", n(conns.len())),
                (
                    "claims",
                    n(conns.iter().filter(|c| c.claim() != WD_RUNNING).count()),
                ),
                (
                    "authed_cut",
                    i64::from(conns.iter().any(|c| c.claim() == WD_AUTHED && c.closed())),
                ),
            ])
        };

        let model = net_slot_displacement_model();
        let cap = n(MAX_INFLIGHT);
        let overrides = [("Cap", cap), ("Arrivals", 2 * cap)];
        let at_scale = with_consts(&model, &overrides);
        let check = |seen: &mut State, action: &str, now: State| {
            let (ok, why) = validate_transition_tiered(
                &model,
                &overrides,
                seen,
                &now,
                Some(action),
                "NetSlotDisplacement(serve)",
            );
            assert!(
                ok,
                "the real listener took `{action}` from {seen:?} to {now:?}, which the model \
                 does not admit — {why}"
            );
            for invariant in &at_scale.invariants {
                assert!(
                    at_scale.check_invariant(invariant.name, &now),
                    "{} broken after `{action}`: {now:?}",
                    invariant.name
                );
            }
            *seen = now;
        };

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let a: IpAddr = "198.51.100.7".parse().unwrap();
        let b: IpAddr = "203.0.113.9".parse().unwrap();
        let slots = Arc::new(Slots::default());
        let mut conns: Vec<Conn> = Vec::new();
        // The accept loop: a slot for a connection from `source`, armed with
        // `deadline` (the others never reach theirs in this test).
        let accept = |conns: &mut Vec<Conn>, source: IpAddr, deadline: Duration| {
            assert!(slots.make_room(source), "a slot is free");
            let peer = TcpStream::connect(addr).unwrap();
            let (tcp, _) = listener.accept().unwrap();
            let watchdog = arm_unauthenticated(&tcp, deadline).unwrap();
            let evictor = watchdog.evictor();
            let id = u64::try_from(conns.len()).unwrap();
            let slot = slots.take(id, source, evictor.clone());
            conns.push(Conn {
                _tcp: tcp,
                peer,
                evictor,
                watchdog: Some(watchdog),
                slot: Some(slot),
            });
        };
        let never = MUST_HAPPEN * 5;
        let until = |what: &str, done: &dyn Fn() -> bool| {
            let started = Instant::now();
            while !done() {
                assert!(started.elapsed() < MUST_HAPPEN, "{what} never happened");
                std::thread::sleep(Duration::from_millis(1));
            }
        };

        let mut seen = project(&slots, &conns, false);
        assert_eq!(seen, model.init_state(), "the listener starts empty");

        // Every slot taken, from one address.
        for _ in 0..MAX_INFLIGHT {
            accept(&mut conns, a, never);
            check(&mut seen, "Accept", project(&slots, &conns, false));
        }

        // The first connection authenticates: its handshake wins the claim.
        assert_eq!(conns[0].watchdog.take().unwrap().finish(), Claim::Authed);
        check(&mut seen, "Authenticate", project(&slots, &conns, false));

        // NEGATIVE CONTROL: the listener's claim on the relay loses and
        // changes nothing. Had it moved anything, that move must still be a
        // step of the committed model, and a store over the relay's claim
        // (`authed` to `cut`) is none.
        let claimed = conns[0].evictor.evict();
        let after = project(&slots, &conns, false);
        if after != seen {
            let (ok, why) = validate_transition_tiered(&model, &overrides, &seen, &after, None, "");
            assert!(
                ok,
                "the listener's claim on a relay moved {seen:?} to {after:?}, which the \
                 model does not admit — {why}"
            );
        }
        assert!(!claimed, "an admitted relay is never evicted");
        assert_eq!(after, seen, "and nothing moved");

        // A newcomer from another address evicts the oldest connection that
        // has not authenticated; no slot frees within ACCEPT_POLL, so it is
        // refused; then the evicted connection's handler lets go.
        let newcomer = std::thread::spawn({
            let slots = Arc::clone(&slots);
            move || slots.make_room(b)
        });
        until("the eviction", &|| conns[1].claim() == WD_EVICTED);
        check(&mut seen, "Evict", project(&slots, &conns, true));
        assert!(!newcomer.join().unwrap(), "no slot freed: refused");
        check(&mut seen, "Refuse", project(&slots, &conns, false));
        assert_eq!(conns[1].watchdog.take().unwrap().finish(), Claim::Evicted);
        until("the eviction's close at the dialer", &|| conns[1].closed());
        conns[1].slot = None;
        check(&mut seen, "Release", project(&slots, &conns, false));

        // A connection that never authenticates meets its deadline.
        let late = conns.len();
        accept(&mut conns, a, Duration::from_millis(250));
        let now = project(&slots, &conns, false);
        if conns[late].claim() == WD_FIRED && now["cut"] > seen["cut"] {
            // The deadline beat this projection (a stalled host): the
            // connection was pending when its slot was taken.
            let mut taken = now.clone();
            for (var, by) in [("pending", 1), ("cut", -1), ("claims", -1)] {
                *taken.get_mut(var).unwrap() += by;
            }
            check(&mut seen, "Accept", taken);
        } else {
            check(&mut seen, "Accept", now);
            until("the deadline", &|| conns[late].claim() == WD_FIRED);
        }
        check(&mut seen, "Deadline", project(&slots, &conns, false));
        assert_eq!(
            conns[late].watchdog.take().unwrap().finish(),
            Claim::Deadline
        );
        until("the deadline's close at the dialer", &|| {
            conns[late].closed()
        });
        conns[late].slot = None;
        check(&mut seen, "Release", project(&slots, &conns, false));
        accept(&mut conns, a, never);
        check(&mut seen, "Accept", project(&slots, &conns, false));

        // Evict again, and free the slot while the newcomer waits: it takes
        // the slot (or, past ACCEPT_POLL, is refused and takes it after).
        let newcomer = std::thread::spawn({
            let slots = Arc::clone(&slots);
            move || slots.make_room(b)
        });
        until("the eviction", &|| conns[2].claim() == WD_EVICTED);
        check(&mut seen, "Evict", project(&slots, &conns, true));
        assert_eq!(conns[2].watchdog.take().unwrap().finish(), Claim::Evicted);
        conns[2].slot = None;
        check(&mut seen, "Release", project(&slots, &conns, true));
        if !newcomer.join().unwrap() {
            check(&mut seen, "Refuse", project(&slots, &conns, false));
        }
        accept(&mut conns, b, never);
        check(&mut seen, "Accept", project(&slots, &conns, false));

        // The relay ends.
        conns[0].slot = None;
        check(&mut seen, "Close", project(&slots, &conns, false));
        assert!(!conns[0].closed(), "nothing ever closed the relay's socket");

        // Cut the rest, so no watchdog outlives the test.
        for conn in &mut conns {
            if let Some(watchdog) = conn.watchdog.take() {
                conn.evictor.evict();
                watchdog.finish();
            }
        }
    }

    /// The control for the rule above: ADMITTED relays still fill the listener
    /// where they did — [`MAX_INFLIGHT`] token holders hold every slot, a
    /// newcomer is turned away at capacity even after every relay is older than
    /// [`PREAUTH_GRACE`], and no relay is displaced to make room for it.
    #[test]
    fn admitted_relays_still_fill_the_listener_and_are_never_displaced() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let token = EdgeToken::generate();
        let running = Arc::new(AtomicBool::new(true));
        let (heard, server) = serve_counting(listener, token, &running);

        let mut relays: Vec<_> = (0..MAX_INFLIGHT)
            .map(|i| token_holder(addr, &token).unwrap_or_else(|e| panic!("relay {i}: {e}")))
            .collect();
        // Older than the grace an unauthenticated connection gets, so the age
        // rule would take a relay's slot if relays were displaceable.
        std::thread::sleep(PREAUTH_GRACE + Duration::from_millis(500));

        assert!(
            token_holder(addr, &token).is_err(),
            "a newcomer is turned away while every slot holds an admitted relay"
        );
        assert!(heard.at_capacity.load(Ordering::SeqCst) >= 1);
        assert_eq!(heard.displaced.load(Ordering::SeqCst), 0);
        for (i, relay) in relays.iter_mut().enumerate() {
            assert!(echoes(relay), "relay {i} still answers");
        }

        drop(relays);
        running.store(false, Ordering::Relaxed);
        server.join().unwrap();
    }
}
