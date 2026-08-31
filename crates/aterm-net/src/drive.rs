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
//! * **Driver** ([`dial_and_relay`]) — the host doing the driving. Dials a pinned
//!   endpoint, presents the channel-bound capability, then relays a local control
//!   client to the remote. The network analog of `proxy::connect_and_relay`.
//!
//! **Secure-default-OFF**: nothing here runs unless a caller explicitly stands up
//! a listener with an operator-provided cert+key and a capability lookup. There is
//! no implicit bind, no default port, no ambient authority.

use std::io;
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aterm_session::EdgeToken;
use aterm_uds::CtlStream;
use rustls::{ClientConfig, ServerConfig};

use crate::tls::{self, TlsTransport};
use crate::{Granted, RemoteEndpoint, present_capability, verify_capability};

/// What happened on one accepted connection — surfaced to the caller's logger so
/// the network listener has an auditable trail (mirrors the local socket's
/// `log_denial`). Never carries a secret.
#[derive(Clone, Debug)]
pub enum NetEvent {
    /// A dialer verified and was relayed to the local control socket.
    Relayed(Granted),
    /// A connection was refused (bad handshake, denied capability, dial error).
    /// The string is a non-sensitive reason for the audit log.
    Rejected(String),
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
/// arm one; keeping it in a single type is what stops the two copies drifting.
///
/// **Event-driven, not polled.** The thread parks for the WHOLE remaining deadline
/// in one go and [`finish`](Self::finish) `unpark`s it the instant the handshake
/// claims `WD_AUTHED`, so a connection that authenticates early — loopback/LAN,
/// i.e. the normal case — does not wait out a poll interval before the join
/// returns. It previously slept in 50 ms steps, which made that whole step the
/// connection-setup latency (~55 ms loopback vs ~0.6 ms for the same handshake
/// with no watchdog).
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
        Ok(Self { state, thread })
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
    accept_and_relay_inner(tcp, config, lookup, connect_local, HANDSHAKE_TIMEOUT)
}

