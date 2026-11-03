// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether this Mac is on the home ALab LAN now, which a member's desk lets a Mac that proves
//! itself in by (`docs/DESIGN-automatic-lan-admission-2026-11-02.md`, "The home ALab LAN").
//!
//! An address does not make a network home: a phone hotspot, a café, a hotel or a travel router
//! numbers its hosts from RFC 1918 too, and a Mac listens and advertises on every network it is
//! on. A network is the home ALab LAN here only by evidence that cannot be made up without a
//! member: at least [`MEMBERS`] distinct fleet members proved their node keys to this Mac,
//! within [`FRESH`], within the current stay on this network and booked on this very network
//! when heard (so behind the gateway in use: a new gateway is a new network and a new stay, and
//! an answer heard behind another gateway counts for that one), from addresses on the very subnet
//! in question, through this Mac's current address on it and the same interface. Only the network
//! the IPv4 default route takes counts, and only over Ethernet or Wi-Fi (`en…`, not point to
//! point, no tunnel holding the default route): a VM or Internet Sharing bridge, a VPN or tunnel,
//! AirDrop's links and cellular never do, whatever answered there (member VMs on a lab bridge are
//! members too). A network in the Personal Hotspot range (`172.20.10.0/28`), one
//! Network.framework calls expensive or cellular (Wi-Fi to a phone's hotspot, Android's
//! included), or whose path could not be read, is never home. No evidence is no home: a Mac that
//! proves itself to this one then waits for a person's Allow.
//!
//! WHERE THIS MAC IS HAS ONE SOURCE, the network place (`aterm_netplace`): the route, gateway,
//! subnet, path and the members' node-proven answers are its facts and its evidence file, read
//! through [`aterm_netplace::proof`], so admission never reads another place than every other
//! reporter names. The place's own word is not enough here: it reads `alab-lan` for a network it
//! merely remembers (members answered there once), and only this stay's fresh answers admit.
//! This Mac's member desk reads it through [`home_net`], and marks a proof from another host on
//! that network as from the home ALab LAN (`crate::members::desk`).

use aterm_netplace::proof::Now;
use aterm_netplace::{Answer, Lane, Link, NetworkId, RouteFacts};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

/// How long a member's proven answer counts: past three heartbeat rounds (each every three
/// minutes, to up to three members), so a Mac on the home LAN keeps its evidence while one
/// round fails, and one that left it loses the evidence within minutes even if the next network
/// numbers its hosts alike.
pub const FRESH: Duration = Duration::from_secs(10 * 60);
/// How many distinct members must have answered on a network for it to be home: two Macs on one
/// hotspot or café network never make it so.
pub const MEMBERS: usize = 3;
/// How far past this Mac's clock an answer may be dated and still count (a clock step back).
const AHEAD_MS: u64 = 60_000;
/// Apple's Personal Hotspot hands out addresses from this network.
const HOTSPOT: (Ipv4Addr, u32) = (Ipv4Addr::new(172, 20, 10, 0), 28);

/// Why this Mac's network is not the home ALab LAN now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Away {
    /// The default route takes no Ethernet or Wi-Fi interface (a VPN, a tunnel, a bridge,
    /// cellular), a tunnel holds it, or there is none.
    Interface,
    /// The subnet is in the Personal Hotspot range.
    Hotspot,
    /// The network is expensive or cellular (Wi-Fi to a phone's hotspot) as Network.framework
    /// reads it, or the path could not be read or carries no traffic.
    Expensive,
    /// The default gateway's hardware address cannot be read now (no route, or ARP not
    /// resolved yet), so no evidence can be tied to this network.
    NoGateway,
    /// Fewer than [`MEMBERS`] members proved themselves on this network within [`FRESH`] in the
    /// current stay.
    TooFew {
        /// How many did.
        heard: usize,
    },
}

