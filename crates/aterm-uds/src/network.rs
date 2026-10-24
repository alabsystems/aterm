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
