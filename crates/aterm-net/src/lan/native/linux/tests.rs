// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn reply() -> Vec<u8> {
    let mut bytes = vec![0; HEADER + MESSAGE];
    bytes[4..6].copy_from_slice(&24_u16.to_ne_bytes());
    bytes[8..12].copy_from_slice(&88_u32.to_ne_bytes());
    bytes[16] = 2;
    bytes[17] = 32;
    bytes[22] = 253;
    bytes[23] = 1;
    attribute(&mut bytes, 1, &[192, 0, 2, 2]);
    attribute(&mut bytes, 4, &3_u32.to_ne_bytes());
    attribute(&mut bytes, 7, &[192, 0, 2, 1]);
    let len = bytes.len() as u32;
    bytes[..4].copy_from_slice(&len.to_ne_bytes());
    bytes
}
fn peer() -> SocketAddr {
    "192.0.2.2:99".parse().unwrap()
}

#[test]
fn direct_route_decoding_and_destination_identity() {
    let bytes = reply();
    let route = parse(&bytes, 88, peer()).unwrap().unwrap();
    assert!(route.direct);
    assert_eq!(route.index, 3);
    assert_eq!(route.source, Some("192.0.2.1".parse().unwrap()));
    assert_eq!(parse(&bytes, 89, peer()).unwrap(), None);
    assert!(parse(&bytes, 88, "192.0.2.3:99".parse().unwrap()).is_err());
    for kind in [3, 5, 9, 18, 19, 21, 22, 30, 99] {
        let mut bytes = reply();
        attribute(&mut bytes, kind, &[0; 4]);
        let len = bytes.len() as u32;
        bytes[..4].copy_from_slice(&len.to_ne_bytes());
        assert!(!parse(&bytes, 88, peer()).unwrap().unwrap().direct);
    }
    for kind in [2, 3, 6, 7, 8] {
        let mut bytes = reply();
        bytes[23] = kind;
        assert!(!parse(&bytes, 88, peer()).unwrap().unwrap().direct);
    }
}

#[test]
fn offload_metrics_and_repeated_empty_padding_keep_direct_route_evidence() {
    let mut bytes = reply();
    bytes[24..28].copy_from_slice(&0x4000_u32.to_ne_bytes());
    attribute(&mut bytes, 0x8008, &[0; 4]);
    attribute(&mut bytes, 24, &[]);
    attribute(&mut bytes, 24, &[]);
    let length = bytes.len() as u32;
    bytes[..4].copy_from_slice(&length.to_ne_bytes());
    assert!(parse(&bytes, 88, peer()).unwrap().unwrap().direct);
    attribute(&mut bytes, 24, &[1]);
    let length = bytes.len() as u32;
    bytes[..4].copy_from_slice(&length.to_ne_bytes());
    assert!(parse(&bytes, 88, peer()).is_err());
}

#[test]
fn malformed_route_attributes_do_not_authorize_or_allocate() {
    let bytes = reply();
    for end in 0..bytes.len() {
        assert!(parse(&bytes[..end], 88, peer()).is_err());
    }
    for length in [0_u16, 1, 3, u16::MAX] {
        let mut bad = bytes.clone();
        bad[28..30].copy_from_slice(&length.to_ne_bytes());
        assert!(parse(&bad, 88, peer()).is_err());
    }
    let mut duplicate = bytes.clone();
    attribute(&mut duplicate, 4, &3_u32.to_ne_bytes());
    let len = duplicate.len() as u32;
    duplicate[..4].copy_from_slice(&len.to_ne_bytes());
    assert!(parse(&duplicate, 88, peer()).is_err());
    let mut error = bytes;
    error[4..6].copy_from_slice(&2_u16.to_ne_bytes());
    assert!(parse(&error, 88, peer()).is_err());
}

#[test]
fn queries_bind_address_port_uid_and_ipv6_scope_without_mutating_routes() {
    let local: SocketAddr = "[fe80::1%3]:1200".parse().unwrap();
    let peer: SocketAddr = "[fe80::2%3]:1300".parse().unwrap();
    let bytes = request(Some(local), peer, 88, 501, Some(3));
    assert_eq!(u16_at(&bytes, 4).unwrap(), 26);
    assert_eq!(u16_at(&bytes, 6).unwrap(), 1);
    assert_eq!(bytes[16], 10);
    assert_eq!(bytes[17], 128);
    assert_eq!(bytes[18], 128);
    let mut attrs = std::collections::BTreeMap::new();
    let mut at = 28;
    while at < bytes.len() {
        let len = usize::from(u16_at(&bytes, at).unwrap());
        attrs.insert(
            u16_at(&bytes, at + 2).unwrap(),
            bytes[at + 4..at + len].to_vec(),
        );
        at += align4(len).unwrap();
    }
    assert_eq!(attrs[&4], 3_u32.to_ne_bytes());
    assert_eq!(attrs[&25], 501_u32.to_ne_bytes());
    assert_eq!(attrs[&27], [6]);
    assert_eq!(attrs[&28], 1200_u16.to_be_bytes());
    assert_eq!(attrs[&29], 1300_u16.to_be_bytes());
}

#[cfg(target_os = "linux")]
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
    assert!(!physical("lo").unwrap());
}
