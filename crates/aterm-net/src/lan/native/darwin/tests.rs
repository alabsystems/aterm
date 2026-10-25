// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn link(index: u16, kind: u8) -> Vec<u8> {
    let mut link = vec![0; 20];
    link[0] = 20;
    link[1] = AF_LINK;
    link[2..4].copy_from_slice(&index.to_ne_bytes());
    link[4] = kind;
    link
}
fn inet(ip: [u8; 4]) -> Vec<u8> {
    let mut address = vec![0; 16];
    address[0] = 16;
    address[1] = AF_INET;
    address[4..8].copy_from_slice(&ip);
    address
}
fn reply() -> Vec<u8> {
    let mut bytes = vec![0; HEADER];
    bytes[2] = 5;
    bytes[3] = 4;
    bytes[4..6].copy_from_slice(&3_u16.to_ne_bytes());
    bytes[8..12].copy_from_slice(&5_u32.to_ne_bytes());
    bytes[12..16].copy_from_slice(&0x37_u32.to_ne_bytes());
    bytes[16..20].copy_from_slice(&77_u32.to_ne_bytes());
    bytes[20..24].copy_from_slice(&88_u32.to_ne_bytes());
    bytes.extend_from_slice(&inet([192, 0, 2, 2]));
    bytes.extend_from_slice(&link(3, IFT_ETHER));
    // A short sockaddr netmask is valid, with four-byte alignment padding.
    bytes.extend_from_slice(&[7, 0, 0, 0, 255, 255, 255, 0]);
    bytes.extend_from_slice(&link(3, IFT_ETHER));
    bytes.extend_from_slice(&inet([192, 0, 2, 1]));
    let len = bytes.len() as u16;
    bytes[..2].copy_from_slice(&len.to_ne_bytes());
    bytes
}

#[test]
fn direct_route_decoding_and_exact_reply_identity() {
    let bytes = reply();
    let route = parse(&bytes, 88, 77).unwrap().unwrap();
    assert!(route.direct);
    assert_eq!(route.index, 3);
    assert_eq!(route.source, Some("192.0.2.1".parse().unwrap()));
    assert_eq!(parse(&bytes, 89, 77).unwrap(), None);
    assert_eq!(parse(&bytes, 88, 78).unwrap(), None);
    let mut router = bytes.clone();
    router[8..12].copy_from_slice(&(5_u32 | 0x1000_0000).to_ne_bytes());
    assert!(parse(&router, 88, 77).unwrap().unwrap().direct);
    for denied in [
        0x2_u32, 0x8, 0x10, 0x20, 0x1000, 0x200000, 0x400000, 0x800000, 0x2000000, 0x8000000,
        0x20000000, 0x40000000,
    ] {
        let mut changed = bytes.clone();
        changed[8..12].copy_from_slice(&(5 | denied).to_ne_bytes());
        assert!(!parse(&changed, 88, 77).unwrap().unwrap().direct);
    }
}

#[test]
fn unrelated_short_kernel_notifications_do_not_abort_a_query() {
    for (kind, length) in [(0x0c, 68usize), (0x0f, 52)] {
        let mut bytes = vec![0; length];
        bytes[..2].copy_from_slice(&(length as u16).to_ne_bytes());
        bytes[2] = 5;
        bytes[3] = kind;
        assert_eq!(parse(&bytes, 88, 77).unwrap(), None);
    }
}

#[test]
fn an_interface_address_cannot_name_another_ipv6_zone() {
    let mut bytes = reply();
    bytes.truncate(bytes.len() - 16);
    let mut address = vec![0; 28];
    address[0] = 28;
    address[1] = AF_INET6;
    address[8..24].copy_from_slice(&"fe80::1".parse::<Ipv6Addr>().unwrap().octets());
    address[24..28].copy_from_slice(&9_u32.to_ne_bytes());
    bytes.extend_from_slice(&address);
    let length = bytes.len() as u16;
    bytes[..2].copy_from_slice(&length.to_ne_bytes());
    assert!(parse(&bytes, 88, 77).is_err());
    let start = bytes.len() - 4;
    bytes[start..].copy_from_slice(&3_u32.to_ne_bytes());
    assert!(parse(&bytes, 88, 77).unwrap().unwrap().direct);
}

#[test]
fn malformed_route_frames_and_conflicting_interfaces_fail_closed() {
    let bytes = reply();
    for end in 0..bytes.len() {
        assert!(parse(&bytes[..end], 88, 77).is_err());
    }
    let mut wrong = bytes.clone();
    wrong[4..6].copy_from_slice(&9_u16.to_ne_bytes());
    assert!(parse(&wrong, 88, 77).is_err());
    let mut wrong = bytes.clone();
    wrong[24] = 1;
    assert!(parse(&wrong, 88, 77).is_err());
    let mut wrong = bytes.clone();
    wrong[2] = 6;
    assert!(parse(&wrong, 88, 77).is_err());
    let mut wrong = bytes.clone();
    wrong.push(0);
    let len = wrong.len() as u16;
    wrong[..2].copy_from_slice(&len.to_ne_bytes());
    assert!(parse(&wrong, 88, 77).is_err());
    // Correct header, but the next sockaddr extends beyond the record.
    let mut wrong = bytes;
    wrong[HEADER] = 255;
    assert!(parse(&wrong, 88, 77).is_err());
}

#[test]
fn scoped_requests_are_native_bounded_and_read_only() {
    let peer: SocketAddr = "[fe80::2%3]:99".parse().unwrap();
    let bytes = request(peer, 88, 77, None).unwrap();
    assert_eq!(bytes[3], 4);
    assert_eq!(u16_at(&bytes, 4).unwrap(), 3);
    assert_eq!(u32_at(&bytes, 8).unwrap(), 0x0100_0000);
    assert_eq!(u32_at(&bytes, 12).unwrap(), 0x11);
    assert_eq!(&bytes[HEADER + 10..HEADER + 12], &3_u16.to_be_bytes());
    assert_eq!(u32_at(&bytes, HEADER + 24).unwrap(), 0);
    assert_eq!(usize::from(u16_at(&bytes, 0).unwrap()), bytes.len());
    assert!(request("[fe80::2%65536]:99".parse().unwrap(), 1, 1, None).is_err());
    let bound = request("192.0.2.1:99".parse().unwrap(), 88, 77, Some(3)).unwrap();
    assert_eq!(u16_at(&bound, 4).unwrap(), 3);
    assert_eq!(u32_at(&bound, 8).unwrap(), 0x0100_0000);
    assert!(request(peer, 88, 77, Some(4)).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn live_kernel_loopback_route_is_readable_without_connecting() {
    let route = lookup(
        None,
        "127.0.0.1:9".parse().unwrap(),
        None,
        super::super::deadline(),
    )
    .unwrap();
    assert_ne!(route.index, 0);
    assert!(!route.direct);
    assert!(!physical("lo0").unwrap());
    assert!(!physical("utun0").unwrap());
}

#[cfg(target_os = "macos")]
#[test]
fn live_link_local_route_uses_the_kernel_embedded_zone() {
    let index = super::super::index("lo0").unwrap();
    let peer = SocketAddr::V6(std::net::SocketAddrV6::new(
        "fe80::1".parse().unwrap(),
        9,
        0,
        index,
    ));
    for bound in [None, Some(index)] {
        let route = lookup(None, peer, bound, super::super::deadline()).unwrap();
        assert_eq!(route.index, index);
        assert!(!route.direct);
    }
}
