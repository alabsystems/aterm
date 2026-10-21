// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A network listener's admission: the slots it holds connections in, the rule
//! by which a connection that has proved nothing gives its slot up (r13 NET-1,
//! 2026-10-10), and the wall-clock deadline on the unauthenticated phase that
//! both ends of the fleet handshake ([`crate::fleet`]) arm.
//!
//! * **Slots** ([`Slots`]) — the bound on a listener's threads and
//!   descriptors. When every slot is held, a newcomer takes the slot of a
//!   connection that has not authenticated, by one of two rules
//!   ([`pick_displaced`]): an address holding at least two more unauthenticated
//!   connections than the newcomer's gives up its oldest at once (rule 1);
//!   otherwise the oldest that has gone the listener's grace without
//!   authenticating does, but only from an address no lighter than the
//!   newcomer's (rule 2), so a heavier address's reconnects never cut a
//!   lighter one's slow handshake. An authenticated connection's slot is never
//!   taken. Idle sockets renewed against the handshake deadline used to hold
//!   every slot and lock every real peer out.
//! * **The watchdog** ([`HandshakeWatchdog`]) — a per-syscall timeout bounds an
//!   idle read, not a peer that dribbles a byte just under it; the watchdog
//!   force-closes the socket at a total wall-clock deadline unless the
//!   handshake claimed the connection first. One compare-and-swap decides
//!   between the handshake, the deadline and the listener's eviction.
//!
//! The slots and the watchdog's claim are the ty model
//! `aterm_models_core::net_slot_displacement_model` (`NetSlotDisplacement`),
//! whose actions the `#[refines]` anchors here name.

use std::io;
use std::net::{IpAddr, Ipv6Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// How often a listener's accept loop wakes to re-check its `running` flag
/// ([`crate::fleet::serve`]), so the kill-switch is observed promptly instead
/// of only when the next peer connects; and how long making room waits for a
/// displaced connection to let go ([`Slots::make_room`]).
pub(crate) const ACCEPT_POLL: Duration = Duration::from_millis(200);

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
pub(crate) fn wait_listener_readable(
    listener: &TcpListener,
    timeout: Duration,
) -> io::Result<bool> {
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
pub(crate) fn wait_listener_readable(
    listener: &TcpListener,
    timeout: Duration,
) -> io::Result<bool> {
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
pub(crate) fn wait_listener_readable(
    _listener: &TcpListener,
    timeout: Duration,
) -> io::Result<bool> {
    // Native shipping targets use poll/WSAPoll above. Retain a compiling,
    // bounded fallback for other std targets rather than claiming readiness.
    std::thread::sleep(timeout);
    Ok(false)
}

/// The address a connection is counted against (the module doc's rule 1): its
/// IPv4 address, or its IPv6 /64 — the block one site is given, inside which a
/// single host mints addresses at will. An IPv4-mapped peer (a dual-stack
/// listener) counts as its IPv4 address.
///
/// On a LAN the /64 is coarse: every IPv6 host on one segment shares one
/// (`fe80::/64` for link-local, the LAN's own SLAAC or ULA prefix otherwise),
/// so an IPv6 attacker and every IPv6 peer there are one address, which rule 1
/// never separates; only rule 2 does. A listener on an IPv4 address is held to
/// both rules.
pub(crate) fn source_of(peer: IpAddr) -> IpAddr {
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
/// slot is ever named — rule 1, then rule 2 (with `grace`, the listener's
/// own), of the module doc — and only one from an address holding at least
/// as many unauthenticated connections as the newcomer's.
fn pick_displaced(held: &[Held], source: IpAddr, now: Instant, grace: Duration) -> Option<usize> {
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
    // 2. Otherwise whichever has gone `grace` without authenticating, from an
    // address no lighter than the newcomer's: a heavier address's reconnects
    // never cut a lighter one's slow handshake.
    oldest(&|h| count(h.source) >= mine && now.saturating_duration_since(h.since) >= grace)
}

/// One connection a listener holds a slot for, handshaking or served.
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

/// A listener's slots ([`crate::fleet::serve`]'s), one per connection in
/// flight: the bound on its threads and descriptors. Only the accept loop
/// takes a slot or displaces a connection (the module doc's rules); each
/// connection's [`SlotTenure`] frees its own.
pub(crate) struct Slots {
    held: Mutex<Vec<Slot>>,
    /// Signalled whenever a slot is freed.
    freed: Condvar,
    /// The most connections in flight at once.
    capacity: usize,
    /// Rule 2's grace.
    grace: Duration,
}

impl Slots {
    /// No slot held yet: at most `capacity` at once, an unauthenticated one
    /// given up to a newcomer from a no-heavier address once `grace` old.
    pub(crate) fn new(capacity: usize, grace: Duration) -> Self {
        Self {
            held: Mutex::new(Vec::new()),
            freed: Condvar::new(),
            capacity,
            grace,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Slot>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether a newcomer from `source` gets a slot: a free one, or one
    /// [`pick_displaced`] lets it take. A displaced connection is cut off at
    /// once, and its threads end before the newcomer's start, so making room
    /// never exceeds the bound; one that has not let go within [`ACCEPT_POLL`]
    /// leaves the newcomer refused rather than the loop stalled.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetSlotDisplacement",
            action = "Refuse",
            project = "slots::tests::slots_conform_to_net_slot_displacement"
        )
    )]
    pub(crate) fn make_room(&self, source: IpAddr) -> bool {
        let held = self.lock();
        if held.len() < self.capacity {
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
            let Some(displaced) = pick_displaced(&view, source, now, self.grace) else {
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
            .wait_timeout_while(held, ACCEPT_POLL, |held| held.len() >= self.capacity)
            .unwrap_or_else(PoisonError::into_inner);
        held.len() < self.capacity
    }

    /// Give connection `id`, armed with `phase`, a slot, freed when the
    /// returned tenure drops.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetSlotDisplacement",
            action = "Accept",
            project = "slots::tests::slots_conform_to_net_slot_displacement"
        )
    )]
    pub(crate) fn take(self: &Arc<Self>, id: u64, source: IpAddr, phase: Evictor) -> SlotTenure {
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

    /// Free connection `id`'s slot as its [`SlotTenure`] drops: a cut
    /// connection's (`Release`) or a served one's (`Close`).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetSlotDisplacement",
            action = "Release",
            project = "slots::tests::slots_conform_to_net_slot_displacement"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetSlotDisplacement",
            action = "Close",
            project = "slots::tests::slots_conform_to_net_slot_displacement"
        )
    )]
    fn free(&self, id: u64) {
        self.lock().retain(|slot| slot.id != id);
        self.freed.notify_all();
    }
}

