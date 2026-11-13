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

/// The failed-accept retry bound, and how long after a route or interface change it heard a
/// LAN listener looks once more (`lan::Listener::validity_deadline`). A listener idles on this
/// only on macOS with no kept read of its attachments (`lan::unheard_look`), and off Unix;
/// slot displacement also retains this bounded release wait.
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
#[path = "slots_tests.rs"]
pub(crate) mod tests;
