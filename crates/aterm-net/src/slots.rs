// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Network listener slot admission ([`Slots`], [`pick_displaced`]) and unauthenticated
//! [`HandshakeWatchdog`] deadline for [`crate::fleet`].

use std::io;
use std::net::{IpAddr, Ipv6Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// How often a listener's accept loop wakes to re-check its `running` flag.
pub(crate) const ACCEPT_POLL: Duration = Duration::from_millis(200);

#[cfg(any(unix, windows))]
fn poll_timeout_millis(timeout: Duration) -> i32 {
    let millis = timeout.as_millis().saturating_add(u128::from(
        !timeout.subsec_nanos().is_multiple_of(1_000_000),
    ));
    i32::try_from(millis).unwrap_or(i32::MAX)
}

/// Wait until a non-blocking listener may accept, or until `timeout` expires.
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
        // SAFETY: `descriptor` is one initialized pollfd valid for the call; `listener` keeps its
        // borrowed descriptor open for the whole wait.
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
    use aterm_win32::Win32::Networking::WinSock::{
        POLLIN, SOCKET_ERROR, WSAGetLastError, WSAPOLLFD, WSAPoll,
    };
    use std::os::windows::io::AsRawSocket as _;

    let mut descriptor = WSAPOLLFD {
        fd: listener.as_raw_socket() as usize,
        events: POLLIN,
        revents: 0,
    };
    // SAFETY: std initialized Winsock before it created `listener`; `descriptor` names that live
    // borrowed socket for the duration of the call.
    let result = unsafe {
        WSAPoll(
            std::ptr::from_mut(&mut descriptor),
            1,
            poll_timeout_millis(timeout),
        )
    };
    if result == SOCKET_ERROR {
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
    std::thread::sleep(timeout);
    Ok(false)
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

    /// Whether a newcomer from `source` gets a slot.
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
        }
        let (held, _) = self
            .freed
            .wait_timeout_while(held, ACCEPT_POLL, |held| held.len() >= self.capacity)
            .unwrap_or_else(PoisonError::into_inner);
        held.len() < self.capacity
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

/// The watchdog's claim at its deadline.
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

/// The unauthenticated-phase watchdog.
pub(crate) struct HandshakeWatchdog {
    /// The state claimed by exactly one of the handshake, the watchdog and an [`Evictor`].
    state: Arc<AtomicU8>,
    /// The parked watchdog thread.
    thread: std::thread::JoinHandle<()>,
    /// Set by the thread each time it is about to wait.
    #[cfg(test)]
    waiting: Arc<std::sync::atomic::AtomicBool>,
}

impl HandshakeWatchdog {
    /// Arm the deadline on a clone of `tcp`.
    ///
    /// # Errors
    /// If the socket cannot be cloned or the watchdog thread cannot start.
    pub(crate) fn arm(tcp: &TcpStream, handshake_timeout: Duration) -> io::Result<Self> {
        let state = Arc::new(AtomicU8::new(WD_RUNNING));
        let wd_sock = tcp.try_clone()?;
        let wd_state = Arc::clone(&state);
        #[cfg(test)]
        let waiting = Arc::new(std::sync::atomic::AtomicBool::new(false));
        #[cfg(test)]
        let wd_waiting = Arc::clone(&waiting);
        let thread = std::thread::Builder::new().spawn(move || {
            let start = Instant::now();
            let cut_off = loop {
                match wd_state.load(Ordering::Acquire) {
                    WD_RUNNING => {}
                    WD_EVICTED => break true,
                    _ => break false, // finished early — nothing to cut off
                }
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

    /// Claim [`WD_AUTHED`] iff no deadline or eviction claimed the phase first, wake the watchdog.
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
        listener.set_nonblocking(true).unwrap();
        let waiting_listener = listener.try_clone().unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
        let waiter = std::thread::spawn(move || {
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
        const DEADLINE: Duration = Duration::from_millis(150);
        const HAMMER_BUDGET: Duration = Duration::from_secs(180);
        const CEILING: Duration = MUST_HAPPEN;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let accepting = std::thread::spawn(move || listener.accept().unwrap().0);
        let tcp = TcpStream::connect(addr).unwrap();
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
    /// listener closes it, until dropped.
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

    /// A listener on `[::]:0` that takes IPv4 too. A Unix IPv6 socket does by
    /// default (`IPV6_V6ONLY` off).
    #[cfg(not(windows))]
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
        fn not_run(test: &str, why: &str) -> ! {
            aterm_spec::host::could_not_run(format!("{test}: no dual-stack loopback here ({why})"))
        }
        let listener = match bind_dual_stack() {
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
                std::thread::spawn(move || listener.accept().unwrap().0)
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
