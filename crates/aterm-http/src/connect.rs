// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A timed TCP connect whose refusal is prompt on every platform.

use std::io;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// Connect to `addr` within `timeout`, as `TcpStream::connect_timeout` does.
/// # Errors
/// The connect failed, was refused, or did not complete within `timeout`.
pub fn connect_timeout(addr: &SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
    #[cfg(windows)]
    {
        winsock::connect_timeout(addr, timeout)
    }
    #[cfg(not(windows))]
    {
        TcpStream::connect_timeout(addr, timeout)
    }
}

/// Winsock, hand-rolled (the house style for flat Win32): aterm-win32 carries
/// only its consumers' closure of windows-sys, which names none of these.
#[cfg(windows)]
mod winsock {
    use std::ffi::c_void;
    use std::io;
    use std::net::{SocketAddr, TcpStream};
    use std::os::windows::io::{FromRawSocket as _, RawSocket};
    use std::time::Duration;

    /// `SOCKET`: the pointer-width unsigned integer std's `RawSocket` carries.
    type Socket = usize;
    const INVALID_SOCKET: Socket = usize::MAX;
    const SOCKET_ERROR: i32 = -1;
    const AF_INET: u16 = 2;
    const AF_INET6: u16 = 23;
    const SOCK_STREAM: i32 = 1;
    const IPPROTO_TCP: i32 = 6;
    /// std's own flags for a socket: overlapped-capable, never inherited by a child.
    const WSA_FLAG_OVERLAPPED: u32 = 0x01;
    const WSA_FLAG_NO_HANDLE_INHERIT: u32 = 0x80;
    /// `_WSAIOW(IOC_VENDOR, 17)`.
    const SIO_TCP_INITIAL_RTO: u32 = 0x9800_0011;
    /// `TCP_INITIAL_RTO_UNSPECIFIED_RTT`: the system's initial RTO, untouched.
    const RTT_UNSPECIFIED: u16 = u16::MAX;
    /// The one SYN retransmission kept (see the module).
    const SYN_RETRANSMISSIONS: u8 = 1;
    const WSAEWOULDBLOCK: i32 = 10035;
    /// Winsock 2.2, what std asks for.
    const WINSOCK_2_2: u16 = 0x0202;

    /// `TCP_INITIAL_RTO_PARAMETERS`.
    #[repr(C)]
    struct InitialRto {
        rtt: u16,
        max_syn_retransmissions: u8,
    }

    /// `sockaddr_in`.
    #[repr(C)]
    struct SockaddrIn {
        family: u16,
        port: [u8; 2],
        addr: [u8; 4],
        zero: [u8; 8],
    }

    /// `sockaddr_in6`.
    #[repr(C)]
    struct SockaddrIn6 {
        family: u16,
        port: [u8; 2],
        flowinfo: u32,
        addr: [u8; 16],
        scope_id: u32,
    }

    /// `fd_set`, as winsock lays it out: a count, then up to `FD_SETSIZE` sockets.
    #[repr(C)]
    struct FdSet {
        count: u32,
        sockets: [Socket; 64],
    }

    /// `timeval` (`long` is 32 bits on Windows).
    #[repr(C)]
    struct Timeval {
        sec: i32,
        usec: i32,
    }

    /// Room for a `WSADATA` (408 bytes on 64-bit Windows), aligned for its pointer
    /// field; `WSAStartup` only writes it.
    #[repr(C, align(8))]
    struct WsaData([u8; 512]);

    #[link(name = "ws2_32")]
    unsafe extern "system" {
        fn WSAStartup(version: u16, data: *mut WsaData) -> i32;
        fn WSASocketW(
            af: i32,
            ty: i32,
            protocol: i32,
            info: *const c_void,
            group: u32,
            flags: u32,
        ) -> Socket;
        fn WSAIoctl(
            s: Socket,
            code: u32,
            in_buf: *const c_void,
            in_len: u32,
            out_buf: *mut c_void,
            out_len: u32,
            returned: *mut u32,
            overlapped: *mut c_void,
            completion: *const c_void,
        ) -> i32;
        fn connect(s: Socket, name: *const c_void, len: i32) -> i32;
        fn select(
            nfds: i32,
            read: *mut FdSet,
            write: *mut FdSet,
            except: *mut FdSet,
            timeout: *const Timeval,
        ) -> i32;
        fn WSAGetLastError() -> i32;
    }

    /// The calling thread's last winsock error (winsock keeps its own slot, which
    /// `io::Error::last_os_error` does not read).
    fn last_error() -> io::Error {
        // SAFETY: takes no arguments and only reads this thread's winsock error slot.
        io::Error::from_raw_os_error(unsafe { WSAGetLastError() })
    }

