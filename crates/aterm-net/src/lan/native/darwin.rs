// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! XNU RTM_GET wire ABI, from Apple's bsd/net/route.h and network_cmds/route.
//! Only bounded byte parsing is used; no unaligned native struct references.

#[cfg(target_os = "macos")]
use super::{MAX_MESSAGES, MAX_REPLY, Socket, sequence};
use super::{Route, align4, refuse, u16_at, u32_at};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

const HEADER: usize = 92;
const AF_INET: u8 = 2;
const AF_INET6: u8 = 30;
const AF_LINK: u8 = 18;
const IFT_ETHER: u8 = 6;

fn request(peer: SocketAddr, sequence: u32, pid: u32, bound: Option<u32>) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0; HEADER];
    bytes[2] = 5; // RTM_VERSION
    bytes[3] = 4; // RTM_GET, never a mutating route message.
    let zone = match peer {
        SocketAddr::V6(address) => address.scope_id(),
        _ => 0,
    };
    if matches!(peer, SocketAddr::V6(address) if address.ip().is_unicast_link_local() && zone == 0)
    {
        return Err(refuse("link-local route query requires an explicit zone"));
    }
    if bound.is_some_and(|index| index == 0 || (zone != 0 && zone != index)) {
        return Err(refuse("bound interface differs from the IPv6 zone"));
    }
    if bound.is_some() || zone != 0 {
        let scope: u16 = bound
            .unwrap_or(zone)
            .try_into()
            .map_err(|_| refuse("IPv6 zone exceeds Darwin route index"))?;
        bytes[4..6].copy_from_slice(&scope.to_ne_bytes());
        if scope != 0 {
            bytes[8..12].copy_from_slice(&0x0100_0000_u32.to_ne_bytes());
        }
    }
    // RTA_DST | RTA_IFP. Asking for the interface also returns its address.
    bytes[12..16].copy_from_slice(&0x11_u32.to_ne_bytes());
    bytes[16..20].copy_from_slice(&pid.to_ne_bytes());
    bytes[20..24].copy_from_slice(&sequence.to_ne_bytes());
    match peer {
        SocketAddr::V4(address) => {
            let mut sockaddr = [0; 16];
            sockaddr[0] = 16;
            sockaddr[1] = AF_INET;
            sockaddr[4..8].copy_from_slice(&address.ip().octets());
            bytes.extend_from_slice(&sockaddr);
        }
        SocketAddr::V6(address) => {
            let mut sockaddr = [0; 28];
            sockaddr[0] = 28;
            sockaddr[1] = AF_INET6;
            sockaddr[8..24].copy_from_slice(&address.ip().octets());
            if address.ip().is_unicast_link_local() {
                // PF_ROUTE uses the KAME radix-key form, unlike bind/connect.
                // The validated zone is embedded in s6_addr[2..4].
                let zone: u16 = address
                    .scope_id()
                    .try_into()
                    .map_err(|_| refuse("IPv6 zone exceeds Darwin route index"))?;
                sockaddr[10..12].copy_from_slice(&zone.to_be_bytes());
            } else {
                sockaddr[24..28].copy_from_slice(&address.scope_id().to_ne_bytes());
            }
            bytes.extend_from_slice(&sockaddr);
        }
    }
    let mut interface = [0; 20]; // sockaddr_dl's fixed public prefix.
    interface[0] = 20;
    interface[1] = AF_LINK;
    bytes.extend_from_slice(&interface);
    let length: u16 = bytes
        .len()
        .try_into()
        .map_err(|_| refuse("route request exceeds its bound"))?;
    bytes[..2].copy_from_slice(&length.to_ne_bytes());
    Ok(bytes)
}

fn address(bytes: &[u8]) -> io::Result<IpAddr> {
    match bytes.get(1) {
        Some(&AF_INET) if bytes.len() >= 16 => {
            Ok(Ipv4Addr::from(<[u8; 4]>::try_from(&bytes[4..8]).unwrap()).into())
        }
        Some(&AF_INET6) if bytes.len() >= 28 => {
            Ok(Ipv6Addr::from(<[u8; 16]>::try_from(&bytes[8..24]).unwrap()).into())
        }
        _ => Err(refuse("route interface address has an unsupported family")),
    }
}

