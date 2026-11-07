// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Places for the reporters' tests in other crates (`#[cfg(any(test, debug_assertions))]`, the
//! workspace's test-seam rule): synthetic addresses, no reading of the Mac.

use std::net::Ipv4Addr;

use crate::place::{
    Alab, ComputedBy, Internet, InternetSource, InternetState, Kind, Link, NetworkId, PathFacts,
    Place, Recognised, RouteFacts, Where,
};

fn route(address: [u8; 4], prefix: u8, gateway: [u8; 4]) -> RouteFacts {
    RouteFacts {
        interface: "en0".to_owned(),
        address: Ipv4Addr::from(address),
        prefix,
        gateway: Some(Ipv4Addr::from(gateway)),
        gateway_hw: Some(vec![2, 0, 0, 0, 0, 1]),
    }
}

/// On a phone's Personal Hotspot since `since_ms`, away from the ALab LAN since `away_ms`, the
/// internet answering the path monitor only, no member heard of the 41 the book knows.
#[must_use]
pub fn hotspot(since_ms: u64, away_ms: u64) -> Place {
    let route = route([172, 20, 10, 2], 28, [172, 20, 10, 1]);
    Place {
        at: Where::Remote(Kind::Hotspot { by_range: false }),
        kind: Some(Kind::Hotspot { by_range: false }),
        recognised: None,
        once_alab: false,
        network: NetworkId::of(&route),
        route: Some(route),
        tunnel: None,
        path: Some(PathFacts {
            usable: Some(true),
            expensive: true,
            constrained: false,
            link: Link::Wifi,
        }),
        since_ms: Some(since_ms),
        since_before: false,
        internet: Internet {
            state: InternetState::PathUp,
            source: InternetSource::Path,
            at_ms: None,
            endpoint: None,
        },
        alab: Alab::Away {
            since_ms: Some(away_ms),
        },
        heard: 0,
        known: Some(41),
        last_answer: None,
        computed_by: ComputedBy::Keeper,
    }
}

/// On the ALab LAN since `since_ms`, recognised by the members that answered there, `heard` of
/// the 41 the book knows answering in this stay.
#[must_use]
pub fn home(since_ms: u64, heard: usize) -> Place {
    let route = route([192, 168, 4, 31], 22, [192, 168, 4, 1]);
    Place {
        at: Where::AlabLan,
        kind: Some(Kind::Lan),
        recognised: Some(Recognised::Members),
        network: NetworkId::of(&route),
        route: Some(route),
        path: Some(PathFacts {
            usable: Some(true),
            expensive: false,
            constrained: false,
            link: Link::Wifi,
        }),
        alab: Alab::Here,
        heard,
        ..hotspot(since_ms, since_ms)
    }
}
