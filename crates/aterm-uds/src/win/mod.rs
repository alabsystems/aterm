// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AF_UNIX over winsock: the Windows [`CtlStream`]/[`CtlListener`].
//!
//! Mirrors the `std::os::unix::net` surface the control channel uses:
//! `connect`/`bind`/`incoming`/`accept`/`pair`/`try_clone`/`shutdown`/
//! `set_read_timeout`/`set_write_timeout` plus `Read`+`Write` for owned and
//! borrowed streams. Two parts differ in mechanism:
//!
//! * `try_clone` shares one socket via `Arc`: afunix does not reliably support
//!   `WSADuplicateSocketW` and `DuplicateHandle` does not support sockets.
//!   Clones behave like Unix `dup`s (`shutdown` and timeouts affect all; the
//!   socket closes when the last clone drops), so dropping a clone closes
//!   nothing.
//! * Timeouts and interruptible waits use `WSAEventSelect`. On afunix a local
//!   `shutdown` does not wake a `recv` blocked on another thread, yet relay
//!   teardown depends on that wake. So every socket is armed with
//!   `WSAEventSelect(FD_READ|FD_WRITE|FD_CLOSE)` (which also makes it
//!   non-blocking) and a blocked read/write parks in `WSAWaitForMultipleEvents`
//!   over `[net_ev, shutdown_ev]`. A caller timeout becomes the wait's timeout
//!   (expiring as `ErrorKind::WouldBlock`); `shutdown()` sets the direction's
//!   `shutdown_ev`, waking the parked thread and yielding EOF (reads) or
//!   `BrokenPipe` (writes), as on Unix. `recv`/`send` treat `WSAEWOULDBLOCK` by
//!   re-arming via `WSAEnumNetworkEvents` and looping.

pub(crate) mod ffi;

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ffi::RawSocket;

/// Call `WSAStartup(2.2)` once, process-wide, before the first socket call.
/// Never paired with `WSACleanup`. std::net's lazy init cannot be relied on:
/// this crate may run before any std socket exists.
fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let mut data = ffi::WsaData { _opaque: [0; 512] };
        // 0x0202 == winsock 2.2. Failure leaves later calls to surface
        // WSANOTINITIALISED as ordinary io::Errors.
        let _ = unsafe { ffi::WSAStartup(0x0202, &mut data) };
    });
}

/// The current thread's last winsock error as an [`io::Error`] (std maps the
/// `WSAE*` codes onto the portable [`io::ErrorKind`]s, e.g.
/// `WSAECONNREFUSED` → `ConnectionRefused`).
fn last_wsa_error() -> io::Error {
    io::Error::from_raw_os_error(unsafe { ffi::WSAGetLastError() })
}

/// Encode `path` into a `sockaddr_un`, stripping a canonicalize-style verbatim
/// prefix (`\\?\` / `\\?\UNC\`) first. afunix reads `sun_path` as UTF-8, so any
/// valid-Unicode path of at most 107 UTF-8 bytes is accepted directly
/// (including non-ASCII user profiles). Longer or non-Unicode paths fall back
/// to the 8.3 short name of the existing parent directory, else fail with
/// actionable advice rather than binding a garbled path.
fn encode_sun_path(path: &Path) -> io::Result<(ffi::SockaddrUn, i32)> {
    if let Some(s) = path.to_str()
        && let Some(addr) = try_encode(s)
    {
        return Ok(addr);
    }
    // Short-path fallback: the parent dir must already exist for
    // GetShortPathNameW; the file name itself must be valid Unicode and fit
    // (aterm's instance-socket names are short ASCII).
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
        && let Some(name) = name.to_str()
        && let Some(short_parent) = short_path(parent)
        && let Some(addr) = try_encode(&format!("{}\\{name}", short_parent.trim_end_matches('\\')))
    {
        return Ok(addr);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "control-socket path {} does not fit AF_UNIX sun_path \
             (at most 107 bytes as UTF-8); pass a shorter --control-sock \
             path",
            path.display()
        ),
    ))
}