    /// Winsock for this process, once — as std starts its own sockets, and like
    /// std's never cleaned up before the process ends.
    fn ensure_winsock() -> io::Result<()> {
        static STARTED: std::sync::OnceLock<i32> = std::sync::OnceLock::new();
        let code = *STARTED.get_or_init(|| {
            let mut data = WsaData([0; 512]);
            // SAFETY: `data` is a live, aligned buffer larger than `WSADATA`, which the call only
            // writes.
            unsafe { WSAStartup(WINSOCK_2_2, &mut data) }
        });
        if code == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(code))
        }
    }

    /// A set holding the one socket `s`.
    fn one(s: Socket) -> FdSet {
        let mut set = FdSet {
            count: 1,
            sockets: [0; 64],
        };
        set.sockets[0] = s;
        set
    }

    /// `timeout` as `select` takes it: whole seconds saturated, and a positive
    /// sub-microsecond remainder rounded up to one microsecond (std's rule).
    fn timeval(timeout: Duration) -> Timeval {
        let mut tv = Timeval {
            sec: i32::try_from(timeout.as_secs()).unwrap_or(i32::MAX),
            usec: i32::try_from(timeout.subsec_micros()).unwrap_or(999_999),
        };
        if tv.sec == 0 && tv.usec == 0 {
            tv.usec = 1;
        }
        tv
    }

    pub(super) fn connect_timeout(addr: &SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
        if timeout.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot set a 0 duration timeout",
            ));
        }
        ensure_winsock()?;
        let family = if addr.is_ipv4() { AF_INET } else { AF_INET6 };
        // SAFETY: plain arguments; no protocol info is passed.
        let s = unsafe {
            WSASocketW(
                i32::from(family),
                SOCK_STREAM,
                IPPROTO_TCP,
                std::ptr::null(),
                0,
                WSA_FLAG_OVERLAPPED | WSA_FLAG_NO_HANDLE_INHERIT,
            )
        };
        if s == INVALID_SOCKET {
            return Err(last_error());
        }
        // SAFETY: `s` is a socket this call just created and owns alone; from here the stream
        // closes it on every return.
        let stream = unsafe { TcpStream::from_raw_socket(s as RawSocket) };
        let rto = InitialRto {
            rtt: RTT_UNSPECIFIED,
            max_syn_retransmissions: SYN_RETRANSMISSIONS,
        };
        let mut returned = 0u32;
        // SAFETY: `rto` is a live `TCP_INITIAL_RTO_PARAMETERS` read for its size; no output buffer,
        // overlap or completion routine.
        let _ = unsafe {
            WSAIoctl(
                s,
                SIO_TCP_INITIAL_RTO,
                std::ptr::from_ref(&rto).cast(),
                u32::try_from(std::mem::size_of::<InitialRto>()).unwrap_or(u32::MAX),
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        stream.set_nonblocking(true)?;
        let v4;
        let v6;
        let (name, len): (*const c_void, usize) = match addr {
            SocketAddr::V4(a) => {
                v4 = SockaddrIn {
                    family,
                    port: a.port().to_be_bytes(),
                    addr: a.ip().octets(),
                    zero: [0; 8],
                };
                (
                    std::ptr::from_ref(&v4).cast(),
                    std::mem::size_of::<SockaddrIn>(),
                )
            }
            SocketAddr::V6(a) => {
                v6 = SockaddrIn6 {
                    family,
                    port: a.port().to_be_bytes(),
                    flowinfo: a.flowinfo(),
                    addr: a.ip().octets(),
                    scope_id: a.scope_id(),
                };
                (
                    std::ptr::from_ref(&v6).cast(),
                    std::mem::size_of::<SockaddrIn6>(),
                )
            }
        };
        let len = i32::try_from(len).unwrap_or(i32::MAX);
        // SAFETY: `name` points at a live sockaddr of `len` bytes for the call.
        if unsafe { connect(s, name, len) } == SOCKET_ERROR {
            let err = last_error();
            if err.raw_os_error() != Some(WSAEWOULDBLOCK) {
                return Err(err);
            }
            let tv = timeval(timeout);
            let (mut writable, mut failed) = (one(s), one(s));
            // SAFETY: both sets and the timeout are live for the call; winsock ignores `nfds`.
            match unsafe { select(1, std::ptr::null_mut(), &mut writable, &mut failed, &tv) } {
                SOCKET_ERROR => return Err(last_error()),
                0 => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "connection timed out",
                    ));
                }
                _ => {
                    if writable.count != 1
                        && let Some(e) = stream.take_error()?
                    {
                        return Err(e);
                    }
                }
            }
        }
        stream.set_nonblocking(false)?;
        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::{Ipv4Addr, TcpListener};

    use aterm_spec::wait::MUST_HAPPEN;

    use super::*;

    /// A live loopback listener takes the connection, and the stream comes back
    /// blocking: a read waits for the byte the peer writes.
    #[test]
    fn a_listening_port_connects_and_the_stream_blocks() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback");
        let addr = listener.local_addr().expect("addr");
        let mut stream = connect_timeout(&addr, MUST_HAPPEN).expect("connects");
        let (mut peer, _) = listener.accept().expect("accepts");
        peer.write_all(b"x").expect("writes");
        let mut byte = [0u8; 1];
        stream
            .read_exact(&mut byte)
            .expect("a blocking read waits for it");
        assert_eq!(&byte, b"x");
    }

    /// A port nothing listens on is refused by the kernel's own word. No child
    /// is spawned in this test binary, so no forked copy of the dropped listener
    /// can still be accepting.
    #[test]
    fn a_closed_port_is_refused() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        let err = connect_timeout(&addr, MUST_HAPPEN).expect_err("nothing listens");
        assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused, "{err}");
    }

    /// Windows rejects zero before creating a socket; other platforms follow std.
    #[test]
    fn a_zero_budget_is_invalid_input() {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 9));
        let err = connect_timeout(&addr, Duration::ZERO).expect_err("zero");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
    }
}
