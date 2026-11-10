// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Network listener slot admission ([`Slots`], [`pick_displaced`]) and unauthenticated
//! [`HandshakeWatchdog`] deadline for [`crate::fleet`].

#[cfg(any(unix, windows))]
use aterm_uds::deadline::poll_timeout_ms;
use std::io;
use std::net::{IpAddr, Ipv6Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The existing LAN validity and failed-accept retry bound. Ordinary fleet listeners do not
/// use this as an idle timer; slot displacement also retains this bounded release wait.
pub(crate) const ACCEPT_POLL: Duration = Duration::from_millis(200);

/// Sources ready in one native listener wait. A timeout has no ready source.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ListenerReady {
    #[cfg(unix)]
    pub(crate) listener: bool,
    #[cfg(unix)]
    pub(crate) stop: bool,
    #[cfg(unix)]
    pub(crate) live: bool,
    #[cfg(unix)]
    pub(crate) withdrawn: bool,
    #[cfg(unix)]
    pub(crate) changes: bool,
}

/// Wait on the selected listener/control/validity sources until one is ready or the absolute
/// deadline expires. `None` is an indefinite wait; interruption never resets the deadline.
#[cfg(unix)]
pub(crate) fn wait_listener_readable(
    listener: Option<&TcpListener>,
    stop: Option<std::os::fd::BorrowedFd<'_>>,
    live: Option<&aterm_uds::CtlStream>,
    withdrawn: Option<std::os::fd::BorrowedFd<'_>>,
    changes: Option<std::os::fd::BorrowedFd<'_>>,
    deadline: Option<Instant>,
) -> io::Result<Option<ListenerReady>> {
    use std::os::fd::AsRawFd as _;

    let mut descriptors = Vec::with_capacity(5);
    let mut add = |fd| {
        let at = descriptors.len();
        descriptors.push(libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
        at
    };
    let listener_at = listener.map(|socket| add(socket.as_raw_fd()));
    let stop_at = stop.map(|fd| add(fd.as_raw_fd()));
    let live_at = live.map(|socket| add(socket.as_raw_fd()));
    let withdrawn_at = withdrawn.map(|fd| add(fd.as_raw_fd()));
    let changes_at = changes.map(|fd| add(fd.as_raw_fd()));
    if descriptors.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no listener wait source",
        ));
    }
    loop {
        let timeout = deadline.map_or(-1, |at| {
            poll_timeout_ms(at.saturating_duration_since(Instant::now()))
        });
        // SAFETY: at most five initialized pollfds borrow live sources for this call.
        let result = unsafe {
            libc::poll(
                descriptors.as_mut_ptr(),
                descriptors.len() as libc::nfds_t,
                timeout,
            )
        };
        if result >= 0 {
            // A saturated native timeout can end before the absolute deadline.
            if result == 0 && deadline.is_some_and(|at| Instant::now() < at) {
                continue;
            }
            return Ok((result > 0).then(|| ListenerReady {
                listener: listener_at.is_some_and(|at| descriptors[at].revents != 0),
                stop: stop_at.is_some_and(|at| descriptors[at].revents != 0),
                live: live_at.is_some_and(|at| descriptors[at].revents != 0),
                withdrawn: withdrawn_at.is_some_and(|at| descriptors[at].revents != 0),
                changes: changes_at.is_some_and(|at| descriptors[at].revents != 0),
            }));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
        if deadline.is_some_and(|at| Instant::now() >= at) {
            return Ok(None);
        }
    }
}

#[cfg(windows)]
pub(crate) fn wait_listener_readable(
    listener: Option<&TcpListener>,
    deadline: Option<Instant>,
) -> io::Result<Option<ListenerReady>> {
    use aterm_win32::Win32::Networking::WinSock::{
        POLLIN, SOCKET_ERROR, WSAGetLastError, WSAPOLLFD, WSAPoll,
    };
    use std::os::windows::io::AsRawSocket as _;

    let listener = listener
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no listener wait source"))?;
    let mut descriptor = WSAPOLLFD {
        fd: listener.as_raw_socket() as usize,
        events: POLLIN,
        revents: 0,
    };
    loop {
        let timeout = deadline.map_or(-1, |at| {
            poll_timeout_ms(at.saturating_duration_since(Instant::now()))
        });
        // SAFETY: the initialized entry borrows the live Winsock listener for this call.
        let result = unsafe { WSAPoll(&raw mut descriptor, 1, timeout) };
        if result != SOCKET_ERROR {
            // A saturated native timeout can end before the absolute deadline.
            if result == 0 && deadline.is_some_and(|at| Instant::now() < at) {
                continue;
            }
            return Ok((result > 0).then_some(ListenerReady {}));
        }
        // SAFETY: WSAGetLastError takes no arguments and reads only the calling thread's slot.
        let error = io::Error::from_raw_os_error(unsafe { WSAGetLastError() });
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
        if deadline.is_some_and(|at| Instant::now() >= at) {
            return Ok(None);
        }
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn wait_listener_readable(
    _listener: Option<&TcpListener>,
    _deadline: Option<Instant>,
) -> io::Result<Option<ListenerReady>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native listener waits unavailable",
    ))
}

