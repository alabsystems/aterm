// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows `AF_UNIX` stream and listener sockets ([`CtlStream`], [`CtlListener`]) over Winsock.

pub(crate) mod ffi;

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ffi::RawSocket;

/// Call `WSAStartup(2.2)` once, process-wide, before the first socket call.
fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let mut data = ffi::WsaData { _opaque: [0; 512] };
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
/// prefix (`\\?\` / `\\?\UNC\`) first.
fn encode_sun_path(path: &Path) -> io::Result<(ffi::SockaddrUn, i32)> {
    if let Some(s) = path.to_str()
        && let Some(addr) = try_encode(s)
    {
        return Ok(addr);
    }
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
/// deadline passes.
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
    if shutdown_flag.load(Ordering::Acquire) {
        return Ok(false);
    }
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
        unsafe {
            let _ = ffi::closesocket(self.raw);
            let _ = ffi::WSACloseEvent(self.net_ev);
            let _ = ffi::WSACloseEvent(self.recv_shutdown_ev);
            let _ = ffi::WSACloseEvent(self.send_shutdown_ev);
        }
    }
}

/// Connected Windows `AF_UNIX` stream socket.
#[derive(Debug)]
pub struct CtlStream(Arc<Inner>);

impl CtlStream {
    /// Wrap an already-connected socket: create its three events and arm
    /// `WSAEventSelect(FD_READ|FD_WRITE|FD_CLOSE)` on `net_ev`.
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

    /// Connects to the `AF_UNIX` socket bound at `path`.
    /// # Errors
    /// Returns an error if `path` cannot be encoded or `socket`/`connect` fails.
    pub fn connect<P: AsRef<Path>>(path: P) -> io::Result<CtlStream> {
        init();
        let path = path.as_ref();
        let (addr, len) = encode_sun_path(path)?;
        let raw = unsafe { ffi::socket(ffi::AF_UNIX, ffi::SOCK_STREAM, 0) };
        if raw == ffi::INVALID_SOCKET {
            return Err(last_wsa_error());
        }
        if unsafe { ffi::connect(raw, &addr, len) } == ffi::SOCKET_ERROR {
            let err = last_wsa_error();
            let _ = unsafe { ffi::closesocket(raw) };
            if matches!(path.try_exists(), Ok(false)) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no control socket at {}", path.display()),
                ));
            }
            return Err(err);
        }
        CtlStream::from_raw(raw)
    }

    /// Creates a connected pair of [`CtlStream`]s via a temporary listener.
    /// # Errors
    /// Returns any Winsock or filesystem error from binding, connecting, or accepting.
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

    /// Queries the connected peer's process ID via `SIO_AF_UNIX_GETPEERPID`.
    /// # Errors
    /// Returns the mapped Winsock error if `WSAIoctl` fails.
    pub fn peer_pid(&self) -> io::Result<u32> {
        let mut pid: u32 = 0;
        let mut returned: u32 = 0;
        // SAFETY: `raw` is a live socket and `pid` is a valid `u32` output buffer for synchronous `WSAIoctl`.
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

    /// Returns a cloned handle sharing the underlying socket state.
    /// # Errors
    /// Infallible; returns `io::Result` for parity with `UnixStream::try_clone`.
    pub fn try_clone(&self) -> io::Result<CtlStream> {
        Ok(CtlStream(Arc::clone(&self.0)))
    }

    /// Shuts down one or both directions and signals any parked reads or writes.
    /// # Errors
    /// Returns the mapped Winsock error if `shutdown` fails.
    pub fn shutdown(&self, how: std::net::Shutdown) -> io::Result<()> {
        let (flag_recv, flag_send, how) = match how {
            std::net::Shutdown::Read => (true, false, ffi::SD_RECEIVE),
            std::net::Shutdown::Write => (false, true, ffi::SD_SEND),
            std::net::Shutdown::Both => (true, true, ffi::SD_BOTH),
        };
        if unsafe { ffi::shutdown(self.0.raw, how) } == ffi::SOCKET_ERROR {
            return Err(last_wsa_error());
        }
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

    /// Performs one non-blocking `recv`, returning `WouldBlock` when empty or `Ok(0)` on EOF.
    /// # Errors
    /// Returns `WouldBlock` when no bytes are queued, or the mapped Winsock error.
    pub fn read_nonblocking(&self, buf: &mut [u8]) -> io::Result<usize> {
        let inner = &self.0;
        let len = buf.len().min(i32::MAX as usize) as i32;
        let n = unsafe { ffi::recv(inner.raw, buf.as_mut_ptr(), len, 0) };
        if n != ffi::SOCKET_ERROR {
            return Ok(n as usize);
        }
        let err = last_wsa_error();
        match err.raw_os_error() {
            Some(ffi::WSAEWOULDBLOCK) if inner.recv_shutdown.load(Ordering::Acquire) => Ok(0),
            Some(ffi::WSAESHUTDOWN) => Ok(0),
            _ => Err(err),
        }
    }

    /// Sets or clears the read timeout shared across clones.
    /// # Errors
    /// Returns `InvalidInput` for a zero `Duration`.
    pub fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        self.0
            .read_timeout_ms
            .store(timeout_to_ms(t)?, Ordering::Relaxed);
        Ok(())
    }

    /// Sets or clears the write timeout shared across clones.
    /// # Errors
    /// Returns `InvalidInput` for a zero `Duration`.
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
                Some(ffi::WSAESHUTDOWN) => return Ok(0),
                _ => return Err(err),
            }
        }
    }
}

