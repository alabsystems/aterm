// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Preparation of IPv4 multicast discovery; never an authenticated data path.

use super::{Socket, bind_interface, option, set_option, socket_policy, tcp::Address};
use std::{
    io,
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    os::fd::AsRawFd,
};

// XNU bsd/netinet/in.h (9), Linux include/uapi/linux/in.h (32).
// The minimal libc facade does not export this option on either platform.
const MULTICAST_IF: i32 = if cfg!(target_os = "linux") { 32 } else { 9 };

pub(in crate::lan) fn bind(
    source: Ipv4Addr,
    group: Ipv4Addr,
    port: u16,
    interface: Option<(u32, &str)>,
) -> io::Result<UdpSocket> {
    let socket =
        UdpSocket::from(Socket::of_kind(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_UDP)?.0);
    if let Some((index, name)) = interface {
        bind_interface(&socket, index, name)?;
    }
    set_option(&socket, libc::SOL_SOCKET, libc::SO_REUSEADDR, 1)?;
    set_option(&socket, libc::SOL_SOCKET, libc::SO_REUSEPORT, 1)?;
    socket.set_multicast_ttl_v4(1)?;
    socket.set_multicast_loop_v4(true)?;
    // Both kernels accept in_addr here. std handles their different TTL types.
    set_option(
        &socket,
        libc::IPPROTO_IP,
        MULTICAST_IF,
        u32::from_ne_bytes(source.octets()),
    )?;
    let address = Address::new(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)));
    let (pointer, length) = address.parts();
    // SAFETY: address owns the initialized sockaddr for this complete call.
    if unsafe { libc::bind(socket.as_raw_fd(), pointer, length) } != 0 {
        return Err(io::Error::last_os_error());
    }
    policy(&socket, source, interface.map(|(index, _)| index))?;
    socket.join_multicast_v4(&group, &source)?;
    Ok(socket)
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
