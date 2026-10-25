// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux rtnetlink route lookup, using the kernel UAPI's fixed wire layout.
//! No table dump approximates policy routing: RTM_GETROUTE asks the kernel for
//! this destination, source, uid, protocol and port tuple.

#[cfg(target_os = "linux")]
use super::{MAX_MESSAGES, MAX_REPLY, Socket, sequence};
use super::{Route, align4, refuse, u16_at, u32_at};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

const HEADER: usize = 16;
const MESSAGE: usize = 12;

fn attribute(wire: &mut Vec<u8>, kind: u16, bytes: &[u8]) {
    let length = bytes.len() + 4;
    wire.extend_from_slice(&(length as u16).to_ne_bytes());
    wire.extend_from_slice(&kind.to_ne_bytes());
    wire.extend_from_slice(bytes);
    while !wire.len().is_multiple_of(4) {
        wire.push(0);
    }
}
fn octets(address: IpAddr) -> Vec<u8> {
    match address {
        IpAddr::V4(ip) => ip.octets().to_vec(),
        IpAddr::V6(ip) => ip.octets().to_vec(),
    }
}
fn request(
    local: Option<SocketAddr>,
    peer: SocketAddr,
    sequence: u32,
    uid: u32,
    bound: Option<u32>,
) -> Vec<u8> {
    let mut wire = vec![0; HEADER + MESSAGE];
    wire[4..6].copy_from_slice(&26_u16.to_ne_bytes()); // RTM_GETROUTE
    wire[6..8].copy_from_slice(&1_u16.to_ne_bytes()); // NLM_F_REQUEST
    wire[8..12].copy_from_slice(&sequence.to_ne_bytes());
    wire[16] = if peer.is_ipv4() { 2 } else { 10 };
    wire[17] = if peer.is_ipv4() { 32 } else { 128 };
    attribute(&mut wire, 1, &octets(peer.ip())); // RTA_DST
    if let Some(local) = local {
        wire[18] = if local.is_ipv4() { 32 } else { 128 };
        attribute(&mut wire, 2, &octets(local.ip())); // RTA_SRC
        attribute(&mut wire, 28, &local.port().to_be_bytes()); // RTA_SPORT
    }
    let zone = match peer {
        SocketAddr::V6(peer) => peer.scope_id(),
        _ => 0,
    };
    if let Some(index) = bound.or((zone != 0).then_some(zone)) {
        attribute(&mut wire, 4, &index.to_ne_bytes()); // RTA_OIF: actual binding or IPv6 zone.
    }
    attribute(&mut wire, 25, &uid.to_ne_bytes()); // RTA_UID
    attribute(&mut wire, 27, &[6]); // RTA_IP_PROTO: TCP
    attribute(&mut wire, 29, &peer.port().to_be_bytes()); // RTA_DPORT
    let length = wire.len() as u32;
    wire[..4].copy_from_slice(&length.to_ne_bytes());
    wire
}

fn address(bytes: &[u8], v4: bool) -> io::Result<IpAddr> {
    if v4 {
        bytes
            .try_into()
            .map(|a: [u8; 4]| Ipv4Addr::from(a).into())
            .map_err(|_| refuse("invalid IPv4 route address"))
    } else {
        bytes
            .try_into()
            .map(|a: [u8; 16]| Ipv6Addr::from(a).into())
            .map_err(|_| refuse("invalid IPv6 route address"))
    }
}

