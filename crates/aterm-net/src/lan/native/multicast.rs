// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Preparation of IPv4 multicast discovery; never an authenticated data path.

use super::{Socket, bind_interface, option, set_option, socket_policy};
use aterm_uds::connect::Inet;
use std::{
    io,
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    os::fd::AsRawFd,
};

// XNU bsd/netinet/in.h (9), Linux include/uapi/linux/in.h (32).
// The minimal libc facade does not export this option on either platform.
const MULTICAST_IF: i32 = if cfg!(target_os = "linux") { 32 } else { 9 };

/// The receive room, in bytes, a discovery socket asks for ([`bind`]). Its owner reads it once
/// a polled round, not as each datagram comes, so what a round's length brings waits in the
/// socket: 320 datagrams, the most one read takes (aterm-db's `HEARD`), come to 384,000 bytes
/// at the largest a discovery datagram may be (`crate::lan::MAX_PACKET`), and to 737,280 where
/// a kernel charges each datagram a 2,304-byte buffer. A kernel may grant less than is asked
/// (Linux grants no more than its `net.core.rmem_max`): the socket keeps what it is granted,
/// never less than it had, and a socket with no room left drops what comes, which for a
/// beacon is a hint its sender repeats.
const ROOM: u32 = 1 << 20;

/// A socket bound to the wildcard address on `port`, sharing it (`SO_REUSEADDR`,
/// `SO_REUSEPORT`), with `group` joined on interface `index` and a one-hop multicast limit. A
/// LAN attachment's (`interface` named) is made by `aterm_uds::lan::udp`: on macOS by the root
/// LAN broker, which passes it here (macOS's Local Network privacy gates the bind and join of a
/// process of the person's, never of root's); elsewhere here. A loopback one (no `interface`)
/// reaches no LAN and is made here ([`loopback`]), as a loopback [`crate::lan::Connection`] is
/// dialed here. This process then binds it to `interface` (where
/// one is named), names `source` as the address its multicast leaves from and keeps its own
/// sends looped back, before [`policy`] checks the result.
///
/// # Errors
/// For an attachment on macOS a missing LAN broker (`NotConnected`, its words naming what
/// installs it) or a port or group the broker's rule refuses (`aterm_uds::lan::here::check_udp`);
/// or a socket, bind, join or option error.
pub(in crate::lan) fn bind(
    source: Ipv4Addr,
    group: Ipv4Addr,
    port: u16,
    index: u32,
    interface: Option<&str>,
) -> io::Result<UdpSocket> {
    let options = aterm_uds::lan::UdpOptions {
        bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)),
        reuse: true,
        groups: vec![(group.into(), Some(index))],
        multicast_if: None,
        ttl: Some(1),
    };
    let socket = if interface.is_some() {
        aterm_uds::lan::udp(options).map_err(aterm_uds::lan::LanError::into_io)?
    } else {
        loopback(source, group, port)?
    };
    aterm_uds::keep_out_of_children(&socket)?;
    socket.set_nonblocking(true)?;
    // Only ever raised: a default already larger stays, and a refusal leaves the default.
    if option(&socket, libc::SOL_SOCKET, libc::SO_RCVBUF)
        .is_ok_and(|room| room < ROOM.cast_signed())
    {
        let _ = set_option(&socket, libc::SOL_SOCKET, libc::SO_RCVBUF, ROOM);
    }
    if let Some(name) = interface {
        bind_interface(&socket, index, name)?;
    }
    socket.set_multicast_loop_v4(true)?;
    // Both kernels accept in_addr here. std handles their different TTL types.
    set_option(
        &socket,
        libc::IPPROTO_IP,
        MULTICAST_IF,
        u32::from_ne_bytes(source.octets()),
    )?;
    policy(&socket, source, interface.map(|_| index))?;
    Ok(socket)
}

/// A loopback discovery socket made in this process: shared, bound to the wildcard address on
/// `port` (any port: no root broker binds it, so the broker's port rule has nothing to guard),
/// with `group` joined on `source`'s interface. [`bind`] configures the rest.
fn loopback(source: Ipv4Addr, group: Ipv4Addr, port: u16) -> io::Result<UdpSocket> {
    let socket =
        UdpSocket::from(Socket::of_kind(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_UDP)?.0);
    set_option(&socket, libc::SOL_SOCKET, libc::SO_REUSEADDR, 1)?;
    set_option(&socket, libc::SOL_SOCKET, libc::SO_REUSEPORT, 1)?;
    socket.set_multicast_ttl_v4(1)?;
    let address = Inet::new(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)));
    let (pointer, length) = address.parts();
    // SAFETY: address owns the initialized sockaddr for this complete call.
    if unsafe { libc::bind(socket.as_raw_fd(), pointer, length) } != 0 {
        return Err(io::Error::last_os_error());
    }
    socket.join_multicast_v4(&group, &source)?;
    Ok(socket)
}

/// `socket`'s receive room in bytes, as its kernel reports it (tests), with what [`bind`]
/// asks for.
#[cfg(test)]
pub(in crate::lan) fn room(socket: &UdpSocket) -> io::Result<(usize, usize)> {
    let room = option(socket, libc::SOL_SOCKET, libc::SO_RCVBUF)?;
    Ok((usize::try_from(room).unwrap_or(0), ROOM as usize))
}

pub(in crate::lan) fn policy(
    socket: &UdpSocket,
    source: Ipv4Addr,
    bound: Option<u32>,
) -> io::Result<()> {
    socket_policy(socket, bound, false)?;
    if socket.multicast_ttl_v4()? != 1
        || !socket.multicast_loop_v4()?
        || option(socket, libc::IPPROTO_IP, MULTICAST_IF)? as u32
            != u32::from_ne_bytes(source.octets())
    {
        return Err(super::refuse("multicast socket policy changed"));
    }
    Ok(())
}
