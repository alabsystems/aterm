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
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
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

/// Maximum connections [`serve`] keeps in flight at once (handshaking + relaying).
/// A flood beyond this is refused rather than allowed to spawn unbounded threads
/// and file descriptors. Bounds total resource use to `MAX_INFLIGHT × (2 threads
/// + 2 fds)`.
const MAX_INFLIGHT: usize = 64;

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

/// Decrements the in-flight counter when a connection's handler thread ends
/// (including on panic), so a refused/finished connection always frees its slot.
struct InFlightGuard(Arc<AtomicUsize>);
impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

// Unauthenticated-phase watchdog state. A per-syscall `SO_RCVTIMEO` only bounds an
// IDLE stall — a peer that dribbles one byte just under the timeout keeps resetting
// it. So a watchdog thread enforces a TOTAL WALL-CLOCK deadline: after it elapses it
// force-closes the socket unless the handshake+AUTH already finished. The tri-state
// is claimed by exactly one side (CAS), so the watchdog never shuts a socket the
// relay is about to use.
const WD_RUNNING: u8 = 0;
const WD_AUTHED: u8 = 1;
const WD_FIRED: u8 = 2;

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
/// lengthen or starve it. Exactly one of {authed, fired} wins the CAS, so the
/// watchdog never shuts a socket the relay is about to use.
struct HandshakeWatchdog {
    /// The tri-state claimed by exactly one of the handshake and the watchdog.
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
    /// If the socket cannot be cloned (the caller then never starts the handshake).
    fn arm(tcp: &TcpStream, handshake_timeout: Duration) -> io::Result<Self> {
        let state = Arc::new(AtomicU8::new(WD_RUNNING));
        let wd_sock = tcp.try_clone()?;
        let wd_state = Arc::clone(&state);
        #[cfg(test)]
        let waiting = Arc::new(std::sync::atomic::AtomicBool::new(false));
        #[cfg(test)]
        let wd_waiting = Arc::clone(&waiting);
        let thread = std::thread::spawn(move || {
            let start = Instant::now();
            loop {
                if wd_state.load(Ordering::Acquire) != WD_RUNNING {
                    return; // finished early — nothing to cut off
                }
                // Remaining time measured from ARMING, so a spurious wake or an
                // `unpark` that raced a state store cannot move the deadline.
                let Some(remaining) = handshake_timeout.checked_sub(start.elapsed()) else {
                    break; // deadline reached
                };
                if remaining.is_zero() {
                    break;
                }
                #[cfg(test)]
                wd_waiting.store(true, Ordering::Release);
                std::thread::park_timeout(remaining);
            }
            // Deadline reached: claim FIRED iff still running, then force-close so
            // the blocked handshake/AUTH read errors out.
            if wd_state
                .compare_exchange(WD_RUNNING, WD_FIRED, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let _ = wd_sock.shutdown(std::net::Shutdown::Both);
            }
        });
        Ok(Self {
            state,
            thread,
            #[cfg(test)]
            waiting,
        })
    }

    /// Claim [`WD_AUTHED`] iff the watchdog has not already fired, wake it, and
    /// JOIN it — so its socket clone is dropped before the caller touches the
    /// connection again. Returns whether the unauthenticated phase beat the
    /// deadline; `false` means the watchdog already claimed `WD_FIRED` and the
    /// caller must tear the connection down.
    ///
    /// The state is published by the `AcqRel` compare-exchange BEFORE the wake, so
    /// the watchdog's re-check on waking always observes it; and `unpark` issued
    /// before the thread parks leaves a token that makes its next `park_timeout`
    /// return at once, so the wake can never be lost to that race.
    fn finish(self) -> bool {
        let authed_in_time = self
            .state
            .compare_exchange(WD_RUNNING, WD_AUTHED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        self.thread.thread().unpark();
        let _ = self.thread.join();
        authed_in_time
    }
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
    // Per-syscall floor: bounds a single fully-idle read/write (defense in depth).
    tcp.set_read_timeout(Some(handshake_timeout))?;
    tcp.set_write_timeout(Some(handshake_timeout))?;

    // Total wall-clock deadline on the handshake + AUTH read, which a peer that
    // dribbles bytes cannot reset ([`HandshakeWatchdog`]).
    let watchdog = HandshakeWatchdog::arm(&tcp, handshake_timeout)?;

    // The unauthenticated phase. Any error here (incl. the watchdog's force-close)
    // tears the connection down.
    let unauth = (|| -> io::Result<(TlsTransport<rustls::ServerConnection>, Granted)> {
        let mut transport = tls::accept(tcp, config)?;
        let exporter = transport.exporter().to_vec();
        let granted = verify_capability(transport.stream(), &exporter, lookup)?;
        Ok((transport, granted))
    })();
    let authed_in_time = watchdog.finish();

    // A deadline — the watchdog's force-close (which the blocked read sees as an
    // end of stream) or the per-syscall timer — is said as one, never as the
    // `unexpected end of file` it surfaces as.
    let (mut transport, granted) = match unauth {
        Ok(v) if authed_in_time => v,
        Err(e) if authed_in_time && !is_deadline(&e) => return Err(e),
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
/// **DoS bounds.** At most [`MAX_INFLIGHT`] connections are handled concurrently;
/// a flood beyond that is refused (logged, dropped) rather than allowed to spawn
/// unbounded threads/fds. Each accepted connection's unauthenticated phase is
/// deadline-bounded inside [`accept_and_relay`] ([`HANDSHAKE_TIMEOUT`]), so a
/// slow-loris cannot pin a handler thread. (An AUTHENTICATED peer that then idles
/// holds one slot until it closes; the cap bounds the worst case — these are
/// token-holders the operator already trusts.)
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
    let inflight = Arc::new(AtomicUsize::new(0));

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

        // Concurrency cap: reserve a slot; if we are already at the ceiling, roll
        // back and refuse rather than spawn unbounded work.
        if inflight.fetch_add(1, Ordering::SeqCst) >= MAX_INFLIGHT {
            inflight.fetch_sub(1, Ordering::SeqCst);
            on_event(NetEvent::Refused {
                peer,
                reason: "the listener is at capacity".to_owned(),
            });
            continue; // dropping `tcp` closes the connection
        }
        let guard = InFlightGuard(Arc::clone(&inflight));

        let config = Arc::clone(config);
        let lookup = lookup.clone();
        let connect_local = connect_local.clone();
        let on_event = on_event.clone();
        std::thread::spawn(move || {
            let _slot = guard; // released (slot freed) when this thread ends
            let admitted = std::cell::Cell::new(false);
            let result = accept_and_relay_inner(
                tcp,
                config,
                lookup,
                connect_local,
                HANDSHAKE_TIMEOUT,
                |granted| {
                    admitted.set(true);
                    on_event(NetEvent::Admitted {
                        peer,
                        granted: granted.clone(),
                    });
                },
            );
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
    if watchdog.finish() {
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
    let authed_in_time = watchdog.finish();

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
    use std::sync::Mutex;

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
                assert!(
                    watchdog.finish(),
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
        assert!(
            !watchdog.finish(),
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
}