/// One encode attempt: strip the verbatim prefix, then require UTF-8 ≤ 107
/// bytes (NUL-terminated in a 108-byte `sun_path`).
fn try_encode(s: &str) -> Option<(ffi::SockaddrUn, i32)> {
    let stripped = strip_verbatim(s);
    let bytes = stripped.as_bytes();
    if bytes.is_empty() || bytes.len() > 107 || bytes.contains(&0) {
        return None;
    }
    let mut addr = ffi::SockaddrUn {
        sun_family: ffi::AF_UNIX as u16,
        sun_path: [0u8; 108],
    };
    addr.sun_path[..bytes.len()].copy_from_slice(bytes);
    Some((addr, std::mem::size_of::<ffi::SockaddrUn>() as i32))
}

/// Strip `std::fs::canonicalize`'s verbatim prefixes: `\\?\C:\x` → `C:\x`,
/// `\\?\UNC\server\share` → `\\server\share`.
fn strip_verbatim(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.to_string()
    }
}

/// The 8.3 short form of an existing path via `GetShortPathNameW`, or `None`
/// if unavailable (volume with 8.3 generation disabled, path missing).
fn short_path(p: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = p
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut buf = vec![0u16; 512];
    let n = unsafe { ffi::GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 || n as usize > buf.len() {
        return None;
    }
    Some(strip_verbatim(&String::from_utf16_lossy(
        &buf[..n as usize],
    )))
}

/// Park until the socket is readable/writable, locally shut down, or the
/// deadline passes. Waits on `[net_ev, shutdown_ev]` (the `WSAEventSelect`
/// readiness event and this direction's manual-reset shutdown event). Returns:
/// * `Ok(true)`: woke on readiness; retry the recv/send.
/// * `Ok(false)`: the socket was locally shut down for this direction.
/// * `Err(WouldBlock)`: the caller's `deadline` (`None` = no timeout) passed,
///   the kind a Unix `UnixStream` timeout yields.
fn wait_interruptible(
    raw: RawSocket,
    net_ev: ffi::WsaEvent,
    shutdown_ev: ffi::WsaEvent,
    shutdown_flag: &AtomicBool,
    deadline: Option<Instant>,
) -> io::Result<bool> {
    if shutdown_flag.load(Ordering::Acquire) {
        return Ok(false);
    }
    let dw = match deadline {
        None => ffi::WSA_INFINITE,
        Some(d) => {
            let remaining = d.saturating_duration_since(Instant::now()).as_millis();
            if remaining == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "control-socket operation timed out",
                ));
            }
            // Never let a finite deadline round up to WSA_INFINITE.
            remaining.min(u128::from(ffi::WSA_INFINITE - 1)).max(1) as u32
        }
    };
    let events = [net_ev, shutdown_ev];
    let rc =
        unsafe { ffi::WSAWaitForMultipleEvents(events.len() as u32, events.as_ptr(), 0, dw, 0) };
    if rc == ffi::WSA_WAIT_FAILED {
        return Err(last_wsa_error());
    }
    if rc == ffi::WSA_WAIT_TIMEOUT {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "control-socket operation timed out",
        ));
    }
    // A local shutdown may race the readiness wake; the flag is authoritative.
    if shutdown_flag.load(Ordering::Acquire) {
        return Ok(false);
    }
    // Readiness wake: reset `net_ev` and clear the socket's event record. If
    // anything fired, re-signal `net_ev` so a parked opposite-direction wait on
    // the same socket (clones share it) does not lose its wakeup to our reset.
    // Bounded: `net_ev` only re-fires on a real network event.
    let mut ne = ffi::WsaNetworkEvents {
        lNetworkEvents: 0,
        _iErrorCode: [0; 10],
    };
    if unsafe { ffi::WSAEnumNetworkEvents(raw, net_ev, &mut ne) } == 0 && ne.lNetworkEvents != 0 {
        let _ = unsafe { ffi::WSASetEvent(net_ev) };
    }
    Ok(true)
}

