// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared preparation for outbound sockets and listening sockets. No peer can
//! receive a packet before the caller explicitly connects or starts listening.

use super::{Socket, bind_interface, refuse, restrict_hops, set_option, socket_policy};
use aterm_uds::connect::Inet;
use std::{
    io,
    net::{SocketAddr, TcpStream},
    os::fd::AsRawFd,
};

// XNU bsd/sys/socket.h; the bounded aterm-libc facade omits this Apple option.
#[cfg(target_os = "macos")]
pub(super) const NO_SIGPIPE: i32 = 0x1022;

pub(super) fn bound(source: SocketAddr, interface: Option<(u32, &str)>) -> io::Result<TcpStream> {
    if !super::super::unambiguous(source) || interface.is_some_and(|(index, _)| index == 0) {
        return Err(refuse("invalid local socket binding"));
    }
    let family = if source.is_ipv4() {
        libc::AF_INET
    } else {
        libc::AF_INET6
    };
    // Kept out of children as it was created (`Socket::of_kind`).
    let stream = TcpStream::from(Socket::of_kind(family, libc::SOCK_STREAM, libc::IPPROTO_TCP)?.0);
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
    let address = Inet::new(source);
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