/// Frees its connection's slot when dropped: by the connection's handler
/// thread as it ends (including on panic), so a refused, displaced, cut or
/// finished connection always frees its slot — or earlier, by a served
/// connection's end, before its peer hears it (`crate::fleet::Served`).
pub(crate) struct SlotTenure {
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
// force-closes the socket unless the handshake and its proof already finished. The
// state leaves `WD_RUNNING` exactly once (CAS) — to `WD_AUTHED` by the handshake, to
// `WD_FIRED` by the deadline, or to `WD_EVICTED` by the listener giving the slot to a
// newer connection — so neither the watchdog nor the listener ever shuts a socket
// the hand-over is about to use. This claim, the slots a listener holds and the
// newcomer that waits for one are the ty model
// `aterm_models_core::net_slot_displacement_model` (`NetSlotDisplacement`,
// 2026-10-10), whose actions the `#[refines]` anchors here name.
const WD_RUNNING: u8 = 0;
const WD_AUTHED: u8 = 1;
const WD_FIRED: u8 = 2;
const WD_EVICTED: u8 = 3;

/// How a connection's unauthenticated phase ended: the claim that won.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Claim {
    /// The handshake finished first: the connection is the caller's.
    Authed,
    /// The wall-clock deadline elapsed first; the socket was force-closed.
    Deadline,
    /// The listener gave the connection's slot to a newer one; the socket was
    /// force-closed.
    Evicted,
}

/// Why a connection the listener displaced was not admitted (its `Refused`
/// reason).
pub(crate) const DISPLACED: &str = "a newer connection took its slot before it authenticated";