/// The shared socket state behind every clone of one [`CtlStream`].
/// Timeouts are in milliseconds, `0` meaning none (std rejects a zero
/// timeout). The shutdown flags record a local `shutdown` per direction for
/// parked waits to observe ([`wait_interruptible`]); the paired manual-reset
/// events wake a thread parked in `WSAWaitForMultipleEvents`. `net_ev` is the
/// readiness event armed in [`CtlStream::from_raw`].
#[derive(Debug)]
struct Inner {
    raw: RawSocket,
    net_ev: ffi::WsaEvent,
    recv_shutdown_ev: ffi::WsaEvent,
    send_shutdown_ev: ffi::WsaEvent,
    read_timeout_ms: AtomicU64,
    write_timeout_ms: AtomicU64,
    recv_shutdown: AtomicBool,
    send_shutdown: AtomicBool,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Last clone gone — close the one underlying socket and its events.
        unsafe {
            let _ = ffi::closesocket(self.raw);
            let _ = ffi::WSACloseEvent(self.net_ev);
            let _ = ffi::WSACloseEvent(self.recv_shutdown_ev);
            let _ = ffi::WSACloseEvent(self.send_shutdown_ev);
        }
    }
}

/// A connected AF_UNIX stream socket (Windows). See the module docs for the
/// `try_clone`/timeout semantics; everything else matches
/// `std::os::unix::net::UnixStream` at the call-site level.
#[derive(Debug)]
pub struct CtlStream(Arc<Inner>);

impl CtlStream {
    /// Wrap an already-connected socket: create its three events and arm
    /// `WSAEventSelect(FD_READ|FD_WRITE|FD_CLOSE)` on `net_ev`. That makes the
    /// socket non-blocking, so it must run after connect/accept. On failure the
    /// socket and any created events are closed and the winsock error returned.
    fn from_raw(raw: RawSocket) -> io::Result<Self> {
        let net_ev = unsafe { ffi::WSACreateEvent() };
        let recv_shutdown_ev = unsafe { ffi::WSACreateEvent() };
        let send_shutdown_ev = unsafe { ffi::WSACreateEvent() };
        let armed = net_ev != ffi::WSA_INVALID_EVENT
            && recv_shutdown_ev != ffi::WSA_INVALID_EVENT
            && send_shutdown_ev != ffi::WSA_INVALID_EVENT
            && unsafe {
                ffi::WSAEventSelect(raw, net_ev, ffi::FD_READ | ffi::FD_WRITE | ffi::FD_CLOSE)
            } != ffi::SOCKET_ERROR;
        if !armed {
            let err = last_wsa_error();
            unsafe {
                let _ = ffi::closesocket(raw);
                if net_ev != ffi::WSA_INVALID_EVENT {
                    let _ = ffi::WSACloseEvent(net_ev);
                }
                if recv_shutdown_ev != ffi::WSA_INVALID_EVENT {
                    let _ = ffi::WSACloseEvent(recv_shutdown_ev);
                }
                if send_shutdown_ev != ffi::WSA_INVALID_EVENT {
                    let _ = ffi::WSACloseEvent(send_shutdown_ev);
                }
            }
            return Err(err);
        }
        Ok(Self(Arc::new(Inner {
            raw,
            net_ev,
            recv_shutdown_ev,
            send_shutdown_ev,
            read_timeout_ms: AtomicU64::new(0),
            write_timeout_ms: AtomicU64::new(0),
            recv_shutdown: AtomicBool::new(false),
            send_shutdown: AtomicBool::new(false),
        })))
    }

