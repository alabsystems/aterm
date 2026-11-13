// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