/// Observe only the TCP accept queue without consuming a connection or a control hint.
/// # Errors
/// The timeout cannot be represented as a deadline, or the native wait fails.
#[cfg(all(unix, any(test, debug_assertions)))]
pub(crate) fn listener_pending_within(
    listener: &TcpListener,
    timeout: Duration,
) -> io::Result<bool> {
    let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "listener wait timeout is too large",
        )
    })?;
    wait_listener_readable(
        Some(listener),
        #[cfg(unix)]
        None,
        #[cfg(unix)]
        None,
        #[cfg(unix)]
        None,
        #[cfg(unix)]
        None,
        Some(deadline),
    )
    .map(|ready| ready.is_some_and(|ready| ready.listener))
}

/// The address a connection is counted against ([`pick_displaced`]).
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
    /// Its connection has neither authenticated nor been cut.
    unauthenticated: bool,
}

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
    let most = pending().map(|(_, h)| count(h.source)).max()?;
    if most.saturating_sub(mine) >= 2 {
        return oldest(&|h| count(h.source) == most);
    }
    oldest(&|h| count(h.source) >= mine && now.saturating_duration_since(h.since) >= grace)
}

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

/// A listener's slots ([`crate::fleet::serve`]'s), one per connection in flight.
pub(crate) struct Slots {
    held: Mutex<Vec<Slot>>,
    /// Signalled whenever a slot is freed.
    freed: Condvar,
    /// The most connections in flight.
    capacity: usize,
    /// Rule 2's grace.
    grace: Duration,
}

impl Slots {
    /// No slot held yet: at most `capacity` at once, an unauthenticated one given up to a newcomer
    /// from a no-heavier address once `grace` old.
    pub(crate) const fn new(capacity: usize, grace: Duration) -> Self {
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

    /// How many slots connections from `source` ([`source_of`]) hold before they authenticate.
    #[cfg(test)]
    pub(crate) fn unauthenticated_from(&self, source: IpAddr) -> usize {
        self.lock()
            .iter()
            .filter(|slot| slot.source == source && slot.phase.unauthenticated())
            .count()
    }

    /// Whether a newcomer from `source` gets a slot.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetSlotDisplacement",
            action = "Refuse",
            project = "slots::tests::slots_conform_to_net_slot_displacement"
        )
    )]
    pub(crate) fn make_room(
        self: &Arc<Self>,
        source: IpAddr,
        stop: &aterm_uds::stop::Stop,
    ) -> bool {
        if stop.is_set() {
            return false;
        }
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
        }
        let until = Instant::now() + ACCEPT_POLL;
        let (held, _) = self
            .freed
            .wait_timeout_while(
                held,
                until.saturating_duration_since(Instant::now()),
                |held| held.len() >= self.capacity && !stop.is_set(),
            )
            .unwrap_or_else(PoisonError::into_inner);
        !stop.is_set() && held.len() < self.capacity
    }

    /// Give connection `id`, armed with `phase`, a slot, freed when the returned tenure drops.
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

    /// Free connection `id`'s slot as its [`SlotTenure`] drops.
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

/// Frees its connection's slot when dropped.
pub(crate) struct SlotTenure {
    slots: Arc<Slots>,
    id: u64,
}

impl Drop for SlotTenure {
    fn drop(&mut self) {
        self.slots.free(self.id);
    }
}

const WD_RUNNING: u8 = 0;
const WD_AUTHED: u8 = 1;
const WD_FIRED: u8 = 2;
const WD_EVICTED: u8 = 3;

/// How a connection's unauthenticated phase ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Claim {
    /// The handshake finished first.
    Authed,
    /// The wall-clock deadline elapsed first.
    Deadline,
    /// The listener gave the connection's slot to a newer one.
    Evicted,
}

/// Why a connection the listener displaced was not admitted (its `Refused` reason).
pub(crate) const DISPLACED: &str = "a newer connection took its slot before it authenticated";

/// The listener's hold on one connection's unauthenticated phase.
#[derive(Clone)]
pub(crate) struct Evictor {
    /// The watchdog's state.
    state: Arc<AtomicU8>,
    /// The parked watchdog thread, woken to force-close.
    watchdog: std::thread::Thread,
}

impl Evictor {
    fn unauthenticated(&self) -> bool {
        self.state.load(Ordering::Acquire) == WD_RUNNING
    }