    /// Connect to the AF_UNIX socket bound at `path`.
    ///
    /// # Errors
    /// `NotFound` when nothing exists at `path` (normalized, so
    /// `socket_is_live`'s `ConnectionRefused | NotFound` stale test holds);
    /// `ConnectionRefused` for a stale file with no listener; the mapped
    /// winsock error otherwise.
    pub fn connect<P: AsRef<Path>>(path: P) -> io::Result<CtlStream> {
        init();
        let path = path.as_ref();
        let (addr, len) = encode_sun_path(path)?;
        let raw = unsafe { ffi::socket(ffi::AF_UNIX, ffi::SOCK_STREAM, 0) };
        if raw == ffi::INVALID_SOCKET {
            return Err(last_wsa_error());
        }
        // Connect while still blocking (arming events makes it non-blocking,
        // and connect would then return WSAEWOULDBLOCK).
        if unsafe { ffi::connect(raw, &addr, len) } == ffi::SOCKET_ERROR {
            let err = last_wsa_error();
            let _ = unsafe { ffi::closesocket(raw) };
            // A genuinely absent path reads as NotFound. `try_exists`, not
            // `symlink_metadata`: a bound afunix socket is a reparse point that
            // `symlink_metadata` cannot stat cross-process, while `try_exists`
            // returns Ok(true) for it, so a present socket keeps its real error.
            if matches!(path.try_exists(), Ok(false)) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no control socket at {}", path.display()),
                ));
            }
            return Err(err);
        }
        CtlStream::from_raw(raw) // arms readiness/shutdown events; closes on failure
    }

    /// A connected pair, emulating `socketpair(2)`: bind a throwaway listener
    /// under the temp dir, connect, accept, then close and unlink the listener.
    /// Safe single-threaded, since afunix completes the connect against the
    /// backlog before the accept. The per-process, per-call temp path is
    /// same-user territory, like a Unix socketpair's fd table.
    ///
    /// # Errors
    /// Any bind/connect/accept failure (e.g. an unusable temp dir).
    pub fn pair() -> io::Result<(CtlStream, CtlStream)> {
        static PAIR_SEQ: AtomicU64 = AtomicU64::new(0);
        init();
        let seq = PAIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("at-p{}-{}.sock", std::process::id(), seq));
        let _ = std::fs::remove_file(&path);
        let listener = CtlListener::bind(&path)?;
        let connector = CtlStream::connect(&path)?;
        let accepted = listener.accept().map(|(s, ())| s);
        drop(listener);
        let _ = std::fs::remove_file(&path);
        Ok((accepted?, connector))
    }

    /// The connected peer's pid as afunix recorded it at `connect`
    /// (`SIO_AF_UNIX_GETPEERPID`), which the peer cannot assert for itself.
    ///
    /// # Errors
    /// Whatever `WSAIoctl` reports (an afunix without the ioctl answers
    /// `WSAEINVAL`/`WSAEOPNOTSUPP`), and `InvalidData` for a short answer or a
    /// zero pid.
    pub fn peer_pid(&self) -> io::Result<u32> {
        let mut pid: u32 = 0;
        let mut returned: u32 = 0;
        // SAFETY: `raw` is this stream's live socket; the output buffer is a
        // `u32` we own, sized by `cbOutBuffer`; no input buffer, synchronous
        // (null overlapped and completion routine).
        let rc = unsafe {
            ffi::WSAIoctl(
                self.0.raw,
                ffi::SIO_AF_UNIX_GETPEERPID,
                std::ptr::null_mut(),
                0,
                (&raw mut pid).cast(),
                std::mem::size_of::<u32>() as u32,
                &mut returned,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if rc == ffi::SOCKET_ERROR {
            return Err(last_wsa_error());
        }
        if returned as usize != std::mem::size_of::<u32>() || pid == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SIO_AF_UNIX_GETPEERPID answered no pid",
            ));
        }
        Ok(pid)
    }

    /// A handle sharing this socket (an `Arc` clone; fallible only for parity
    /// with `UnixStream::try_clone`). Clones share `shutdown` and timeouts, and
    /// the socket closes when the last clone drops.
    ///
    /// # Errors
    /// Never fails on Windows.
    pub fn try_clone(&self) -> io::Result<CtlStream> {
        Ok(CtlStream(Arc::clone(&self.0)))
    }

    /// Shut down one or both directions (`ws2_32 shutdown`), raise the matching
    /// local flags, and set each direction's shutdown event, which wakes a
    /// `read`/`write` parked on another thread's clone (afunix does not; see the
    /// module docs).
    ///
    /// # Errors
    /// The mapped winsock error (an already-shut-down socket errors, as on
    /// Unix); the flags are raised only on success.
    pub fn shutdown(&self, how: std::net::Shutdown) -> io::Result<()> {
        let (flag_recv, flag_send, how) = match how {
            std::net::Shutdown::Read => (true, false, ffi::SD_RECEIVE),
            std::net::Shutdown::Write => (false, true, ffi::SD_SEND),
            std::net::Shutdown::Both => (true, true, ffi::SD_BOTH),
        };
        if unsafe { ffi::shutdown(self.0.raw, how) } == ffi::SOCKET_ERROR {
            return Err(last_wsa_error());
        }
        // Raise the flag, then wake the parked wait, which re-checks the flag
        // (Release/Acquire pairs the two).
        if flag_recv {
            self.0.recv_shutdown.store(true, Ordering::Release);
            let _ = unsafe { ffi::WSASetEvent(self.0.recv_shutdown_ev) };
        }
        if flag_send {
            self.0.send_shutdown.store(true, Ordering::Release);
            let _ = unsafe { ffi::WSASetEvent(self.0.send_shutdown_ev) };
        }
        Ok(())
    }

    /// One read that never parks: the `recv` [`io::Read::read`] makes before
    /// it waits, without the wait. `WouldBlock` when nothing is queued, and
    /// end-of-file (`Ok(0)`) for a peer's close or write-half close and for a
    /// local read shutdown, exactly as `read` reports them.
    ///
    /// For a liveness probe that must answer at once: the control socket's
    /// push loop asks one before every pass it writes, and a probe that
    /// borrowed `read` under a short read timeout made every pushed frame wait
    /// that timeout out. A Unix caller peeks with `MSG_PEEK | MSG_DONTWAIT`
    /// instead; this consumes what it reads, so it is for a stream whose next
    /// byte ends it anyway (a push-only subscription's client sends none).
    /// There is no per-call flag to borrow here and none is needed: arming
    /// `WSAEventSelect` already made the socket non-blocking for every clone.
    ///
    /// # Errors
    /// `WouldBlock` for a quiet open peer; the mapped winsock error otherwise.
    pub fn read_nonblocking(&self, buf: &mut [u8]) -> io::Result<usize> {
        let inner = &self.0;
        let len = buf.len().min(i32::MAX as usize) as i32;
        let n = unsafe { ffi::recv(inner.raw, buf.as_mut_ptr(), len, 0) };
        if n != ffi::SOCKET_ERROR {
            return Ok(n as usize);
        }
        let err = last_wsa_error();
        match err.raw_os_error() {
            // `read` would park here and then report a local shutdown as EOF
            // (its wait re-checks the flag); answer the same without parking.
            Some(ffi::WSAEWOULDBLOCK) if inner.recv_shutdown.load(Ordering::Acquire) => Ok(0),
            Some(ffi::WSAESHUTDOWN) => Ok(0),
            _ => Err(err),
        }
    }

    /// Set the read timeout (`None` clears it). Expiry surfaces as
    /// `ErrorKind::WouldBlock`. Shared by all clones and safe to call from any
    /// thread.
    ///
    /// # Errors
    /// `InvalidInput` for a zero `Duration`, matching std.
    pub fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        self.0
            .read_timeout_ms
            .store(timeout_to_ms(t)?, Ordering::Relaxed);
        Ok(())
    }

    /// Set the write timeout (`None` clears it), for parity with std.
    ///
    /// # Errors
    /// `InvalidInput` for a zero `Duration`, matching std.
    pub fn set_write_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        self.0
            .write_timeout_ms
            .store(timeout_to_ms(t)?, Ordering::Relaxed);
        Ok(())
    }
}

