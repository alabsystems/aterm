// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local interface facts for background retry decisions. No socket is connected:
//! even a UDP connect without traffic can ask for Local Network permission.

use std::ffi::CStr;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// An address on an interface that is up and is not loopback. The mask and
/// flags also distinguish a changed attachment whose address stayed the same.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InterfaceAddress {
    pub name: String,
    pub address: IpAddr,
    pub netmask: Option<IpAddr>,
    pub flags: u32,
}

/// Read local interface state without DNS, network traffic or a connection.
/// Results are sorted and deduplicated, so enumeration order cannot wake a retry.
pub fn interfaces() -> std::io::Result<Vec<InterfaceAddress>> {
    let mut head = std::ptr::null_mut();
    // SAFETY: getifaddrs initializes this head pointer on success.
    if unsafe { libc::getifaddrs(&raw mut head) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    struct List(*mut libc::ifaddrs);
    impl Drop for List {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: this head came from getifaddrs and is freed once.
                unsafe { libc::freeifaddrs(self.0) };
            }
        }
    }
    let list = List(head);
    // SAFETY: list owns the live getifaddrs allocation for the whole read.
    Ok(unsafe { collect(list.0) })
}

/// # Safety
/// `at` heads a live ifaddrs list with valid names and family-appropriate
/// addresses/masks. Nothing in the list is modified or freed during this read.
unsafe fn collect(mut at: *const libc::ifaddrs) -> Vec<InterfaceAddress> {
    let mut out = Vec::new();
    while !at.is_null() {
        // SAFETY: the caller supplies a live linked list.
        let node = unsafe { &*at };
        at = node.ifa_next;
        if node.ifa_flags & libc::IFF_UP as u32 == 0
            || node.ifa_flags & libc::IFF_LOOPBACK as u32 != 0
            || node.ifa_name.is_null()
            || node.ifa_addr.is_null()
        {
            continue;
        }
        // SAFETY: ifa_addr points to the address's readable sockaddr header.
        let family = i32::from(unsafe { (*node.ifa_addr).sa_family });
        // SAFETY: the caller provides the sockaddr appropriate to this family.
        let Some(address) = (unsafe { sockaddr_ip(node.ifa_addr, family) }) else {
            continue;
        };
        if address.is_unspecified() || address.is_loopback() {
            continue;
        }
        // SAFETY: getifaddrs supplies a live nul-terminated interface name.
        let name = unsafe { CStr::from_ptr(node.ifa_name) }
            .to_string_lossy()
            .into_owned();
        let netmask = if node.ifa_netmask.is_null() {
            None
        } else {
            // SAFETY: the mask has this address's family; on Darwin its sa_len
            // can be short, which sockaddr_ip reads within that length.
            unsafe { sockaddr_ip(node.ifa_netmask, family) }
        };
        out.push(InterfaceAddress {
            name,
            address,
            netmask,
            flags: node.ifa_flags,
        });
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// # Safety
/// `at` points to a live sockaddr for `family`. On Darwin only sa_len bytes
/// need be readable, including the header; missing mask bytes are zero.
unsafe fn sockaddr_ip(at: *const libc::sockaddr, family: i32) -> Option<IpAddr> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: the header and its advertised bytes are readable by contract.
        let bytes =
            unsafe { std::slice::from_raw_parts(at.cast::<u8>(), usize::from((*at).sa_len)) };
        let octet = |offset: usize| bytes.get(offset).copied().unwrap_or(0);
        match family {
            libc::AF_INET => {
                let start = std::mem::offset_of!(libc::sockaddr_in, sin_addr);
                Some(Ipv4Addr::from(std::array::from_fn::<_, 4, _>(|i| octet(start + i))).into())
            }
            libc::AF_INET6 => {
                let start = std::mem::offset_of!(libc::sockaddr_in6, sin6_addr);
                Some(Ipv6Addr::from(std::array::from_fn::<_, 16, _>(|i| octet(start + i))).into())
            }
            _ => None,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        match family {
            libc::AF_INET => {
                // SAFETY: family identifies a complete sockaddr_in by contract.
                let value = unsafe { std::ptr::read_unaligned(at.cast::<libc::sockaddr_in>()) };
                Some(Ipv4Addr::from(value.sin_addr.s_addr.to_ne_bytes()).into())
            }
            libc::AF_INET6 => {
                // SAFETY: family identifies a complete sockaddr_in6 by contract.
                let value = unsafe { std::ptr::read_unaligned(at.cast::<libc::sockaddr_in6>()) };
                Some(Ipv6Addr::from(value.sin6_addr.s6_addr).into())
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_interfaces_are_stable_and_keep_attachment_changes() {
        // SAFETY: zero is a valid initial value for these sockaddr records.
        let mut v4: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        // SAFETY: as above.
        let mut v6: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
        v4.sin_family = libc::AF_INET as _;
        v4.sin_addr.s_addr = u32::from_ne_bytes([192, 0, 2, 7]);
        v6.sin6_family = libc::AF_INET6 as _;
        v6.sin6_addr.s6_addr = "2001:db8::7".parse::<Ipv6Addr>().unwrap().octets();
        #[cfg(target_os = "macos")]
        {
            v4.sin_len = std::mem::size_of_val(&v4) as u8;
            v6.sin6_len = std::mem::size_of_val(&v6) as u8;
        }
        struct Node {
            value: libc::ifaddrs,
            _next: Option<Box<Node>>,
        }
        let snapshot = |reverse: bool, running: bool| {
            let up = libc::IFF_UP as u32;
            let mut entries = vec![
                (false, up),
                (
                    true,
                    up | if running { libc::IFF_RUNNING as u32 } else { 0 },
                ),
                (false, up),
                (false, 0),
                (false, up | libc::IFF_LOOPBACK as u32),
            ];
            if reverse {
                entries.reverse();
            }
            let mut list: Option<Box<Node>> = None;
            for (ipv6, flags) in entries {
                // SAFETY: zero is a valid initial value for this C record.
                let mut value: libc::ifaddrs = unsafe { std::mem::zeroed() };
                value.ifa_next = list.as_ref().map_or(std::ptr::null_mut(), |next| {
                    std::ptr::from_ref(&next.value).cast_mut()
                });
                value.ifa_name = if ipv6 { c"en0" } else { c"en1" }.as_ptr().cast_mut();
                value.ifa_flags = flags;
                value.ifa_addr = if ipv6 {
                    std::ptr::from_ref(&v6).cast_mut().cast()
                } else {
                    std::ptr::from_ref(&v4).cast_mut().cast()
                };
                list = Some(Box::new(Node { value, _next: list }));
            }
            // SAFETY: every node, name and family-appropriate address stays
            // live and immutable until the read finishes. Boxed links do not move.
            unsafe { collect(std::ptr::from_ref(&list.as_ref().unwrap().value)) }
        };
        let before = snapshot(false, false);
        assert_eq!(before.len(), 2);
        assert_eq!(before[0].name, "en0");
        assert_eq!(before[1].address, "192.0.2.7".parse::<IpAddr>().unwrap());
        assert_eq!(before, snapshot(true, false));
        assert_ne!(before, snapshot(false, true), "link state changes matter");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_short_darwin_netmask_never_reads_past_its_length() {
        let mask = [6u8, libc::AF_INET as u8, 0, 0, 255, 255];
        // SAFETY: this is a live sockaddr header and its six advertised bytes.
        let value = unsafe { sockaddr_ip(mask.as_ptr().cast(), libc::AF_INET) };
        assert_eq!(value, Some(Ipv4Addr::new(255, 255, 0, 0).into()));
    }
}

/// A socket the kernel writes to on each change to this machine's addresses and links (and, on
/// Linux, routes): `PF_ROUTE` on macOS, `NETLINK_ROUTE` joined to those groups on Linux. A retry
/// that waits on it starts at the change (another Wi-Fi, a VPN, a wake), not at its next look.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub struct RouteChanges(std::os::fd::OwnedFd);

/// The netlink groups [`RouteChanges`] joins (`linux/rtnetlink.h`): links, and IPv4 and IPv6
/// addresses and routes; never neighbours, whose cache churns with LAN traffic.
#[cfg(target_os = "linux")]
const RTMGRP_CHANGES: u32 = 0x1 | 0x10 | 0x40 | 0x100 | 0x400;

/// How long [`RouteChanges::wait`] gathers the rest of a burst once a change came: one change
/// (an interface coming up) is several messages within milliseconds.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const BURST: std::time::Duration = std::time::Duration::from_millis(100);

/// The `PF_ROUTE` message types (`net/route.h`) that name a change of network: `RTM_NEWADDR`,
/// `RTM_DELADDR`, `RTM_IFINFO` and `RTM_IFINFO2`.
#[cfg(target_os = "macos")]
const NETWORK_CHANGES: [u8; 4] = [0xc, 0xd, 0xe, 0x12];

/// Whether one message a [`RouteChanges`] socket read names a change of network. On macOS
/// `PF_ROUTE` carries every routing-table message, the ARP and neighbour entries that come and
/// go with LAN traffic and each lookup's miss included: only an address added or removed, or a
/// link's state, counts (`rtm_type`, the fourth byte of every header). On Linux the groups
/// joined carry only changes. A read that overflowed (`ENOBUFS`) lost messages, so it counts.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn names_a_change(message: &[u8]) -> bool {
    #[cfg(target_os = "macos")]
    {
        message
            .get(3)
            .is_some_and(|kind| NETWORK_CHANGES.contains(kind))
    }
    #[cfg(target_os = "linux")]
    {
        !message.is_empty()
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl RouteChanges {
    /// A socket that hears each change from now on; `None` where the kernel refuses one.
    pub fn open() -> Option<RouteChanges> {
        use std::os::fd::FromRawFd as _;
        #[cfg(target_os = "macos")]
        let (domain, kind, protocol) = (libc::PF_ROUTE, libc::SOCK_RAW, libc::AF_UNSPEC);
        #[cfg(target_os = "linux")]
        let (domain, kind, protocol) = (
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            libc::NETLINK_ROUTE,
        );
        // SAFETY: socket(2) takes no pointer; a descriptor it returns is owned below.
        let raw = unsafe { libc::socket(domain, kind, protocol) };
        if raw < 0 {
            return None;
        }
        // SAFETY: `raw` is the fresh descriptor socket(2) returned, owned by nothing else.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        #[cfg(target_os = "macos")]
        {
            // SAFETY: F_SETFD takes an integer flag on this live descriptor.
            if unsafe { libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                return None;
            }
        }
        #[cfg(target_os = "linux")]
        {
            // SAFETY: an all-zero sockaddr_nl is a valid value of the plain C struct.
            let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
            address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
            address.nl_groups = RTMGRP_CHANGES;
            // SAFETY: `address` is a live sockaddr_nl and the length passed is its size.
            let bound = unsafe {
                libc::bind(
                    raw,
                    std::ptr::from_ref(&address).cast(),
                    std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
                )
            };
            if bound < 0 {
                return None;
            }
        }
        crate::keep_out_of_children(&fd).ok()?;
        Some(RouteChanges(fd))
    }

    /// Wait at most `most` for a change of network ([`names_a_change`]), then read the rest of
    /// its burst: whether one came. Messages that name none are read and passed over. A socket
    /// that fails otherwise waits out `most` and says none came, so a retry that waits on it
    /// keeps its own pace.
    pub fn wait(&self, most: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + most;
        let wait_out = || {
            std::thread::sleep(deadline.saturating_duration_since(std::time::Instant::now()));
            false
        };
        loop {
            match self.readable(deadline) {
                Some(true) => {}
                Some(false) => return false,
                None => return wait_out(),
            }
            match self.drain() {
                Some(true) => {
                    let burst = std::time::Instant::now() + BURST;
                    while self.readable(burst) == Some(true) && self.drain().is_some() {}
                    return true;
                }
                Some(false) => {}
                None => return wait_out(),
            }
        }
    }

    /// Whether a message is waiting by `deadline`; `None` when poll(2) failed otherwise than
    /// by a signal's interruption (which looks again).
    fn readable(&self, deadline: std::time::Instant) -> Option<bool> {
        use std::os::fd::AsRawFd as _;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let mut poll = libc::pollfd {
                fd: self.0.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let millis = libc::c_int::try_from(left.as_millis()).unwrap_or(libc::c_int::MAX);
            // SAFETY: `poll` is one live pollfd and the count passed is 1.
            let ready = unsafe { libc::poll(&raw mut poll, 1, millis) };
            if ready >= 0 {
                return Some(ready > 0);
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return None;
            }
        }
    }

    /// Read every message waiting: whether one named a change of network; `None` when the
    /// socket failed (an overflow, which lost messages, counts as a change).
    fn drain(&self) -> Option<bool> {
        use std::os::fd::AsRawFd as _;
        let mut buf = [0u8; 8192];
        let mut changed = false;
        loop {
            // SAFETY: `buf` is live and writable for its whole length.
            let read = unsafe {
                libc::recv(
                    self.0.as_raw_fd(),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    libc::MSG_DONTWAIT,
                )
            };
            if let Ok(read) = usize::try_from(read)
                && read > 0
            {
                changed |= names_a_change(&buf[..read]);
                continue;
            }
            let error = std::io::Error::last_os_error();
            return match error.raw_os_error() {
                _ if read == 0 => changed.then_some(true),
                Some(libc::EAGAIN | libc::EINTR) => Some(changed),
                Some(libc::ENOBUFS) => Some(true),
                _ => changed.then_some(true),
            };
        }
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod route_change_tests {
    use super::*;

    /// The kernel gives a process its route-change socket, and a wait on it ends by its bound
    /// whether or not a change came meanwhile.
    #[test]
    fn a_route_change_socket_opens_and_its_wait_ends() {
        let changes = RouteChanges::open().expect("the kernel's route socket");
        let _came = changes.wait(std::time::Duration::from_millis(50));
    }

    /// An address added or removed, or a link's state, is a change of network; the ARP and
    /// neighbour entries LAN traffic adds and drops, and a lookup's miss, are not (macOS's
    /// `PF_ROUTE` carries them all).
    #[cfg(target_os = "macos")]
    #[test]
    fn only_an_address_or_a_link_is_a_change_of_network() {
        let message = |kind: u8| {
            let mut header = vec![0u8; 92];
            header[2] = 5; // RTM_VERSION
            header[3] = kind;
            header
        };
        // RTM_NEWADDR, RTM_DELADDR, RTM_IFINFO, RTM_IFINFO2.
        for kind in [0xc, 0xd, 0xe, 0x12] {
            assert!(names_a_change(&message(kind)), "type {kind:#x}");
        }
        // RTM_ADD, RTM_DELETE, RTM_CHANGE, RTM_GET, RTM_MISS, RTM_RESOLVE, RTM_NEWMADDR.
        for kind in [0x1, 0x2, 0x3, 0x4, 0x7, 0xb, 0xf] {
            assert!(!names_a_change(&message(kind)), "type {kind:#x}");
        }
        assert!(!names_a_change(&[]));
    }
}