/// The listener's hold on one connection's unauthenticated phase: it can end
/// that phase early, through the same exactly-once claim the deadline makes.
#[derive(Clone)]
pub(crate) struct Evictor {
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
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetSlotDisplacement",
            action = "Evict",
            project = "slots::tests::slots_conform_to_net_slot_displacement"
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
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NetSlotDisplacement",
        action = "Deadline",
        project = "slots::tests::slots_conform_to_net_slot_displacement"
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
/// Both ends of the fleet handshake ([`crate::fleet`]) arm one; keeping it in a
/// single type is what stops the copies drifting.
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
/// so the watchdog never shuts a socket the hand-over is about to use.
pub(crate) struct HandshakeWatchdog {
    /// The state claimed by exactly one of the handshake, the watchdog and an
    /// [`Evictor`].
    state: Arc<AtomicU8>,
    /// The parked watchdog thread — its `Thread` handle is the wake channel.
    thread: std::thread::JoinHandle<()>,
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
    pub(crate) fn arm(tcp: &TcpStream, handshake_timeout: Duration) -> io::Result<Self> {
        let state = Arc::new(AtomicU8::new(WD_RUNNING));
        let wd_sock = tcp.try_clone()?;
        let wd_state = Arc::clone(&state);
        #[cfg(test)]
        let waiting = Arc::new(std::sync::atomic::AtomicBool::new(false));
        #[cfg(test)]
        let wd_waiting = Arc::clone(&waiting);
        // Fallible spawn: the fleet's `serve` arms on its accept loop, which a refused
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
            // Force-close so the blocked handshake or proof read errors out.
            if cut_off {
                aterm_uds::hangup::hang_up(&wd_sock);
            }
        })?;
        Ok(Self {
            state,
            thread,
            #[cfg(test)]
            waiting,
        })
    }

    /// The listener's handle for ending this phase early ([`Evictor::evict`]).
    pub(crate) fn evictor(&self) -> Evictor {
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
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetSlotDisplacement",
            action = "Authenticate",
            project = "slots::tests::slots_conform_to_net_slot_displacement"
        )
    )]
    pub(crate) fn finish(self) -> Claim {
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
pub(crate) fn arm_unauthenticated(
    tcp: &TcpStream,
    handshake_timeout: Duration,
) -> io::Result<HandshakeWatchdog> {
    tcp.set_read_timeout(Some(handshake_timeout))?;
    tcp.set_write_timeout(Some(handshake_timeout))?;
    HandshakeWatchdog::arm(tcp, handshake_timeout)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use aterm_spec::wait::MUST_HAPPEN;
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    /// The slots' production capacity and rule-2 grace: the fleet
    /// listener's.
    const CAP: usize = crate::fleet::MAX_SLOTS;
    const GRACE: Duration = crate::fleet::PREAUTH_GRACE;

    /// The deadline shape a socket read or write takes when its timer runs out.
    fn is_deadline(e: &io::Error) -> bool {
        matches!(
            e.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        )
    }

    #[test]
    fn real_listener_readiness_wakes_before_the_shutdown_interval() {
        // Deliberately much longer than production's 200 ms bound: a
        // sleep-based implementation pays the whole five minutes and fails by a
        // wide margin. The wake ceiling is a minute's hang detector (AGENTS.md),
        // never a latency budget, so it still distinguishes that negative
        // control and no scheduler starvation of a loaded machine reaches it.
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

    /// `count` idle sockets to one address (a listener's capacity: the
    /// fleet's [`CAP`]), each sending nothing and reconnected the moment the
    /// listener closes it, until dropped. Stops them however the test ends, so
    /// a failure does not leave them hammering the rest of the binary.
    pub(crate) struct IdleSockets {
        stop: Arc<AtomicBool>,
        threads: Vec<std::thread::JoinHandle<()>>,
    }

    impl IdleSockets {
        pub(crate) fn hold(target: SocketAddr, count: usize) -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let threads = (0..count)
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

    /// Once the listener turned a connection from `target` away at capacity
    /// (`at_capacity`, its count of those), idle sockets hold every slot: the
    /// attack is in force.
    pub(crate) fn until_turned_away(at_capacity: &AtomicUsize, target: SocketAddr) {
        let started = Instant::now();
        while at_capacity.load(Ordering::SeqCst) == 0 {
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
    /// Ends the test COULD NOT RUN on a host whose loopback has no IPv6 or
    /// whose `[::]` listener is IPv6-only by default (Windows).
    pub(crate) fn dual_stack_listener(test: &str) -> (TcpListener, u16) {
        fn not_run(test: &str, why: &str) -> ! {
            aterm_spec::host::could_not_run(format!("{test}: no dual-stack loopback here ({why})"))
        }
        let listener = match TcpListener::bind("[::]:0") {
            Ok(listener) => listener,
            Err(e) => not_run(test, &format!("binding [::]: {e}")),
        };
        let port = listener.local_addr().unwrap().port();
        let mut sources = Vec::new();
        for ip in [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ] {
            let probe = match TcpStream::connect_timeout(&SocketAddr::new(ip, port), MUST_HAPPEN) {
                Ok(probe) => probe,
                Err(e) => not_run(test, &format!("{ip} cannot reach [::]: {e}")),
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
        (listener, port)
    }

    /// The module doc's rule, case by case, on synthetic addresses. The real-socket
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
        let grown = t0 + GRACE + Duration::from_millis(500);

        // One address holds every slot, unauthenticated and fresh.
        let crowded = slots(&[(a, true); CAP]);
        // Rule 1: another address takes the crowding address's oldest, at
        // any age, however fast that address reconnects.
        assert_eq!(pick_displaced(&crowded, b, fresh, GRACE), Some(0));
        // The crowding address itself waits out GRACE (rule 2) ...
        assert_eq!(pick_displaced(&crowded, a, fresh, GRACE), None);
        // ... and then takes its own oldest.
        assert_eq!(pick_displaced(&crowded, a, grown, GRACE), Some(0));

        // The margin of two: a holds 2, b holds 1, the rest are relays. b
        // cannot take a's slot (it would only swap who is heavier); c can.
        let mut mixed = vec![(c, false); CAP - 3];
        mixed.extend([(b, true), (a, true), (a, true)]);
        let mixed = slots(&mixed);
        assert_eq!(pick_displaced(&mixed, b, fresh, GRACE), None);
        assert_eq!(pick_displaced(&mixed, c, fresh, GRACE), Some(CAP - 2));
        // Rule 2 takes the oldest unauthenticated one, never an older relay.
        assert_eq!(pick_displaced(&mixed, b, grown, GRACE), Some(CAP - 3));

        // Two addresses tie for most: the oldest of either.
        let tied = slots(&[(b, true), (a, true), (a, true), (b, true)]);
        assert_eq!(pick_displaced(&tied, c, fresh, GRACE), Some(0));

        // Every slot an admitted relay, however old: nothing to take.
        let relays = slots(&[(a, false); CAP]);
        for newcomer in [a, b, c] {
            assert_eq!(pick_displaced(&relays, newcomer, grown, GRACE), None);
        }

        // Rule 2's address bound (2026-10-10, on review): b's one handshake
        // is older than GRACE, a's are fresh and fill every other
        // slot. a's own newcomer may not cut b's slow handshake, however old
        // it is: it waits for one of a's to age, and then takes that one.
        let mut slow = vec![Held {
            source: b,
            since: t0,
            unauthenticated: true,
        }];
        slow.extend((1u64..).take(CAP - 1).map(|i| Held {
            source: a,
            since: t0 + GRACE + Duration::from_millis(i),
            unauthenticated: true,
        }));
        let b_aged = t0 + GRACE + Duration::from_millis(500);
        assert_eq!(pick_displaced(&slow, a, b_aged, GRACE), None);
        let all_aged = t0 + GRACE * 2 + Duration::from_millis(500);
        assert_eq!(pick_displaced(&slow, a, all_aged, GRACE), Some(1));
        // A newcomer from an address as light as b's may take it, once old.
        assert_eq!(
            pick_displaced(&slow, c, b_aged, GRACE),
            Some(1),
            "rule 1 first"
        );
        let mut even = slow.clone();
        even.truncate(2);
        even.push(Held {
            source: c,
            since: t0 + GRACE,
            unauthenticated: true,
        });
        assert_eq!(pick_displaced(&even, c, b_aged, GRACE), Some(0));
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
    /// `aterm_models_core::net_slot_displacement_model` (`NetSlotDisplacement`)
    /// at the production capacity (`Cap` overridden to [`CAP`]).
    ///
    /// The test plays both halves of a listener (`crate::fleet::serve`) over
    /// real loopback sockets: its accept loop (`Slots::make_room`,
    /// [`arm_unauthenticated`]'s watchdog, `Slots::take`) and each
    /// connection's handler thread (the handshake's
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
    fn slots_conform_to_net_slot_displacement() {
        use aterm_models_core::net_slot_displacement_model;
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
        let cap = n(CAP);
        let overrides = [("Cap", cap), ("Arrivals", 2 * cap)];
        let at_scale = with_consts(&model, &overrides);
        let check = |seen: &mut State, action: &str, now: State| {
            let (ok, why) = validate_transition_tiered(
                &model,
                &overrides,
                seen,
                &now,
                Some(action),
                "NetSlotDisplacement(slots)",
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
        let slots = Arc::new(Slots::new(CAP, GRACE));
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
        for _ in 0..CAP {
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
}