impl std::fmt::Display for Away {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Interface => f.write_str("this Mac's traffic leaves by no Ethernet or Wi-Fi"),
            Self::Hotspot => f.write_str("this Mac is on a phone hotspot"),
            Self::Expensive => {
                f.write_str("this Mac is on a cellular or hotspot network, or cannot tell")
            }
            Self::NoGateway => f.write_str("this network's router is not known yet"),
            Self::TooFew { heard } => write!(
                f,
                "{heard} of the {MEMBERS} members needed answered on this network in the last \
                 {} minutes",
                FRESH.as_secs() / 60
            ),
        }
    }
}

/// Whether an interface named `name` (point to point or not) can carry the home ALab LAN:
/// Ethernet or Wi-Fi (`en` and a number), not point to point. A VM or Internet Sharing bridge
/// (`bridge…`, `vmenet…`), a VPN or tunnel (`utun…`, `ipsec…`, `ppp…`, `gif…`, `stf…`), AirDrop's
/// `awdl…` and `llw…`, `anpi…` and cellular (`pdp_ip…`) never do.
fn home_kind(name: &str, point_to_point: bool) -> bool {
    name.strip_prefix("en")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        && !point_to_point
}

/// The default route's network where it can be the home ALab LAN: Ethernet or Wi-Fi
/// ([`home_kind`]), no tunnel holding the default route, and an address that is neither loopback
/// nor unspecified.
fn home_route(now: &Now) -> Option<&RouteFacts> {
    now.facts.route.as_ref().filter(|route| {
        now.facts.tunnel.is_none()
            && home_kind(&route.interface, now.point_to_point)
            && !route.address.is_loopback()
            && !route.address.is_unspecified()
    })
}

/// The mask of a prefix length.
fn mask_of(prefix: u8) -> Ipv4Addr {
    Ipv4Addr::from_bits(
        u32::MAX
            .checked_shl(32 - u32::from(prefix.min(32)))
            .unwrap_or(0),
    )
}

/// Whether `ip` is one of this Mac's own addresses, on any interface.
fn is_own(now: &Now, ip: Ipv4Addr) -> bool {
    now.own
        .iter()
        .any(|address| address.to_canonical() == IpAddr::V4(ip))
}

fn same(a: Ipv4Addr, b: Ipv4Addr, mask: Ipv4Addr) -> bool {
    a.to_bits() & mask.to_bits() == b.to_bits() & mask.to_bits()
}

fn network(ip: Ipv4Addr, mask: Ipv4Addr) -> Ipv4Addr {
    Ipv4Addr::from_bits(ip.to_bits() & mask.to_bits())
}

/// Whether `network`/`prefix` overlaps Apple's Personal Hotspot range.
fn hotspot(network: Ipv4Addr, prefix: u32) -> bool {
    let (range, range_prefix) = HOTSPOT;
    let shorter = prefix.min(range_prefix);
    let mask = u32::MAX.checked_shl(32 - shorter).unwrap_or(0);
    network.to_bits() & mask == range.to_bits() & mask
}