/// std-compatible timeout validation: `Some(0)` is an error, sub-millisecond
/// values round up to 1ms (never to "no timeout").
fn timeout_to_ms(t: Option<Duration>) -> io::Result<u64> {
    match t {
        None => Ok(0),
        Some(d) if d.is_zero() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot set a 0 duration timeout",
        )),
        Some(d) => Ok((d.as_millis().min(u128::from(u64::MAX)) as u64).max(1)),
    }
}

impl io::Read for &CtlStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let inner = &self.0;
        let timeout = inner.read_timeout_ms.load(Ordering::Relaxed);
        let deadline = (timeout != 0).then(|| Instant::now() + Duration::from_millis(timeout));
        let len = buf.len().min(i32::MAX as usize) as i32;
        loop {
            // The socket is non-blocking (WSAEventSelect): recv first, park
            // only when there is nothing to read.
            let n = unsafe { ffi::recv(inner.raw, buf.as_mut_ptr(), len, 0) };
            if n != ffi::SOCKET_ERROR {
                return Ok(n as usize);
            }
            let err = last_wsa_error();
            match err.raw_os_error() {
                Some(ffi::WSAEWOULDBLOCK) => {
                    if !wait_interruptible(
                        inner.raw,
                        inner.net_ev,
                        inner.recv_shutdown_ev,
                        &inner.recv_shutdown,
                        deadline,
                    )? {
                        // Locally shut down: EOF, as on Unix, so a relay pump
                        // parked on a clone exits cleanly.
                        return Ok(0);
                    }
                }
                // A raced local `shutdown` surfaces as WSAESHUTDOWN from recv;
                // same normalization to EOF as the flag path above.
                Some(ffi::WSAESHUTDOWN) => return Ok(0),
                _ => return Err(err),
            }
        }
    }
}