fn parse(bytes: &[u8], sequence: u32, peer: SocketAddr) -> io::Result<Option<Route>> {
    if bytes.is_empty() {
        return Err(refuse("empty netlink reply"));
    }
    let mut at = 0;
    while at < bytes.len() {
        let header = bytes
            .get(at..at + HEADER)
            .ok_or_else(|| refuse("truncated netlink header"))?;
        let length = u32_at(header, 0)? as usize;
        if length < HEADER {
            return Err(refuse("invalid netlink length"));
        }
        let end = at
            .checked_add(length)
            .ok_or_else(|| refuse("netlink length overflow"))?;
        let message = bytes
            .get(at..end)
            .ok_or_else(|| refuse("truncated netlink message"))?;
        at = at
            .checked_add(align4(length)?)
            .ok_or_else(|| refuse("netlink length overflow"))?;
        if at > bytes.len() {
            return Err(refuse("truncated netlink padding"));
        }
        if u32_at(header, 8)? != sequence {
            continue;
        }
        if u16_at(header, 4)? != 24 {
            // RTM_NEWROUTE; errors and dump terminators cannot authorize.
            return Err(refuse("kernel did not return a route"));
        }
        if message.len() < HEADER + MESSAGE || message[16] != if peer.is_ipv4() { 2 } else { 10 } {
            return Err(refuse("route reply has another address family"));
        }
        let mut index = None;
        let mut source = None;
        let mut destination = None;
        let mut seen = std::collections::BTreeSet::new();
        let mut direct = message[23] == 1
            && matches!(message[22], 0 | 253)
            && u32_at(message, 24)? & !(0x200 | 0x4000) == 0; // ordinary/cache/offloaded unicast.
        let mut cursor = HEADER + MESSAGE;
        while cursor < message.len() {
            let length = usize::from(u16_at(message, cursor)?);
            let encoded_kind = u16_at(message, cursor + 2)?;
            let kind = encoded_kind & 0x3fff;
            if encoded_kind != kind && !(kind == 8 && encoded_kind == 0x8008) {
                return Err(refuse("unsupported route attribute flags"));
            }
            // RTA_PAD can recur to align independent 64-bit attributes.
            if length < 4 || (kind != 24 && !seen.insert(kind)) {
                return Err(refuse("invalid or duplicate route attribute"));
            }
            let value = message
                .get(cursor + 4..cursor + length)
                .ok_or_else(|| refuse("truncated route attribute"))?;
            cursor = cursor
                .checked_add(align4(length)?)
                .ok_or_else(|| refuse("route attribute overflow"))?;
            if cursor > message.len() {
                return Err(refuse("truncated route attribute padding"));
            }
            match kind {
                24 if !value.is_empty() => return Err(refuse("invalid route padding")),
                1 => destination = Some(address(value, peer.is_ipv4())?),
                4 => {
                    if value.len() != 4 {
                        return Err(refuse("invalid route interface index"));
                    }
                    index = Some(u32_at(value, 0)?);
                }
                7 => source = Some(address(value, peer.is_ipv4())?),
                // Gateway, input path, multipath, translation, encapsulation,
                // nexthop objects and unknown extensions are not direct evidence.
                3 | 5 | 9 | 18 | 19 | 21 | 22 | 30 => direct = false,
                2 | 6 | 8 | 11 | 12 | 15 | 20 | 23 | 24 | 25 | 26 | 27 | 28 | 29 | 31 => {}
                _ => direct = false,
            }
        }
        if destination != Some(peer.ip()) {
            return Err(refuse("route reply names another destination"));
        }
        let index = index
            .filter(|index| *index != 0)
            .ok_or_else(|| refuse("route has no output interface"))?;
        return Ok(Some(Route {
            index,
            source,
            direct,
        }));
    }
    Ok(None)
}

#[cfg(target_os = "linux")]
pub(super) fn lookup(
    local: Option<SocketAddr>,
    peer: SocketAddr,
    bound: Option<u32>,
    until: std::time::Instant,
) -> io::Result<Route> {
    use std::os::fd::AsRawFd;
    let sequence = sequence()?;
    // SAFETY: geteuid has no arguments or memory preconditions.
    let wire = request(local, peer, sequence, unsafe { libc::geteuid() }, bound);
    let socket = Socket::new(libc::AF_NETLINK, libc::NETLINK_ROUTE)?;
    // SAFETY: zero is a valid sockaddr_nl initialization, including its padding.
    let mut kernel: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    kernel.nl_family = libc::AF_NETLINK as _;
    // SAFETY: wire is readable, kernel is a live sockaddr_nl, and fd is owned.
    let sent = unsafe {
        libc::sendto(
            socket.0.as_raw_fd(),
            wire.as_ptr().cast(),
            wire.len(),
            0,
            std::ptr::from_ref(&kernel).cast(),
            std::mem::size_of_val(&kernel) as _,
        )
    };
    if sent != wire.len() as isize {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0; MAX_REPLY];
    for _ in 0..MAX_MESSAGES {
        socket.readable(until)?;
        // SAFETY: zero initializes a writable sender address and its padding.
        let mut sender: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of_val(&sender) as libc::socklen_t;
        // SAFETY: all output buffers and their lengths are live and writable.
        let received = unsafe {
            libc::recvfrom(
                socket.0.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                libc::MSG_DONTWAIT | libc::MSG_TRUNC,
                std::ptr::from_mut(&mut sender).cast(),
                &raw mut length,
            )
        };
        if received < 0 {
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) {
                continue;
            }
            return Err(error);
        }
        if received as usize > buffer.len()
            || length as usize != std::mem::size_of_val(&sender)
            || sender.nl_family != libc::AF_NETLINK as _
            || sender.nl_pid != 0
            || sender.nl_groups != 0
        {
            return Err(refuse("route reply is truncated or not from the kernel"));
        }
        if let Some(route) = parse(&buffer[..received as usize], sequence, peer)? {
            return Ok(route);
        }
    }
    Err(refuse("route query reply bound exceeded"))
}

#[cfg(target_os = "linux")]
pub(super) fn physical(name: &str) -> io::Result<bool> {
    use std::{fs::File, io::Read, path::Path};
    if name.is_empty()
        || name.len() > 15
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        || name == "."
        || name == ".."
    {
        return Ok(false);
    }
    let path = Path::new("/sys/class/net").join(name).canonicalize()?;
    if !path.starts_with("/sys/devices")
        || path.starts_with("/sys/devices/virtual")
        || !path.join("device").exists()
    {
        return Ok(false);
    }
    let mut kind = Vec::new();
    File::open(path.join("type"))?
        .take(33)
        .read_to_end(&mut kind)?;
    Ok(kind.len() <= 32 && kind == b"1\n") // ARPHRD_ETHER
}

#[cfg(test)]
mod tests;