fn ipv4(ip: IpAddr) -> Option<Ipv4Addr> {
    match ip.to_canonical() {
        IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// The home-capable network ([`home_route`]) now (`now.at_ms`), when it is home: this Mac's
/// address and mask there.
fn home_at(now: &Now) -> Result<HomeNet, Away> {
    let Some(route) = home_route(now) else {
        return Err(Away::Interface);
    };
    let own = route.address;
    let mask = mask_of(route.prefix);
    let subnet = network(own, mask);
    let prefix = u32::from(route.prefix.min(32));
    if hotspot(subnet, prefix) {
        return Err(Away::Hotspot);
    }
    if !now.facts.path.is_some_and(|path| {
        path.usable == Some(true) && !path.expensive && path.link != Link::Cellular
    }) {
        return Err(Away::Expensive);
    }
    let Some(network) = NetworkId::of(route) else {
        return Err(Away::NoGateway);
    };
    // Only answers in the current stay on this very network count: a stay the record names on
    // another network (a lagging record, a lookalike behind another router) holds none. And each
    // answer must have been heard on this network itself: a stay continues across a network
    // whose gateway's hardware address went unread, so a lookalike left for the real home LAN
    // and back keeps its stay, and the answers heard on the home LAN meanwhile are booked there.
    // A member's answer counts only while no answer of it as new or newer was heard anywhere
    // else: a member last heard off this network is not here, whatever it said before.
    let Some(since) = now
        .stay_since_ms
        .filter(|_| now.stay_network.as_ref() == Some(&network))
    else {
        return Err(Away::TooFew { heard: 0 });
    };
    let at = now.at_ms;
    let max_ms = at.saturating_add(AHEAD_MS);
    let on_home = |answer: &Answer| {
        answer.network.as_ref() == Some(&network)
            && answer.via == Some(own)
            && answer.iface.as_deref() == Some(route.interface.as_str())
            && answer
                .addr
                .and_then(ipv4)
                .is_some_and(|addr| addr != own && same(addr, own, mask) && !is_own(now, addr))
    };
    let mut nodes: Vec<&str> = now
        .answers
        .iter()
        .filter(|answer| {
            answer.lane == Lane::Fleet
                && answer.logged
                && on_home(answer)
                && answer.began_ms >= since
                && answer.at_ms <= max_ms
                && at.saturating_sub(answer.at_ms) < FRESH.as_millis() as u64
                && !now.answers.iter().any(|other| {
                    other.lane == answer.lane
                        && other.member == answer.member
                        && other.at_ms <= max_ms
                        && other.at_ms >= answer.at_ms
                        && !on_home(other)
                })
        })
        .map(|answer| answer.member.as_str())
        .collect();
    nodes.sort_unstable();
    nodes.dedup();
    if nodes.len() >= MEMBERS {
        Ok(HomeNet { own, mask })
    } else {
        Err(Away::TooFew { heard: nodes.len() })
    }
}

/// This Mac's home ALab LAN, as its address and mask there ([`home_net_at`]): what a member's
/// desk judges a proving Mac's network by (`crate::members::desk`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HomeNet {
    own: Ipv4Addr,
    mask: Ipv4Addr,
}

impl HomeNet {
    /// Whether `peer` is another host on this network: an IPv4 (or IPv4-mapped) address on its
    /// subnet that is not this Mac's address there.
    #[must_use]
    pub fn holds(&self, peer: IpAddr) -> bool {
        ipv4(peer).is_some_and(|peer| peer != self.own && same(peer, self.own, self.mask))
    }

    /// The test home network: this Mac at `192.168.1.20/24`.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn test() -> Self {
        Self::at(
            Ipv4Addr::new(192, 168, 1, 20),
            Ipv4Addr::new(255, 255, 255, 0),
        )
    }

    /// A home network a test names: this Mac at `own` under `mask`.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn at(own: Ipv4Addr, mask: Ipv4Addr) -> Self {
        Self { own, mask }
    }
}

/// This Mac's home ALab LAN (`now`), as its address and mask there.
///
/// # Errors
/// [`Away`] names why the network the default route takes is not the home ALab LAN now.
pub fn home_net_at(now: &Now) -> Result<HomeNet, Away> {
    home_at(now)
}

/// This Mac's home ALab LAN now, from the network place under this process's state root
/// (`aterm_types::dirs::state_dir`, where its fleet heartbeat notes the answers,
/// [`aterm_netplace::proof::read`]); `None` where it is on none, or with no state root. It reads
/// the network (Network.framework's path among it), so never on a window's main thread.
#[must_use]
pub fn home_net() -> Option<HomeNet> {
    let state = aterm_types::dirs::state_dir()?;
    home_net_at(&aterm_netplace::proof::read(&state)).ok()
}

#[cfg(test)]
mod tests;