    /// Claim [`WD_EVICTED`] iff the phase is still running.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetSlotDisplacement",
            action = "Evict",
            project = "slots::tests::slots_conform_to_net_slot_displacement"
        )
    )]
    pub(crate) fn evict(&self) -> bool {
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

/// Claim the elapsed deadline without replacing an earlier decision.
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

/// An owned socket deadline, also used for a connection's unauthenticated phase.
/// The socket must stay open until this guard is finished or dropped, which joins its thread.
pub(crate) struct HandshakeWatchdog {
    until: Instant,
    /// The state claimed by exactly one of the handshake, the watchdog and an [`Evictor`].
    state: Arc<AtomicU8>,
    /// The parked watchdog thread (`None` once [`Self::finish`] or [`Drop`] has joined it).
    thread: Option<std::thread::JoinHandle<()>>,
    /// Set by the thread each time it is about to wait.
    #[cfg(test)]
    waiting: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(all(test, unix))]
    joining: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl HandshakeWatchdog {
    /// Arm the deadline on `tcp`.
    ///
    /// # Errors
    /// If the socket cannot be cloned (Windows) or the watchdog thread cannot start.
    pub(crate) fn arm(tcp: &TcpStream, handshake_timeout: Duration) -> io::Result<Self> {
        Self::arm_with_clock(tcp, handshake_timeout, Instant::now)
    }

    fn arm_with_clock(
        tcp: &TcpStream,
        handshake_timeout: Duration,
        now: impl Fn() -> Instant + Send + 'static,
    ) -> io::Result<Self> {
        // Thread creation and delayed scheduling are part of this handshake's
        // budget. The worker must not restart its clock when it finally runs.
        let start = now();
        let until = start.checked_add(handshake_timeout).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "TLS deadline is out of range")
        })?;
        Self::arm_until_with_clock(tcp, until, now)
    }

    /// Arm an existing absolute deadline, including time spent before this phase.
    pub(crate) fn arm_until(tcp: &TcpStream, until: Instant) -> io::Result<Self> {
        Self::arm_until_with_clock(tcp, until, Instant::now)
    }

    pub(crate) fn arm_until_with_clock(
        tcp: &TcpStream,
        until: Instant,
        now: impl Fn() -> Instant + Send + 'static,
    ) -> io::Result<Self> {
        let state = Arc::new(AtomicU8::new(WD_RUNNING));
        let _ = aterm_uds::keep_out_of_children(tcp);
        #[cfg(unix)]
        let wd_fd = {
            use std::os::fd::AsRawFd as _;
            tcp.as_raw_fd()
        };
        #[cfg(not(unix))]
        let wd_sock = tcp.try_clone()?;
        let wd_state = Arc::clone(&state);
        #[cfg(test)]
        let waiting = Arc::new(std::sync::atomic::AtomicBool::new(false));
        #[cfg(test)]
        let wd_waiting = Arc::clone(&waiting);
        let thread = std::thread::Builder::new().spawn(move || {
            let cut_off = loop {
                match wd_state.load(Ordering::Acquire) {
                    WD_RUNNING => {}
                    WD_EVICTED | WD_FIRED => break true,
                    _ => break false, // finished early — nothing to cut off
                }
                let remaining = until.saturating_duration_since(now());
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
            if cut_off {
                #[cfg(unix)]
                {
                    // SAFETY: `wd_fd` belongs to the caller's `TcpStream`, which is kept open
                    // until after `HandshakeWatchdog::finish` (or `Drop`) joins this thread.
                    // Avoiding `try_clone` here means `finish` never closes a duplicate
                    // descriptor while the socket stays in use (on macOS, a concurrent
                    // `PROC_PIDFDSOCKETINFO` scan during `close` of one duplicate permanently
                    // drains the shared socket so later blocking reads fail with `EBADF`).
                    unsafe {
                        let _ = libc::shutdown(wd_fd, libc::SHUT_WR);
                        let _ = libc::shutdown(wd_fd, libc::SHUT_RD);
                    }
                }
                #[cfg(not(unix))]
                aterm_uds::hangup::hang_up(&wd_sock);
            }
        })?;
        Ok(Self {
            until,
            state,
            thread: Some(thread),
            #[cfg(test)]
            waiting,
            #[cfg(all(test, unix))]
            joining: None,
        })
    }

    /// Observe the descriptor immediately before joining the watchdog thread.
    #[cfg(all(test, unix))]
    pub(crate) fn observe_join(&mut self, probe: impl FnOnce() + Send + Sync + 'static) {
        self.joining = Some(Box::new(probe));
    }

    /// Whether the deadline claimed the phase, so the watchdog hung the socket up at it: what a
    /// read or write of it meets then is the deadline's, not the peer's.
    pub(crate) fn fired(&self) -> bool {
        self.state.load(Ordering::Acquire) == WD_FIRED
    }

    /// The listener's handle for ending this phase early ([`Evictor::evict`]).
    pub(crate) fn evictor(&self) -> Evictor {
        Evictor {
            state: Arc::clone(&self.state),
            watchdog: self
                .thread
                .as_ref()
                .expect("watchdog thread is live until finish")
                .thread()
                .clone(),
        }
    }

    /// Claim [`WD_AUTHED`] only before the deadline and if no eviction claimed the phase first,
    /// then wake the watchdog.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetSlotDisplacement",
            action = "Authenticate",
            project = "slots::tests::slots_conform_to_net_slot_displacement"
        )
    )]
    pub(crate) fn finish(self) -> Claim {
        self.finish_at(Instant::now())
    }

    fn finish_at(mut self, now: Instant) -> Claim {
        // A delayed watchdog cannot grant time to a handshake that completed
        // after its original deadline. An earlier eviction still wins.
        if now >= self.until {
            let _ = claim_deadline(&self.state);
        }
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
        if let Some(thread) = self.thread.take() {
            #[cfg(all(test, unix))]
            if let Some(probe) = self.joining.take() {
                probe();
            }
            thread.thread().unpark();
            let _ = thread.join();
        }
        claim
    }
}