fn parse(bytes: &[u8], sequence: u32, pid: u32) -> io::Result<Option<Route>> {
    if bytes.len() < 4 || usize::from(u16_at(bytes, 0)?) != bytes.len() || bytes[2] != 5 {
        return Err(refuse("truncated Darwin route reply"));
    }
    // PF_ROUTE also broadcasts shorter interface/address notifications. Their
    // headers do not have an RTM_GET pid/sequence layout and are not our reply.
    if bytes[3] != 4 {
        return Ok(None);
    }
    if bytes.len() < HEADER {
        return Err(refuse("truncated Darwin route query reply"));
    }
    if u32_at(bytes, 16)? != pid || u32_at(bytes, 20)? != sequence {
        return Ok(None);
    }
    if bytes[2] != 5 || bytes[3] != 4 || u32_at(bytes, 24)? != 0 {
        return Err(refuse("Darwin rejected the route query"));
    }
    let flags = u32_at(bytes, 8)?;
    let fields = u32_at(bytes, 12)?;
    if fields & !0xff != 0 {
        return Err(refuse("unknown Darwin route address fields"));
    }
    let mut at = HEADER;
    let mut index = None;
    let mut source = None;
    let mut source_scope = 0;
    let mut ethernet = false;
    let mut gateway_index = None;
    for field in 0..8 {
        if fields & (1 << field) == 0 {
            continue;
        }
        let length = usize::from(
            *bytes
                .get(at)
                .ok_or_else(|| refuse("missing Darwin sockaddr"))?,
        );
        let padded = align4(length.max(4))?;
        let raw = bytes
            .get(at..at + length)
            .ok_or_else(|| refuse("truncated Darwin sockaddr"))?;
        if field == 4 || field == 1 {
            if raw.len() < 8 || raw[1] != AF_LINK {
                // An IP gateway is routed, not a direct link.
                if field == 1 {
                    gateway_index = Some(0);
                } else {
                    return Err(refuse("route interface is not a link address"));
                }
            } else {
                let value = u32::from(u16_at(raw, 2)?);
                if field == 4 {
                    index = Some(value);
                    ethernet = raw[4] == IFT_ETHER;
                } else {
                    gateway_index = Some(value);
                }
            }
        } else if field == 5 {
            source = Some(address(raw)?);
            if raw[1] == AF_INET6 {
                source_scope = u32_at(raw, 24)?;
            }
        }
        at = at
            .checked_add(padded)
            .ok_or_else(|| refuse("route address length overflow"))?;
        if at > bytes.len() {
            return Err(refuse("truncated route alignment padding"));
        }
    }
    if at != bytes.len() {
        return Err(refuse("trailing Darwin route bytes"));
    }
    let index = index
        .filter(|index| *index != 0)
        .ok_or_else(|| refuse("route has no interface index"))?;
    let header_index = u32::from(u16_at(bytes, 4)?);
    if header_index != 0 && header_index != index {
        return Err(refuse("ambiguous route interface"));
    }
    if source_scope != 0 && source_scope != index {
        return Err(refuse("route source zone differs from the interface"));
    }
    // UP, HOST, DONE, CLONING, LLINFO, STATIC, PROTO1/2/3, PRCLONING,
    // WASCLONED, PINNED, IFSCOPE, IFREF and ROUTER (the neighbor can itself
    // forward; this route still must be direct). Every other flag withholds.
    const ALLOWED: u32 = 0x1517_cd45;
    let direct = flags & 1 != 0
        && flags & !ALLOWED == 0
        && ethernet
        && gateway_index.is_none_or(|gateway| gateway == index);
    let source = source
        .map(|source| super::super::scoped_ip(source, index))
        .transpose()?;
    Ok(Some(Route {
        index,
        source,
        direct,
    }))
}

#[cfg(target_os = "macos")]
pub(super) fn lookup(
    _local: Option<SocketAddr>,
    peer: SocketAddr,
    bound: Option<u32>,
    until: std::time::Instant,
) -> io::Result<Route> {
    use std::os::fd::AsRawFd;
    let sequence = sequence()?;
    let pid = std::process::id();
    let bytes = request(peer, sequence, pid, bound)?;
    let socket = Socket::new(libc::PF_ROUTE, 0)?;
    // SAFETY: bytes is readable for its stated length and the descriptor is live.
    let sent = unsafe { libc::write(socket.0.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
    if sent != bytes.len() as isize {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0; MAX_REPLY];
    for _ in 0..MAX_MESSAGES {
        socket.readable(until)?;
        // SAFETY: buffer is writable for its full length on this live socket.
        let received = unsafe {
            libc::recv(
                socket.0.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                libc::MSG_DONTWAIT,
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
        if let Some(route) = parse(&buffer[..received as usize], sequence, pid)? {
            return Ok(route);
        }
    }
    Err(refuse("route query reply bound exceeded"))
}

#[cfg(target_os = "macos")]
pub(super) fn physical(name: &str) -> io::Result<bool> {
    use std::ffi::CStr;
    if !name
        .strip_prefix("en")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    {
        return Ok(false);
    }
    let mut head = std::ptr::null_mut();
    // SAFETY: getifaddrs initializes this pointer on success.
    if unsafe { libc::getifaddrs(&raw mut head) } != 0 {
        return Err(io::Error::last_os_error());
    }
    struct List(*mut libc::ifaddrs);
    impl Drop for List {
        fn drop(&mut self) {
            // SAFETY: this list came from the successful getifaddrs call.
            unsafe { libc::freeifaddrs(self.0) };
        }
    }
    let list = List(head);
    let mut at = list.0;
    let mut result = None;
    while !at.is_null() {
        // SAFETY: the live immutable getifaddrs list owns this node.
        let node = unsafe { &*at };
        at = node.ifa_next;
        if node.ifa_name.is_null() || node.ifa_addr.is_null() {
            continue;
        }
        // SAFETY: this live node provides a nul-terminated name and sockaddr header.
        if unsafe { CStr::from_ptr(node.ifa_name) }.to_bytes() != name.as_bytes()
            || unsafe { (*node.ifa_addr).sa_family } != AF_LINK
        {
            continue;
        }
        // SAFETY: an AF_LINK address's public prefix includes type at byte 4
        // only if the kernel's advertised sockaddr length contains it.
        let length = unsafe { (*node.ifa_addr).sa_len };
        if length < 8 {
            return Err(refuse("truncated interface link address"));
        }
        // SAFETY: length above establishes the readable eight-byte link prefix.
        let link = unsafe { std::slice::from_raw_parts(node.ifa_addr.cast::<u8>(), 8) };
        let ethernet = link[4] == IFT_ETHER;
        if result.is_some_and(|old| old != ethernet) {
            return Err(refuse("ambiguous interface link type"));
        }
        result = Some(ethernet);
    }
    Ok(result == Some(true))
}

#[cfg(test)]
mod tests;