impl io::Write for &CtlStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let inner = &self.0;
        let timeout = inner.write_timeout_ms.load(Ordering::Relaxed);
        let deadline = (timeout != 0).then(|| Instant::now() + Duration::from_millis(timeout));
        let len = buf.len().min(i32::MAX as usize) as i32;
        loop {
            let n = unsafe { ffi::send(inner.raw, buf.as_ptr(), len, 0) };
            if n != ffi::SOCKET_ERROR {
                return Ok(n as usize);
            }
            let err = last_wsa_error();
            match err.raw_os_error() {
                Some(ffi::WSAEWOULDBLOCK) => {
                    if !wait_interruptible(
                        inner.raw,
                        inner.net_ev,
                        inner.send_shutdown_ev,
                        &inner.send_shutdown,
                        deadline,
                    )? {
                        // Locally shut down for writing: `BrokenPipe`, as on
                        // Unix.
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "control socket shut down for writing",
                        ));
                    }
                }
                // Same parity for a raced local shutdown observed by send.
                Some(ffi::WSAESHUTDOWN) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "control socket shut down for writing",
                    ));
                }
                _ => return Err(err),
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(()) // sends are unbuffered; nothing to push
    }
}

impl io::Read for CtlStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self).read(buf)
    }
}

impl io::Write for CtlStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&*self).flush()
    }
}

/// A bound, listening AF_UNIX socket (Windows). Dropping it closes the
/// listening socket but, like `UnixListener`, does not unlink the socket file.
#[derive(Debug)]
pub struct CtlListener {
    raw: RawSocket,
}

impl CtlListener {
    /// Bind and listen (backlog 128) at `path`. The socket file is an afunix
    /// reparse point; a stale one must be removed first, as on Unix.
    ///
    /// # Errors
    /// `InvalidInput` for an unencodable path (see [`CtlStream::connect`]);
    /// `AddrInUse` when a file already exists there; the mapped winsock error
    /// otherwise.
    pub fn bind<P: AsRef<Path>>(path: P) -> io::Result<CtlListener> {
        init();
        let (addr, len) = encode_sun_path(path.as_ref())?;
        let raw = unsafe { ffi::socket(ffi::AF_UNIX, ffi::SOCK_STREAM, 0) };
        if raw == ffi::INVALID_SOCKET {
            return Err(last_wsa_error());
        }
        let listener = CtlListener { raw }; // owns + closes on early return
        if unsafe { ffi::bind(raw, &addr, len) } == ffi::SOCKET_ERROR {
            return Err(last_wsa_error());
        }
        if unsafe { ffi::listen(raw, 128) } == ffi::SOCKET_ERROR {
            return Err(last_wsa_error());
        }
        Ok(listener)
    }

    /// Accept one connection. The `()` stands where std returns the peer
    /// `SocketAddr` (unnamed for AF_UNIX peers; no call site reads it).
    ///
    /// # Errors
    /// The mapped winsock error.
    pub fn accept(&self) -> io::Result<(CtlStream, ())> {
        let raw = unsafe { ffi::accept(self.raw, std::ptr::null_mut(), std::ptr::null_mut()) };
        if raw == ffi::INVALID_SOCKET {
            return Err(last_wsa_error());
        }
        Ok((CtlStream::from_raw(raw)?, ()))
    }

    /// A blocking iterator over incoming connections (never `None`), matching
    /// `UnixListener::incoming`.
    pub fn incoming(&self) -> Incoming<'_> {
        Incoming { listener: self }
    }
}

impl Drop for CtlListener {
    fn drop(&mut self) {
        let _ = unsafe { ffi::closesocket(self.raw) };
    }
}

/// Iterator returned by [`CtlListener::incoming`].
#[derive(Debug)]
pub struct Incoming<'a> {
    listener: &'a CtlListener,
}