impl Drop for HandshakeWatchdog {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = self.state.compare_exchange(
                WD_RUNNING,
                WD_AUTHED,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            #[cfg(all(test, unix))]
            if let Some(probe) = self.joining.take() {
                probe();
            }
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

/// Arm both deadlines on an accepted connection's unauthenticated phase.
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

    const CAP: usize = crate::fleet::MAX_SLOTS;
    const GRACE: Duration = crate::fleet::PREAUTH_GRACE;

    fn is_deadline(e: &io::Error) -> bool {
        matches!(
            e.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        )
    }

    #[test]
    fn real_listener_readiness_wakes_before_the_shutdown_interval() {
        const TEST_WAIT: Duration = Duration::from_secs(300);
        const MAX_READY_WAKE: Duration = MUST_HAPPEN;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let waiting_listener = listener.try_clone().unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
        let waiter = std::thread::spawn(move || {
            entered_tx.send(()).unwrap();
            wait_listener_readable(
                Some(&waiting_listener),
                #[cfg(unix)]
                None,
                #[cfg(unix)]
                None,
                #[cfg(unix)]
                None,
                #[cfg(unix)]
                None,
                Some(Instant::now() + TEST_WAIT),
            )
        });
        entered_rx.recv().unwrap();

        let started = Instant::now();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        assert!(waiter.join().unwrap().unwrap().is_some());
        let elapsed = started.elapsed();
        assert!(
            elapsed < MAX_READY_WAKE,
            "a queued real connection must wake readiness instead of waiting out \
             the {TEST_WAIT:?} test interval ({elapsed:?})"
        );
        let (accepted, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Nonblocking,
        )
        .expect("the readiness event corresponds to an accept-ready connection");
        drop((accepted, client));
    }

    #[test]
    fn idle_listener_readiness_wait_is_bounded_without_spinning() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let started = Instant::now();
        assert!(
            wait_listener_readable(
                Some(&listener),
                #[cfg(unix)]
                None,
                #[cfg(unix)]
                None,
                #[cfg(unix)]
                None,
                #[cfg(unix)]
                None,
                Some(started + ACCEPT_POLL),
            )
            .unwrap()
            .is_none()
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed < MUST_HAPPEN,
            "the explicit {ACCEPT_POLL:?} readiness deadline ends within the hang detector ({elapsed:?})"
        );
        assert_eq!(
            aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Ready,
                aterm_uds::AcceptedMode::Nonblocking,
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn a_late_watchdog_start_does_not_restart_the_handshake_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let tcp = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_peer, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        tcp.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
        let creator = std::thread::current().id();
        let started = Instant::now();
        let budget = MUST_HAPPEN * 10;
        // The worker's first observation is already beyond the deadline.
        // No sleep or assumption about actual scheduler latency is involved.
        let now = move || {
            if std::thread::current().id() == creator {
                started
            } else {
                started + budget * 2
            }
        };
        let watchdog = HandshakeWatchdog::arm_with_clock(&tcp, budget, now).unwrap();
        let mut byte = [0; 1];
        let read = (&tcp).read(&mut byte);
        assert!(
            matches!(&read, Ok(0)) || read.as_ref().is_err_and(|error| !is_deadline(error)),
            "a delayed watchdog must cut the expired handshake before waiting again: {read:?}"
        );
        assert_eq!(watchdog.finish(), Claim::Deadline);
    }

    #[test]
    fn a_late_handshake_cannot_outrun_its_delayed_watchdog() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let tcp = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_peer, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        tcp.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
        let started = Instant::now();
        let budget = MUST_HAPPEN * 10;
        let watchdog = HandshakeWatchdog::arm_with_clock(&tcp, budget, move || started).unwrap();
        let until = Instant::now() + MUST_HAPPEN;
        while !watchdog.waiting.load(Ordering::Acquire) {
            assert!(Instant::now() < until, "the watchdog never began waiting");
            std::thread::sleep(Duration::from_millis(1));
        }
        // The exact boundary is expired even if the timer has not run.
        assert_eq!(
            watchdog.finish_at(started + budget),
            Claim::Deadline,
            "completion must check elapsed time even while the watchdog is still waiting"
        );
        let mut byte = [0; 1];
        let read = (&tcp).read(&mut byte);
        assert!(
            matches!(&read, Ok(0)) || read.as_ref().is_err_and(|error| !is_deadline(error)),
            "a completion that claims the deadline must close its socket: {read:?}"
        );
    }