/// The most slices one [`io::Write::write_vectored`] hands `WSASend`, the
/// same bound as `IOV_MAX` on Unix; the rest is a short write.
const MAX_SEND_BUFS: usize = 1024;

impl CtlStream {
    /// Run `attempt`, one non-blocking `send`/`WSASend` answering the bytes it
    /// sent or `None` on `SOCKET_ERROR`, until it sends: parked on the
    /// readiness event while the socket has no room, under the write timeout.
    fn send_with(&self, mut attempt: impl FnMut() -> Option<usize>) -> io::Result<usize> {
        let inner = &self.0;
        let timeout = inner.write_timeout_ms.load(Ordering::Relaxed);
        let deadline = (timeout != 0).then(|| Instant::now() + Duration::from_millis(timeout));
        loop {
            if let Some(n) = attempt() {
                return Ok(n);
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
}

impl io::Write for &CtlStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let raw = self.0.raw;
        let len = buf.len().min(i32::MAX as usize) as i32;
        self.send_with(|| {
            let n = unsafe { ffi::send(raw, buf.as_ptr(), len, 0) };
            (n != ffi::SOCKET_ERROR).then_some(n as usize)
        })
    }

    /// One `WSASend` of up to [`MAX_SEND_BUFS`] slices, where std's default
    /// would make one `send` per slice.
    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        let bufs = &bufs[..bufs.len().min(MAX_SEND_BUFS)];
        if bufs.is_empty() {
            return Ok(0);
        }
        let raw = self.0.raw;
        self.send_with(|| {
            let mut sent = 0u32;
            let rc = unsafe {
                ffi::WSASend(
                    raw,
                    bufs.as_ptr(),
                    bufs.len() as u32,
                    &mut sent,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            (rc != ffi::SOCKET_ERROR).then_some(sent as usize)
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        // sends are unbuffered; nothing to push
        Ok(())
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

    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        (&*self).write_vectored(bufs)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&*self).flush()
    }
}

/// Bound and listening Windows `AF_UNIX` socket.
#[derive(Debug)]
pub struct CtlListener {
    raw: RawSocket,
}

impl CtlListener {
    /// Binds and listens at `path`.
    /// # Errors
    /// Returns an error if `path` cannot be encoded or `socket`/`bind`/`listen` fails.
    pub fn bind<P: AsRef<Path>>(path: P) -> io::Result<CtlListener> {
        init();
        let (addr, len) = encode_sun_path(path.as_ref())?;
        let raw = unsafe { ffi::socket(ffi::AF_UNIX, ffi::SOCK_STREAM, 0) };
        if raw == ffi::INVALID_SOCKET {
            return Err(last_wsa_error());
        }
        let listener = CtlListener { raw };
        if unsafe { ffi::bind(raw, &addr, len) } == ffi::SOCKET_ERROR {
            return Err(last_wsa_error());
        }
        if unsafe { ffi::listen(raw, 128) } == ffi::SOCKET_ERROR {
            return Err(last_wsa_error());
        }
        Ok(listener)
    }

    /// Accepts one incoming connection.
    /// # Errors
    /// Returns the mapped Winsock error if `accept` fails.
    pub fn accept(&self) -> io::Result<(CtlStream, ())> {
        let raw = unsafe { ffi::accept(self.raw, std::ptr::null_mut(), std::ptr::null_mut()) };
        if raw == ffi::INVALID_SOCKET {
            return Err(last_wsa_error());
        }
        Ok((CtlStream::from_raw(raw)?, ()))
    }

    /// Returns a blocking iterator over incoming connections.
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