impl Iterator for Incoming<'_> {
    type Item = io::Result<CtlStream>;

    fn next(&mut self) -> Option<io::Result<CtlStream>> {
        Some(self.listener.accept().map(|(s, ())| s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hang detector, never a latency budget: a wake that must come ends the
    /// wait the moment it lands.
    const HANG_DETECTOR: Duration = Duration::from_secs(60);

    /// Non-ASCII (but valid-UTF-8) paths encode directly — the default socket
    /// dir under a Cyrillic/CJK `%LOCALAPPDATA%` must not need any fallback —
    /// while the 107-byte sun_path budget stays enforced on the UTF-8 length.
    #[test]
    fn utf8_non_ascii_paths_encode_directly() {
        let s = "C:\\Users\\Владимир\\AppData\\Local\\aterm\\aterm-1234.sock";
        let (addr, _) = try_encode(s).expect("non-ASCII UTF-8 within budget encodes");
        assert_eq!(addr.sun_family, ffi::AF_UNIX as u16);
        assert_eq!(&addr.sun_path[..s.len()], s.as_bytes());
        assert_eq!(addr.sun_path[s.len()], 0, "NUL-terminated");
        // 54 two-byte chars = 108 UTF-8 bytes + "C:\" — over the 107 budget.
        assert!(try_encode(&format!("C:\\{}", "я".repeat(54))).is_none());
        assert!(
            try_encode("C:\\a\0b.sock").is_none(),
            "interior NUL rejected"
        );
    }

    /// afunix.sys reads `sun_path` as UTF-8: a full bind/connect/accept round
    /// trip through a non-ASCII directory.
    #[test]
    fn non_ascii_dir_binds_and_connects() {
        use std::io::{Read, Write};
        let dir =
            std::env::temp_dir().join(format!("aterm-uds-профиль-配置-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create non-ASCII test dir");
        let path = dir.join("aterm-1.sock");

        let listener = CtlListener::bind(&path).expect("bind in non-ASCII dir");
        let server = std::thread::spawn(move || {
            let (stream, ()) = listener.accept().expect("accept");
            let mut buf = [0u8; 4];
            (&stream).read_exact(&mut buf).expect("read");
            (&stream).write_all(&buf).expect("echo");
        });
        let client = CtlStream::connect(&path).expect("connect in non-ASCII dir");
        (&client).write_all(b"ping").expect("write");
        let mut back = [0u8; 4];
        (&client).read_exact(&mut back).expect("read echo");
        assert_eq!(&back, b"ping");
        server.join().expect("server thread");

        // The socket file landed at the real (non-ASCII) path.
        assert!(std::fs::symlink_metadata(&path).is_ok());
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A no-timeout read parks on the readiness/shutdown events and wakes
    /// with EOF when a clone is shut down.
    #[test]
    fn idle_parked_read_wakes_on_shutdown() {
        use std::io::Read;
        let (a, _b) = CtlStream::pair().expect("pair");
        let clone = a.try_clone().expect("clone");
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 16];
            // No timeout: parks until the shutdown event fires.
            let _ = tx.send((&a).read(&mut buf));
        });
        std::thread::sleep(Duration::from_millis(80));
        clone.shutdown(std::net::Shutdown::Both).expect("shutdown");
        let res = rx
            .recv_timeout(HANG_DETECTOR)
            .expect("shutdown must wake the parked read");
        if let Ok(n) = res {
            assert_eq!(n, 0, "post-shutdown read is EOF");
        }
        reader.join().expect("reader thread");
    }

    /// A no-timeout read parked with no data wakes on the peer's write via the
    /// `FD_READ` readiness event and returns the bytes.
    #[test]
    fn parked_read_wakes_on_peer_write() {
        use std::io::{Read, Write};
        let (a, b) = CtlStream::pair().expect("pair");
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 5];
            let n = (&a).read(&mut buf).expect("read");
            let _ = tx.send((n, buf));
        });
        std::thread::sleep(Duration::from_millis(80));
        (&b).write_all(b"hello").expect("write");
        let (n, buf) = rx
            .recv_timeout(HANG_DETECTOR)
            .expect("peer write must wake the parked read");
        assert_eq!(&buf[..n], b"hello");
        reader.join().expect("reader thread");
    }
}