/// [`accept_and_relay`] with the unauthenticated-phase deadline injected (tests
/// use a short value to exercise the slow-loris timeout without a real wait).
fn accept_and_relay_inner<F, G>(
    tcp: TcpStream,
    config: Arc<ServerConfig>,
    lookup: F,
    connect_local: G,
    handshake_timeout: Duration,
) -> io::Result<Granted>
where
    F: FnOnce(&str, &str) -> Option<EdgeToken>,
    G: FnOnce() -> io::Result<CtlStream>,
{
    // Per-syscall floor: bounds a single fully-idle read/write (defense in depth).
    tcp.set_read_timeout(Some(handshake_timeout))?;
    tcp.set_write_timeout(Some(handshake_timeout))?;

    // Total wall-clock deadline on the whole unauthenticated phase (handshake +
    // AUTH read): a watchdog force-closes a clone of the socket once the deadline
    // elapses, so a peer that DRIBBLES bytes (resetting the per-syscall timer) is
    // still cut off. Exactly one of {authed, fired} wins the CAS, so the watchdog
    // never shuts a socket the relay will use.
    let watchdog = HandshakeWatchdog::arm(&tcp, handshake_timeout)?;

    // The unauthenticated phase. Any error here (incl. the watchdog's force-close)
    // tears the connection down.
    let unauth = (|| -> io::Result<(TlsTransport<rustls::ServerConnection>, Granted)> {
        let mut transport = tls::accept(tcp, config)?;
        let exporter = transport.exporter().to_vec();
        let granted = verify_capability(transport.stream(), &exporter, lookup)?;
        Ok((transport, granted))
    })();
    // Claim AUTHED iff the watchdog has not already fired; then wake and join it
    // (so its socket clone is dropped before we touch the connection again).
    let authed_in_time = watchdog.finish();

    let (mut transport, granted) = match unauth {
        Ok(v) if authed_in_time => v,
        Ok(_) => return Err(io::Error::other("handshake deadline exceeded")),
        Err(e) => return Err(e),
    };

    // Authenticated: drop the handshake deadline before the (long-lived) relay,
    // which installs its own non-blocking poll on the same socket.
    {
        let sock = transport.stream().get_mut();
        sock.set_read_timeout(None)?;
        sock.set_write_timeout(None)?;
    }
    // NOW (and only now) dial the local control socket and bridge the two.
    let local = connect_local()?;
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
        on_event(NetEvent::Rejected(format!(
            "could not configure listener readiness: {error}"
        )));
        return;
    }
    let inflight = Arc::new(AtomicUsize::new(0));

    while running.load(Ordering::Relaxed) {
        let tcp = match listener.accept() {
            Ok((s, _)) => s,
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
            on_event(NetEvent::Rejected("listener at capacity".to_owned()));
            continue; // dropping `tcp` closes the connection
        }
        let guard = InFlightGuard(Arc::clone(&inflight));

        let config = Arc::clone(config);
        let lookup = lookup.clone();
        let connect_local = connect_local.clone();
        let on_event = on_event.clone();
        std::thread::spawn(move || {
            let _slot = guard; // released (slot freed) when this thread ends
            let ev = match accept_and_relay(tcp, config, lookup, connect_local) {
                Ok(granted) => NetEvent::Relayed(granted),
                Err(e) => NetEvent::Rejected(e.to_string()),
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
/// **Identity pinning — what this enforces, and what it does not.** The server
/// CERTIFICATE fingerprint IS enforced: `config` (from [`tls::client_config`])
/// rejects the handshake unless the peer presents the pinned cert AND proves key
/// possession, so a redirected/MITM endpoint fails before any secret crosses the
/// wire. The session-NONCE half of the rebind guard
/// ([`RemoteEndpoint::matches`](crate::RemoteEndpoint::matches)) is NOT enforced by
/// THIS entry point — it presents the capability and relays with no nonce check, so
/// the un-pinned dial path is byte-identical to before the pin existed. A caller
/// that wants the rebind guard uses [`dial_and_relay_pinned`], which enforces
/// `matches` before relaying. (Channel binding still makes a stale capability
/// useless against a relaunched session: a different session ⇒ a different TLS
/// exporter ⇒ the tag fails to verify — so this is a defense-in-depth gap, not an
/// auth bypass.)
///
/// # Errors
/// On a connect/TLS/handshake failure (incl. cert-pin mismatch) or a denied
/// capability (before any relay). A relay-stage I/O error after a successful
/// present is returned too, but by then the capability HAS been accepted and bytes
/// may have flowed.
pub fn dial_and_relay<A: ToSocketAddrs>(
    addr: A,
    config: Arc<ClientConfig>,
    src: &str,
    op: &str,
    token: &EdgeToken,
    prebuffer: &[u8],
    local: CtlStream,
) -> io::Result<()> {
    dial_and_relay_inner(
        addr,
        config,
        src,
        op,
        token,
        prebuffer,
        local,
        HANDSHAKE_TIMEOUT,
    )
}

/// [`dial_and_relay`] with the session-NONCE rebind guard ENFORCED when `pin` is
/// `Some`. The cert-fingerprint half of the pin is already TLS-enforced (`config`
/// rejects any peer that is not the pinned cert), so the remaining check is the
/// launch-nonce half of [`RemoteEndpoint::matches`]: after the capability is
/// presented and BEFORE any control bytes relay, the remote's live launch nonce is
/// read and `matches` is required to hold.
///
/// **Fail-closed.** The shipping wire protocol does not yet carry a launch-identity
/// echo, so the live nonce is currently UNOBSERVABLE
/// ([`observe_launch_nonce_unavailable`]). A configured `pin` therefore refuses to
/// dial rather than relay unverified — a pin the operator asked for is never
/// silently skipped. When the listener grows a `LaunchNonce` echo, only the
/// observer is replaced; this enforcement point is unchanged.
///
/// `pin == None` is exactly [`dial_and_relay`] (no nonce read, byte-identical relay).
///
/// # Errors
/// Everything [`dial_and_relay`] returns, plus — when `pin` is `Some` — a
/// nonce-mismatch or unobservable-nonce error (both BEFORE any relay), so a
/// relaunched/rebound session is never driven by a stale pin.
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
        pin,
        observe_launch_nonce_unavailable,
    )
}

/// Read the remote's live launch nonce for the [`RemoteEndpoint::matches`] rebind
/// check. The shipping wire protocol does not yet carry a launch-identity echo (the
/// listener does not send its `LaunchNonce`), so this returns `None` — which makes
/// [`dial_and_relay_pinned`] FAIL CLOSED when a nonce pin is configured, rather than
/// silently skip the check. Replace this with a real read once the listener echoes
/// its launch identity; the enforcement point in [`dial_and_relay_pinned_inner`] is
/// then unchanged.
fn observe_launch_nonce_unavailable(
    _transport: &mut TlsTransport<rustls::ClientConnection>,
) -> io::Result<Option<String>> {
    Ok(None)
}

/// [`dial_and_relay`] with the unauthenticated-phase deadline injected (tests use a
/// short value to exercise the dribbling-server timeout without a real wait). The
/// un-pinned path: delegates to [`dial_and_relay_pinned_inner`] with no pin, so no
/// launch-nonce read happens and the relay is byte-identical to the original.
#[allow(clippy::too_many_arguments)]
fn dial_and_relay_inner<A: ToSocketAddrs>(
    addr: A,
    config: Arc<ClientConfig>,
    src: &str,
    op: &str,
    token: &EdgeToken,
    prebuffer: &[u8],
    local: CtlStream,
    handshake_timeout: Duration,
) -> io::Result<()> {
    dial_and_relay_pinned_inner(
        addr,
        config,
        src,
        op,
        token,
        prebuffer,
        local,
        handshake_timeout,
        None,
        observe_launch_nonce_unavailable,
    )
}

/// [`dial_and_relay_pinned`] with the unauthenticated-phase deadline AND the
/// launch-nonce observer injected. `observe_nonce` yields the remote's live launch
/// nonce (`None` ⇒ unobservable ⇒ a configured pin fails closed); tests inject a
/// matching/mismatching nonce to exercise the rebind guard without a wire echo. When
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

    // Total wall-clock deadline on the whole unauthenticated phase (TLS handshake +
    // the OK/DENIED verdict read inside `present_capability`): a watchdog force-closes
    // a clone of the socket once the deadline elapses, so a pinned-but-compromised
    // peer that DRIBBLES bytes (resetting the per-syscall timer) is still cut off.
    // Exactly one of {authed, fired} wins the CAS, so the watchdog never shuts a
    // socket the relay will use. The clone MUST be taken before `tcp` is moved into
    // `tls::connect`.
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
    // Claim AUTHED iff the watchdog has not already fired; then wake and join it
    // (so its socket clone is dropped before we touch the connection again).
    let authed_in_time = watchdog.finish();

    let mut transport = match unauth {
        Ok(t) if authed_in_time => t,
        Ok(_) => return Err(io::Error::other("handshake deadline exceeded")),
        Err(e) => return Err(e),
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
    // remains, read here via `observe_nonce`. A pin that cannot be verified (no wire
    // echo yet ⇒ `None`) FAILS CLOSED — an operator-requested guard is never silently
    // skipped. `pin == None` never touches `observe_nonce`, so the un-pinned path is
    // byte-identical to `dial_and_relay`.
    if let Some(pin) = &pin {
        match observe_nonce(&mut transport)? {
            Some(nonce) if pin.matches(&pin.fingerprint, &nonce) => {}
            Some(_) => {
                return Err(io::Error::other(
                    "launch-nonce pin mismatch: the remote is a relaunched/rebound session; \
                     refusing to relay",
                ));
            }
            None => {
                return Err(io::Error::other(
                    "connection pins a launch nonce but the remote supplied no launch identity \
                     (the wire echo is not yet implemented); refusing to dial (fail-closed)",
                ));
            }
        }
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
    use std::io::{Read, Write};
    use std::sync::Mutex;

    const TEST_CERT_DER: &[u8] = include_bytes!("testdata/cert.der");
    const TEST_KEY_DER: &[u8] = include_bytes!("testdata/key.pkcs8.der");

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
        // Deliberately much longer than production's 200 ms bound: a sleep-based
        // implementation pays all five seconds and fails by a wide margin. The
        // two-second wake ceiling still distinguishes that negative control but
        // tolerates severe scheduler starvation in a parallel debug test run.
        const TEST_WAIT: Duration = Duration::from_secs(5);
        const MAX_READY_WAKE: Duration = Duration::from_secs(2);

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
            elapsed < Duration::from_secs(2),
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
            dial_and_relay(addr, ccfg, "driver-1", "drive", &token, b"PRE\n", drv_local)
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
            dial_and_relay(addr, ccfg, "dial", "drive", &token, b"", drv_local)
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
                )
            }
        });

        // Connect, then stall (send nothing, hold the socket open).
        let _stalled = TcpStream::connect(addr).unwrap();
        let started = Instant::now();
        let res = host.join().unwrap();
        assert!(
            res.is_err(),
            "a stalled pre-auth peer must be dropped, not relayed"
        );
        assert!(
            !dialed_local.load(Ordering::SeqCst),
            "a peer that never authenticated must NEVER dial the local socket"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
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
                )
            }
        });

        // A valid TLS record HEADER (handshake, len=4096) so rustls keeps waiting
        // for the body — then dribble it 1 byte / 80ms, well under the 300ms
        // per-syscall timeout, so ONLY the wall-clock watchdog can stop it.
        let mut peer = TcpStream::connect(addr).unwrap();
        let started = Instant::now();
        let dribbler = std::thread::spawn(move || {
            let _ = peer.write_all(&[0x16, 0x03, 0x03, 0x10, 0x00]);
            let _ = peer.flush();
            for _ in 0..40 {
                if peer.write_all(&[0x00]).and_then(|()| peer.flush()).is_err() {
                    break; // server force-closed -> our writes start failing
                }
                std::thread::sleep(Duration::from_millis(80));
            }
        });

        let res = host.join().unwrap();
        let elapsed = started.elapsed();
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
            // Upper bound. The watchdog is EVENT-DRIVEN: it parks for the whole
            // remaining deadline in one go, so it force-closes at the deadline
            // itself rather than at the next tick after it — which is why this
            // bound is 5s against a 300ms deadline and no longer the 30s the
            // sleep-polled shape needed. Without the watchdog the dial hangs for
            // the whole handshake timeout, so this still fails loudly on
            // 'runs unbounded' — AND now also on 'the wake starved the deadline'.
            elapsed < Duration::from_secs(5),
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
        // so ONLY the wall-clock watchdog can stop the dial.
        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            let _ = peer.write_all(&[0x16, 0x03, 0x03, 0x10, 0x00]);
            let _ = peer.flush();
            for _ in 0..40 {
                if peer.write_all(&[0x00]).and_then(|()| peer.flush()).is_err() {
                    break; // dialer force-closed -> our writes start failing
                }
                std::thread::sleep(Duration::from_millis(80));
            }
        });

        let (drv_local, _drv_client) = CtlStream::pair().unwrap();
        let started = Instant::now();
        // 300ms deadline (vs the 10s default) so the test is fast.
        let res = dial_and_relay_inner(
            addr,
            ccfg,
            "dial",
            "drive",
            &token,
            b"",
            drv_local,
            Duration::from_millis(300),
        );
        let elapsed = started.elapsed();
        assert!(
            res.is_err(),
            "a dribbling server must be cut off, not left to hang the dial"
        );
        assert!(
            elapsed >= Duration::from_millis(200),
            "the cut-off must be the wall-clock watchdog (~300ms), not an instant reject ({elapsed:?})"
        );
        assert!(
            // Upper bound. The watchdog is EVENT-DRIVEN: it parks for the whole
            // remaining deadline in one go, so it force-closes at the deadline
            // itself rather than at the next tick after it — which is why this
            // bound is 5s against a 300ms deadline and no longer the 30s the
            // sleep-polled shape needed. Without the watchdog the dial hangs for
            // the whole handshake timeout, so this still fails loudly on
            // 'runs unbounded' — AND now also on 'the wake starved the deadline'.
            elapsed < Duration::from_secs(5),
            "the wall-clock deadline must bound the dribble, not run unbounded ({elapsed:?})"
        );
        let _ = server.join();
    }

    /// One loopback connection through the SHIPPING drive path (a watchdog armed on
    /// BOTH ends), timed from "start connecting" to "the first control byte has made
    /// the full round trip through the authenticated relay" — the span a
    /// `dial <name> <verb>` actually pays, since aterm-ctl rejects a bare
    /// `dial <name>` and every remote verb is therefore a fresh TCP+TLS connection.
    fn time_drive_setup(listener: &TcpListener, token: EdgeToken) -> Duration {
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let ccfg = client_config(cert_fingerprint(TEST_CERT_DER));

        let (svc_a, svc_b) = CtlStream::pair().unwrap();
        let echo = spawn_echo(svc_b);
        let svc_a = Arc::new(Mutex::new(Some(svc_a)));

        // Park the host in accept() BEFORE the clock starts: we time the connection,
        // not thread creation on the listener side.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let listener_clone = listener.try_clone().unwrap();
        let host = std::thread::spawn({
            let svc_a = Arc::clone(&svc_a);
            move || {
                ready_tx.send(()).unwrap();
                let (tcp, _) = listener_clone.accept().unwrap();
                accept_and_relay(
                    tcp,
                    scfg,
                    |_s, op| (op == "drive").then_some(token),
                    move || Ok(svc_a.lock().unwrap().take().unwrap()),
                )
            }
        });
        ready_rx.recv().unwrap();

        let (drv_local, mut drv_client) = CtlStream::pair().unwrap();
        let started = Instant::now();
        let driver = std::thread::spawn(move || {
            dial_and_relay(addr, ccfg, "probe", "drive", &token, b"", drv_local)
        });
        drv_client.write_all(b"P\n").unwrap();
        drv_client.flush().unwrap();
        let mut got = [0u8; 2];
        drv_client.read_exact(&mut got).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(&got, b"P\n");

        drv_client.shutdown(std::net::Shutdown::Both).unwrap();
        drop(drv_client);
        let granted = host.join().unwrap().unwrap();
        // Reach witness for the ARM: reaching `Granted` means the capability gate
        // inside `accept_and_relay_inner` ran, and that function arms a watchdog
        // unconditionally — so this sample provably contains the state under test.
        assert_eq!(granted.op, "drive");
        driver.join().unwrap().ok();
        echo.join().ok();
        elapsed
    }

    /// The CONTROL: the identical TLS 1.3 handshake, channel-bound capability
    /// exchange and relay, assembled inline with NO watchdog. Nothing here calls
    /// `accept_and_relay`/`dial_and_relay`, so no watchdog is ever armed — this is
    /// the transport floor the drive path is measured against.
    fn time_bare_handshake(listener: &TcpListener, token: EdgeToken) -> Duration {
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let ccfg = client_config(cert_fingerprint(TEST_CERT_DER));
        let timeout = Duration::from_secs(10);

        let (svc_a, svc_b) = CtlStream::pair().unwrap();
        let echo = spawn_echo(svc_b);

        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let listener_clone = listener.try_clone().unwrap();
        let host = std::thread::spawn(move || -> io::Result<()> {
            ready_tx.send(()).unwrap();
            let (tcp, _) = listener_clone.accept().unwrap();
            tcp.set_read_timeout(Some(timeout))?;
            tcp.set_write_timeout(Some(timeout))?;
            let mut transport = tls::accept(tcp, scfg)?;
            let exporter = transport.exporter().to_vec();
            let _granted = verify_capability(transport.stream(), &exporter, |_s, op| {
                (op == "drive").then_some(token)
            })?;
            {
                let sock = transport.stream().get_mut();
                sock.set_read_timeout(None)?;
                sock.set_write_timeout(None)?;
            }
            tls::relay(transport, svc_a)
        });
        ready_rx.recv().unwrap();

        let (drv_local, mut drv_client) = CtlStream::pair().unwrap();
        let started = Instant::now();
        let driver = std::thread::spawn(move || -> io::Result<()> {
            let tcp = TcpStream::connect_timeout(&addr, timeout)?;
            tcp.set_read_timeout(Some(timeout))?;
            tcp.set_write_timeout(Some(timeout))?;
            let mut transport = tls::connect(tcp, tls::fixed_server_name(), ccfg)?;
            let exporter = transport.exporter().to_vec();
            present_capability(transport.stream(), &exporter, "probe", "drive", &token)?;
            {
                let sock = transport.stream().get_mut();
                sock.set_read_timeout(None)?;
                sock.set_write_timeout(None)?;
            }
            tls::relay(transport, drv_local)
        });
        drv_client.write_all(b"P\n").unwrap();
        drv_client.flush().unwrap();
        let mut got = [0u8; 2];
        drv_client.read_exact(&mut got).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(&got, b"P\n");

        drv_client.shutdown(std::net::Shutdown::Both).unwrap();
        drop(drv_client);
        host.join().unwrap().unwrap();
        driver.join().unwrap().ok();
        echo.join().ok();
        elapsed
    }

    #[test]
    fn a_fast_handshake_is_not_paced_by_the_watchdogs_wake_interval() {
        // TWO-SIDED, and the two arms differ ONLY by the watchdog:
        //   ARM     — `accept_and_relay` + `dial_and_relay`: a watchdog on each end.
        //   CONTROL — the same TLS handshake, capability exchange and relay inline,
        //             with no watchdog at all.
        //
        // The watchdog used to sleep-poll in 50 ms steps and then be JOINED, so a
        // connection whose unauthenticated phase finished early — loopback and LAN,
        // i.e. the normal case — could not complete setup until the CURRENT step
        // expired. Measured on loopback before this became event-driven: the ARM
        // p50 was 55.4 ms against a 0.64 ms CONTROL, so ~99% of connection setup
        // was the poll interval. Event-driven, the two arms coincide.
        //
        // The bound is RELATIVE (arm minus control) so a slow or loaded box moves
        // both arms together and cannot make this flake; 20 ms is under half a
        // 50 ms poll step, so the old shape fails it by ~35 ms every single time.
        const MAX_WATCHDOG_COST: Duration = Duration::from_millis(20);
        const REPS: usize = 3;

        let token = EdgeToken::generate();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        // Warm: the first connection of each shape pays rustls provider init.
        let _ = time_bare_handshake(&listener, token);
        let _ = time_drive_setup(&listener, token);

        // Minimum over the repetitions: for a latency this is the sample least
        // polluted by an unrelated scheduling stall, and the defect under test is
        // deterministic (it is present in EVERY pre-fix sample, never just the tail).
        let control = (0..REPS)
            .map(|_| time_bare_handshake(&listener, token))
            .min()
            .unwrap();
        let arm = (0..REPS)
            .map(|_| time_drive_setup(&listener, token))
            .min()
            .unwrap();

        let cost = arm.saturating_sub(control);
        assert!(
            cost < MAX_WATCHDOG_COST,
            "the unauthenticated-phase watchdog must not pace a fast handshake: \
             drive setup {arm:?} vs the same transport without a watchdog {control:?} \
             (watchdog cost {cost:?}, budget {MAX_WATCHDOG_COST:?}). A cost near a whole \
             wake interval means the watchdog is being waited out instead of woken."
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
        // deadline: not before it, and not pushed out by the hammering.
        const DEADLINE: Duration = Duration::from_millis(150);
        /// Must exceed `CEILING`, or a watchdog whose deadline restarts on every
        /// wake would simply fire once the hammering stopped and escape the test.
        const HAMMER_BUDGET: Duration = Duration::from_secs(3);
        /// Generous against scheduling noise but far under `HAMMER_BUDGET`.
        const CEILING: Duration = Duration::from_millis(1500);

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
                        started.elapsed() < Duration::from_secs(20),
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
            dial_and_relay(
                addr,
                ccfg,
                "driver-1",
                "drive",
                &driver_token,
                b"",
                drv_local,
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
    /// (`None` ⇒ unobservable, the shipping wire's current state). Returns the
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
        // The observer supplies the MATCHING live nonce (standing in for the wire
        // echo the shipping protocol does not yet carry), so the rebind guard holds
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
        assert!(err.to_string().contains("mismatch"), "{err}");
    }

    #[test]
    fn a_pin_with_no_observable_nonce_fails_closed() {
        // The shipping wire carries no launch-identity echo yet, so the live nonce is
        // unobservable (`None`). A configured pin must FAIL CLOSED, never relay
        // unverified — the same posture the production `dial_and_relay_pinned` takes.
        let pin = RemoteEndpoint {
            host: "ignored".into(),
            sid: "s-1".into(),
            nonce: "nonce-pinned".into(),
            fingerprint: "fp".into(),
        };
        let res = dial_with_pin(pin, None);
        let err = res.expect_err("an unverifiable pin must fail closed");
        assert!(err.to_string().contains("fail-closed"), "{err}");
    }
}
