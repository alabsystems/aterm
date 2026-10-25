// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared preparation for outbound sockets and listening sockets. No peer can
//! receive a packet before the caller explicitly connects or starts listening.

use super::{Socket, bind_interface, refuse, restrict_hops, set_option, socket_policy};
use std::{
    io,
    net::{SocketAddr, TcpStream},
    os::fd::AsRawFd,
};

// XNU bsd/sys/socket.h; the bounded aterm-libc facade omits this Apple option.
#[cfg(target_os = "macos")]
pub(super) const NO_SIGPIPE: i32 = 0x1022;

pub(super) enum Address {
    V4(libc::sockaddr_in),
    V6(libc::sockaddr_in6),
}
impl Address {
    pub(super) fn new(address: SocketAddr) -> Self {
        match address {
            SocketAddr::V4(address) => {
                // SAFETY: all-zero padding is valid for a sockaddr_in.
                let mut raw: libc::sockaddr_in = unsafe { std::mem::zeroed() };
                raw.sin_family = libc::AF_INET as _;
                #[cfg(target_os = "macos")]
                {
                    raw.sin_len = std::mem::size_of_val(&raw) as _;
                }
                raw.sin_port = address.port().to_be();
                raw.sin_addr.s_addr = u32::from_ne_bytes(address.ip().octets());
                Self::V4(raw)
            }
            SocketAddr::V6(address) => {
                // SAFETY: all-zero padding is valid for a sockaddr_in6.
                let mut raw: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
                raw.sin6_family = libc::AF_INET6 as _;
                #[cfg(target_os = "macos")]
                {
                    raw.sin6_len = std::mem::size_of_val(&raw) as _;
                }
                raw.sin6_port = address.port().to_be();
                raw.sin6_addr.s6_addr = address.ip().octets();
                raw.sin6_scope_id = address.scope_id();
                Self::V6(raw)
            }
        }
    }
    pub(super) fn parts(&self) -> (*const libc::sockaddr, libc::socklen_t) {
        match self {
            Self::V4(raw) => (
                std::ptr::from_ref(raw).cast(),
                std::mem::size_of_val(raw) as _,
            ),
            Self::V6(raw) => (
                std::ptr::from_ref(raw).cast(),
                std::mem::size_of_val(raw) as _,
            ),
        }
    }
}

pub(super) fn bound(source: SocketAddr, interface: Option<(u32, &str)>) -> io::Result<TcpStream> {
    if !super::super::unambiguous(source) || interface.is_some_and(|(index, _)| index == 0) {
        return Err(refuse("invalid local socket binding"));
    }
    let family = if source.is_ipv4() {
        libc::AF_INET
    } else {
        libc::AF_INET6
    };
    let stream = TcpStream::from(Socket::of_kind(family, libc::SOCK_STREAM, libc::IPPROTO_TCP)?.0);
    aterm_uds::keep_out_of_children(&stream)?;
    // Adopting an OwnedFd does not run std's Apple socket constructor.
    #[cfg(target_os = "macos")]
    set_option(&stream, libc::SOL_SOCKET, NO_SIGPIPE, 1)?;
    if source.is_ipv6() {
        set_option(&stream, libc::IPPROTO_IPV6, libc::IPV6_V6ONLY, 1)?;
    }
    if let Some((index, name)) = interface {
        bind_interface(&stream, index, name)?;
        restrict_hops(&stream)?;
    }
    let address = Address::new(source);
    let (pointer, length) = address.parts();
    // SAFETY: address owns the initialized sockaddr for the complete call.
    if unsafe { libc::bind(stream.as_raw_fd(), pointer, length) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let actual = stream.local_addr()?;
    let mut expected = source;
    if source.port() == 0 {
        expected.set_port(actual.port());
    }
    if actual != expected {
        return Err(refuse(
            "kernel socket binding differs from the requested source",
        ));
    }
    socket_policy(
        &stream,
        interface.map(|(index, _)| index),
        interface.is_some(),
    )?;
    Ok(stream)
}