    /// An early handshake's `finish()` wakes the watchdog it joins, never waiting out the
    /// deadline: armed for ten minutes and parked, it is finished within the one-minute hang
    /// detector.
    #[test]
    fn finishing_an_early_handshake_wakes_the_watchdog_instead_of_waiting_out_its_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let accepting = std::thread::spawn(move || {
            aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap()
            .0
        });
        let tcp = TcpStream::connect(addr).unwrap();
        let _peer = accepting.join().unwrap();
        let watchdog = HandshakeWatchdog::arm(&tcp, MUST_HAPPEN * 10).unwrap();
        let entered = Instant::now() + MUST_HAPPEN;
        while !watchdog.waiting.load(Ordering::Acquire) {
            assert!(
                Instant::now() < entered,
                "the watchdog never began its wait"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(watchdog.finish());
        });
        assert_eq!(
            rx.recv_timeout(MUST_HAPPEN),
            Ok(Claim::Authed),
            "an early handshake wakes the parked watchdog and beats its deadline"
        );
    }

    #[test]
    fn spurious_wakes_neither_shorten_nor_starve_the_watchdog_deadline() {
        const DEADLINE: Duration = Duration::from_millis(150);
        const HAMMER_BUDGET: Duration = Duration::from_secs(180);
        const CEILING: Duration = MUST_HAPPEN;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let accepting = std::thread::spawn(move || {
            aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap()
            .0
        });
        let tcp = TcpStream::connect(addr).unwrap();
        let _peer = accepting.join().unwrap();
        tcp.set_read_timeout(Some(Duration::from_millis(5)))
            .unwrap();

        let started = Instant::now();
        let watchdog = HandshakeWatchdog::arm(&tcp, DEADLINE).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let waker = watchdog.evictor().watchdog;
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
            closed_after >= DEADLINE,
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

    /// How long an idle socket ([`IdleSockets`]) waits before it dials again
    /// once the listener closed it or its dial failed. A full listener closes
    /// a newcomer the moment it accepts it, so a socket that dialed again at
    /// once would spin, and each turn-away leaves a TIME_WAIT socket for 30 s
    /// (macOS): one displaced socket dialing at kernel speed through
    /// `a_slow_handshake_is_never_cut_for_a_heavier_address`'s 1.5 s wait made
    /// 16,322 of them, nearly all of the Mac's TIME_WAIT count at a full
    /// `verify`'s peak (each on the listener's own port, so other dials still
    /// found ports), with the listener's thread doing nothing but turning them
    /// away. Paced, a socket dials at most `elapsed / KNOCK_EVERY + 1` times
    /// (`idle_sockets_turned_away_knock_and_never_spin`), still dozens of
    /// knocks within the grace ([`GRACE`]) the displacement rules turn on.
    pub(crate) const KNOCK_EVERY: Duration = Duration::from_millis(20);

    /// `count` idle sockets to one address (a listener's capacity: the
    /// fleet's [`CAP`]), each sending nothing and dialing again
    /// [`KNOCK_EVERY`] after the listener closes it, until dropped.
    pub(crate) struct IdleSockets {
        stop: Arc<AtomicBool>,
        dials: Arc<AtomicUsize>,
        threads: Vec<std::thread::JoinHandle<()>>,
    }

    impl IdleSockets {
        pub(crate) fn hold(target: SocketAddr, count: usize) -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let dials = Arc::new(AtomicUsize::new(0));
            let threads = (0..count)
                .map(|_| {
                    let stop = Arc::clone(&stop);
                    let dials = Arc::clone(&dials);
                    std::thread::spawn(move || {
                        let mut first = true;
                        while !stop.load(Ordering::SeqCst) {
                            if !std::mem::take(&mut first) {
                                std::thread::sleep(KNOCK_EVERY);
                                if stop.load(Ordering::SeqCst) {
                                    break;
                                }
                            }
                            dials.fetch_add(1, Ordering::SeqCst);
                            let Ok(mut idle) = TcpStream::connect_timeout(&target, MUST_HAPPEN)
                            else {
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
            Self {
                stop,
                dials,
                threads,
            }
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

    /// IDLE SOCKETS KNOCK, THEY NEVER SPIN (MH1): against a listener that
    /// closes every connection the moment it accepts it, as a full fleet
    /// listener answers a newcomer, `SOCKETS` idle sockets over `elapsed` dial
    /// at most `SOCKETS * (elapsed / KNOCK_EVERY + 1)` times. The bound is the
    /// pause's own (a sleep never ends early), so it holds however loaded the
    /// machine is. Negative control: with the pause removed, the sockets dial
    /// at kernel speed and pass the bound before the first knock is due.
    #[test]
    fn idle_sockets_turned_away_knock_and_never_spin() {
        const SOCKETS: usize = 2;
        const TURNED_AWAY: usize = 40;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = listener.local_addr().unwrap();
        /// Sends the doorman home however the test ends, a failed assertion included.
        struct Done(Arc<AtomicBool>);
        impl Drop for Done {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let done = Arc::new(AtomicBool::new(false));
        let (turned, turned_away) = std::sync::mpsc::channel();
        let doorman = {
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                while !done.load(Ordering::SeqCst) {
                    if wait_listener_readable(
                        Some(&listener),
                        #[cfg(unix)]
                        None,
                        #[cfg(unix)]
                        None,
                        #[cfg(unix)]
                        None,
                        #[cfg(unix)]
                        None,
                        Some(Instant::now() + ACCEPT_POLL),
                    )
                    .unwrap()
                    .is_none()
                    {
                        continue;
                    }
                    while let Ok((tcp, _)) = aterm_uds::accept(
                        &listener,
                        aterm_uds::AcceptWait::Ready,
                        aterm_uds::AcceptedMode::Nonblocking,
                    ) {
                        drop(tcp);
                        let _ = turned.send(());
                    }
                }
            })
        };

        let home = Done(Arc::clone(&done));
        let started = Instant::now();
        let idle = IdleSockets::hold(target, SOCKETS);
        let dials = Arc::clone(&idle.dials);
        for n in 0..TURNED_AWAY {
            let left = MUST_HAPPEN.saturating_sub(started.elapsed());
            assert!(
                turned_away.recv_timeout(left).is_ok(),
                "turn-away {n} of {TURNED_AWAY}: the idle sockets stopped dialing"
            );
        }
        drop(idle);
        let elapsed = started.elapsed();
        drop(home);
        doorman.join().unwrap();

        let dialed = dials.load(Ordering::SeqCst);
        let knocks = elapsed.as_nanos() / KNOCK_EVERY.as_nanos() + 1;
        let bound = SOCKETS * usize::try_from(knocks).unwrap();
        assert!(
            dialed >= TURNED_AWAY,
            "{dialed} dials, {TURNED_AWAY} turned away"
        );
        assert!(
            dialed <= bound,
            "{SOCKETS} idle sockets dialed {dialed} times in {elapsed:?}: at most {bound} at \
             one dial each {KNOCK_EVERY:?}"
        );
    }

    /// Once connections from `target`'s address hold every one of `slots`
    /// before they authenticate, idle sockets fill the listener: the attack is
    /// in force. Read from the listener's own slots, so no probe takes a seat
    /// and no order of the kernel's accept queue is assumed.
    pub(crate) fn until_seated(slots: &Slots, target: SocketAddr) {
        let source = source_of(target.ip());
        aterm_spec::wait::until_within(MUST_HAPPEN, "the idle sockets fill the listener", || {
            (slots.unauthenticated_from(source) == slots.capacity).then_some(())
        });
    }

    /// A listener on `[::]:0` that takes IPv4 too. A Unix IPv6 socket does by
    /// default (`IPV6_V6ONLY` off).
    #[cfg(not(any(windows, target_os = "macos")))]
    fn bind_dual_stack() -> io::Result<TcpListener> {
        TcpListener::bind("[::]:0")
    }

    /// A listener on `[::]:0` that takes IPv4 too. A Windows IPv6 socket has
    /// `IPV6_V6ONLY` on by default, and the option only counts before `bind`,
    /// which `TcpListener::bind` does at once: so the socket is made, cleared
    /// and bound here.
    #[cfg(windows)]
    fn bind_dual_stack() -> io::Result<TcpListener> {
        use aterm_win32::Win32::Networking::WinSock::{
            AF_INET6, INVALID_SOCKET, IPPROTO_IPV6, IPPROTO_TCP, IPV6_V6ONLY, SOCK_STREAM,
            SOCKADDR, SOCKADDR_IN6, SOCKET_ERROR, WSAGetLastError, bind, listen, setsockopt,
            socket,
        };
        use std::os::windows::io::{AsRawSocket as _, FromRawSocket as _};

        // SAFETY: WSAGetLastError takes no arguments and only reads that slot.
        let failed = || io::Error::from_raw_os_error(unsafe { WSAGetLastError() });
        // std's first socket starts Winsock (WSAStartup), which a raw `socket` needs.
        drop(std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?);
        // SAFETY: plain values; Winsock is started.
        let raw = unsafe { socket(i32::from(AF_INET6), SOCK_STREAM, IPPROTO_TCP) };
        if raw == INVALID_SOCKET {
            return Err(failed());
        }
        // SAFETY: a socket just made, owned by nothing else: the listener closes it.
        let listener = unsafe { TcpListener::from_raw_socket(raw as u64) };
        let handle = listener.as_raw_socket() as usize;
        let off: u32 = 0;
        // SAFETY: `off` is a live 4-byte value for the call, the size IPV6_V6ONLY reads.
        let cleared = unsafe {
            setsockopt(
                handle,
                IPPROTO_IPV6,
                IPV6_V6ONLY,
                std::ptr::from_ref(&off).cast(),
                size_of::<u32>() as i32,
            )
        };
        if cleared == SOCKET_ERROR {
            return Err(failed());
        }
        let any = SOCKADDR_IN6 {
            sin6_family: AF_INET6,
            ..SOCKADDR_IN6::default()
        };
        // SAFETY: `any` is a live SOCKADDR_IN6 of the size passed, for the call.
        let bound = unsafe {
            bind(
                handle,
                std::ptr::from_ref(&any).cast::<SOCKADDR>(),
                size_of::<SOCKADDR_IN6>() as i32,
            )
        };
        if bound == SOCKET_ERROR {
            return Err(failed());
        }
        // SAFETY: a bound socket of this listener's.
        if unsafe { listen(handle, 128) } == SOCKET_ERROR {
            return Err(failed());
        }
        Ok(listener)
    }

    /// A listener on `[::]` that also takes IPv4, and the port it took. It
    /// sees a 127.0.0.1 peer as `::ffff:127.0.0.1` ([`source_of`]: 127.0.0.1)
    /// and a ::1 peer as `::/64`: two addresses on loopback with no second
    /// one configured, which a listener on 127.0.0.1 cannot see (lo0 carries
    /// no other usable IPv4 address without an alias, which needs root).
    pub(crate) fn dual_stack_listener(test: &str) -> (TcpListener, u16) {
        /// How many shadowed ports in a row are passed over; the next one is COULD NOT RUN.
        const PASS_OVER: usize = 64;
        fn not_run(test: &str, why: &str) -> ! {
            aterm_spec::host::could_not_run(format!("{test}: no dual-stack loopback here ({why})"))
        }
        // A shadowed port's socket is kept until a port is chosen, so the kernel hands out
        // the next port, never the same one again.
        let mut passed_over = Vec::new();
        let listener = loop {
            match draw_dual_stack() {
                Ok(Drawn::Clear(listener)) => break listener,
                Ok(Drawn::Shadowed(kept, _)) if passed_over.len() < PASS_OVER => {
                    passed_over.push(kept);
                }
                Ok(Drawn::Shadowed(_, by)) => not_run(
                    test,
                    &format!(
                        "{} ports in a row are shadowed on loopback, the last by {by}",
                        PASS_OVER + 1
                    ),
                ),
                Err(e) => not_run(test, &format!("drawing a dual-stack port: {e}")),
            }
        };
        drop(passed_over);
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
            let peer = accept_probe(&listener, &probe);
            drop(probe);
            sources.push(source_of(peer.ip()));
        }
        assert_ne!(
            sources[0], sources[1],
            "127.0.0.1 and ::1 are two addresses"
        );
        (listener, port)
    }

    /// What a draw of a dual-stack listener found.
    enum Drawn {
        /// A listener on `[::]` whose port no other socket takes a loopback connect on.
        Clear(TcpListener),
        /// A socket of the draw's, on a port the other socket bound at the address given
        /// takes loopback connects on: kept until a port is chosen.
        #[cfg_attr(
            not(target_os = "macos"),
            expect(dead_code, reason = "only macOS hands out a shadowed port")
        )]
        Shadowed(TcpListener, SocketAddr),
    }

    /// A listener on `[::]` that takes IPv4 too, on a port no other socket takes a loopback
    /// connect on. A connect reaches the most specific socket on its port: one bound to
    /// 127.0.0.1 or ::1 over a wildcard, an IPv4 wildcard over a dual-stack one. macOS's port
    /// search for `[::]:0` passes over IPv4 sockets, and `SO_REUSEADDR` (std's) lets a bind sit
    /// beside a wildcard: so the port is drawn on `0.0.0.0`, whose search passes over every
    /// port an IPv4 or dual-stack wildcard holds; the listener binds `[::]` on it beside the
    /// draw, which fails where an IPv6 wildcard holds the port; the draw closes, since it would
    /// outrank the listener, and no IPv4 wildcard can bind there after it; and
    /// [`held_on_loopback`] reads 127.0.0.1 and ::1. Measured on macOS 27.0, 2026-11-03: a
    /// Claude Code held 127.0.0.1:49373 under a test's `[::]:49373` drawn on `[::]`, and the
    /// test's probe went to it.
    #[cfg(target_os = "macos")]
    fn draw_dual_stack() -> io::Result<Drawn> {
        let draw = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        let port = draw.local_addr()?.port();
        let listener = match TcpListener::bind((Ipv6Addr::UNSPECIFIED, port)) {
            Ok(listener) => listener,
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
                let by = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port);
                return Ok(Drawn::Shadowed(draw, by));
            }
            Err(e) => return Err(e),
        };
        drop(draw);
        Ok(match held_on_loopback(port) {
            Some(by) => Drawn::Shadowed(listener, by),
            None => Drawn::Clear(listener),
        })
    }

    /// Linux refuses a bind beside a listening wildcard on its port, so its port search never
    /// hands out a port another socket could take a loopback connect on. On Windows the
    /// probe's bounded accept says what went wrong.
    #[cfg(not(target_os = "macos"))]
    fn draw_dual_stack() -> io::Result<Drawn> {
        bind_dual_stack().map(Drawn::Clear)
    }

    /// Where another socket holds `port` on 127.0.0.1 or ::1: a bind there with `SO_REUSEADDR`
    /// (std's) beside a wildcard fails only where another socket holds that very address
    /// (measured on macOS 27.0, 2026-11-03).
    #[cfg(target_os = "macos")]
    fn held_on_loopback(port: u16) -> Option<SocketAddr> {
        [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ]
        .into_iter()
        .map(|ip| SocketAddr::new(ip, port))
        .find(|&at| matches!(TcpListener::bind(at), Err(e) if e.kind() == io::ErrorKind::AddrInUse))
    }

    /// The source of `probe`'s connection, accepted from `listener` within a minute: a probe
    /// another socket took never arrives, and an unbounded accept would wait for it forever.
    fn accept_probe(listener: &TcpListener, probe: &TcpStream) -> SocketAddr {
        let sent_from = probe.local_addr().unwrap();
        let on = listener.local_addr().unwrap();
        let started = Instant::now();
        let peer = match aterm_uds::accept(
            listener,
            aterm_uds::AcceptWait::Until(started + MUST_HAPPEN),
            aterm_uds::AcceptedMode::Nonblocking,
        ) {
            Ok((accepted, peer)) => {
                drop(accepted);
                peer
            }
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                panic!(
                    "the connect from {sent_from} never reached the listener on {on} \
                         (another socket on port {}, bound to {} or a wildcard, would take it)",
                    on.port(),
                    sent_from.ip(),
                );
            }
            Err(e) => panic!("accepting on {on}: {e}"),
        };
        assert_eq!(
            (peer.ip().to_canonical(), peer.port()),
            (sent_from.ip(), sent_from.port()),
            "the listener on {on} accepted the probe's connection"
        );
        peer
    }

    /// THE SHADOWED PORT, live: beside a drawn listener on `[::]:Q`, macOS binds a listener on
    /// 127.0.0.1:Q, a connect to 127.0.0.1:Q reaches it and never the wildcard, and the draw's
    /// check reads the port as held; no IPv4 wildcard can bind Q beside the drawn listener.
    /// NEGATIVE CONTROL (run): with the 127.0.0.1 listener closed, the port reads as free.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_port_another_socket_holds_on_loopback_is_passed_over() {
        let test = "a_port_another_socket_holds_on_loopback_is_passed_over";
        let (wildcard, port) = dual_stack_listener(test);
        assert!(
            matches!(
                TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)),
                Err(e) if e.kind() == io::ErrorKind::AddrInUse
            ),
            "no IPv4 wildcard outranks the drawn listener once its draw closed"
        );
        let held = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let holder = TcpListener::bind(held).expect("macOS binds 127.0.0.1 beside [::]");
        let probe = TcpStream::connect_timeout(&held, MUST_HAPPEN).unwrap();
        // The more specific listener takes the connect: the wildcard would wait for it forever.
        accept_probe(&holder, &probe);
        assert_eq!(held_on_loopback(port), Some(held));
        drop((probe, holder));
        assert_eq!(
            held_on_loopback(port),
            None,
            "only the wildcard holds the port now"
        );
        drop(wildcard);
    }

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

        let crowded = slots(&[(a, true); CAP]);
        assert_eq!(pick_displaced(&crowded, b, fresh, GRACE), Some(0));
        assert_eq!(pick_displaced(&crowded, a, fresh, GRACE), None);
        assert_eq!(pick_displaced(&crowded, a, grown, GRACE), Some(0));

        let mut mixed = vec![(c, false); CAP - 3];
        mixed.extend([(b, true), (a, true), (a, true)]);
        let mixed = slots(&mixed);
        assert_eq!(pick_displaced(&mixed, b, fresh, GRACE), None);
        assert_eq!(pick_displaced(&mixed, c, fresh, GRACE), Some(CAP - 2));
        assert_eq!(pick_displaced(&mixed, b, grown, GRACE), Some(CAP - 3));

        let tied = slots(&[(b, true), (a, true), (a, true), (b, true)]);
        assert_eq!(pick_displaced(&tied, c, fresh, GRACE), Some(0));

        let relays = slots(&[(a, false); CAP]);
        for newcomer in [a, b, c] {
            assert_eq!(pick_displaced(&relays, newcomer, grown, GRACE), None);
        }

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

    #[test]
    fn an_eviction_cuts_only_a_connection_that_has_not_authenticated() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let pair = || {
            let accepting = {
                let listener = listener.try_clone().unwrap();
                std::thread::spawn(move || {
                    aterm_uds::accept(
                        &listener,
                        aterm_uds::AcceptWait::Forever,
                        aterm_uds::AcceptedMode::Blocking,
                    )
                    .unwrap()
                    .0
                })
            };
            let peer = TcpStream::connect(addr).unwrap();
            let tcp = accepting.join().unwrap();
            tcp.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
            (tcp, peer)
        };

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

    #[test]
    fn slots_conform_to_net_slot_displacement() {
        use aterm_models_core::net_slot_displacement_model;
        use aterm_spec::interp::with_consts;
        use aterm_spec::verify::validate_transition_tiered;
        use std::collections::BTreeMap;

        type State = BTreeMap<&'static str, i64>;

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
        let accept = |conns: &mut Vec<Conn>, source: IpAddr, deadline: Duration| {
            assert!(
                slots.make_room(source, &aterm_uds::stop::Stop::new()),
                "a slot is free"
            );
            let peer = TcpStream::connect(addr).unwrap();
            let (tcp, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
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

        for _ in 0..CAP {
            accept(&mut conns, a, never);
            check(&mut seen, "Accept", project(&slots, &conns, false));
        }

        assert_eq!(conns[0].watchdog.take().unwrap().finish(), Claim::Authed);
        check(&mut seen, "Authenticate", project(&slots, &conns, false));

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

        let newcomer = std::thread::spawn({
            let slots = Arc::clone(&slots);
            move || slots.make_room(b, &aterm_uds::stop::Stop::new())
        });
        until("the eviction", &|| conns[1].claim() == WD_EVICTED);
        check(&mut seen, "Evict", project(&slots, &conns, true));
        assert!(!newcomer.join().unwrap(), "no slot freed: refused");
        check(&mut seen, "Refuse", project(&slots, &conns, false));
        assert_eq!(conns[1].watchdog.take().unwrap().finish(), Claim::Evicted);
        until("the eviction's close at the dialer", &|| conns[1].closed());
        conns[1].slot = None;
        check(&mut seen, "Release", project(&slots, &conns, false));

        let late = conns.len();
        accept(&mut conns, a, Duration::from_millis(250));
        let now = project(&slots, &conns, false);
        if conns[late].claim() == WD_FIRED && now["cut"] > seen["cut"] {
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

        let newcomer = std::thread::spawn({
            let slots = Arc::clone(&slots);
            move || slots.make_room(b, &aterm_uds::stop::Stop::new())
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

        conns[0].slot = None;
        check(&mut seen, "Close", project(&slots, &conns, false));
        assert!(!conns[0].closed(), "nothing ever closed the relay's socket");

        for conn in &mut conns {
            if let Some(watchdog) = conn.watchdog.take() {
                conn.evictor.evict();
                watchdog.finish();
            }
        }
    }
}
