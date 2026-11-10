// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A LAB VAULT'S HOME LAN (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §10, step E1b): the seam a
//! lab drives the home-LAN rule by. A member of the live ALab vault on its home ALab LAN lets a
//! Mac that proves itself in with nobody's Allow ([`super::desk::Local::automatically_admits`]),
//! the home known only by evidence no single Mac can make up (`crate::home_lan`). A lab's members
//! hear too few members for that evidence and are never on the live vault, so in a debug build a
//! lab vault's own state names its home LAN instead: [`FILE`] in the profile's vault folder
//! ([`Paths::dir`]), one IPv4 CIDR line, private to its owner, which only the lab tool writes
//! ([`write`]; `targo --unverified lab commons-lan home-lan`).
//!
//! [`home`] answers a home only where every gate holds: the profile follows a vault ([`Config`])
//! that is not the live ALab vault ([`client::alab_vault`]), served at a URL (a vault read from a
//! path cannot hand a new Mac the log bundle it joins by), whose records do not pin the live
//! ALab genesis (`live_genesis`, [`client::on_live_genesis`]); the profile's identity carries the
//! lab mark ([`client::lab_mark`], A12); [`FILE`] holds a CIDR [`Cidr::parse`] takes; and this
//! Mac's IPv4 default route, as the vault's own proof reader reads it
//! ([`aterm_netplace::facts::here_now`], which no seam file scripts), takes an Ethernet or Wi-Fi
//! interface (`en` and a number, not point to point, no tunnel holding the default route) at an
//! address inside the CIDR. The home is then this Mac at that address under the CIDR's mask
//! ([`HomeNet::at`]), so a proof from another host inside the CIDR is from the home LAN, and one
//! from this Mac's own address, from loopback or from outside the CIDR is not.
//!
//! A release build compiles none of this module (`members.rs` declares it under
//! `cfg(any(test, debug_assertions))`, and `desk::Local::of` reads it only there), so a file
//! planted in a release profile is inert.

use crate::client::{self, Config, Paths};
use crate::home_lan::HomeNet;
use crate::transport::Source;
use crate::{Error, Result};
use aterm_netplace::facts::Here;
use std::net::Ipv4Addr;

/// The file in a lab profile's vault folder ([`Paths::dir`]) that names its home LAN.
pub const FILE: &str = "lab-home-lan";

/// The most bytes [`FILE`] may hold: one CIDR line (`255.255.255.255/30` and a newline is 19).
const MAX_BYTES: usize = 64;

/// The shortest prefix a lab home takes: a /16 holds every host a lab LAN numbers.
const WIDEST: u8 = 16;

/// The longest prefix a lab home takes: a /30 still holds two hosts.
const NARROWEST: u8 = 30;

/// Apple's Personal Hotspot range (`crate::home_lan`'s, which is never home either).
const HOTSPOT: (Ipv4Addr, u8) = (Ipv4Addr::new(172, 20, 10, 0), 28);

/// An IPv4 network a lab names its home LAN by: its network address and prefix length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    net: Ipv4Addr,
    prefix: u8,
}

/// The mask of a prefix length of at most 32.
fn mask_of(prefix: u8) -> Ipv4Addr {
    Ipv4Addr::from_bits(
        u32::MAX
            .checked_shl(32 - u32::from(prefix.min(32)))
            .unwrap_or(0),
    )
}

impl Cidr {
    /// `text` as a lab home: one `a.b.c.d/n` (surrounding whitespace and one trailing newline
    /// allowed), an RFC 1918 network (`10/8`, `172.16/12`, `192.168/16`) with a prefix from /16
    /// to /30 and no host bits set, that overlaps no Personal Hotspot range (`172.20.10.0/28`).
    ///
    /// # Errors
    /// Anything else: IPv6, public or loopback space, a prefix out of range, host bits, a
    /// hotspot overlap, or text that is no CIDR.
    pub fn parse(text: &str) -> Result<Self> {
        let refuse = |why: &str| Error::new(format!("a lab home LAN is {why}"));
        if text.len() > MAX_BYTES {
            return Err(refuse("one short CIDR line"));
        }
        let line = text.trim();
        if line.is_empty() || line.contains(char::is_whitespace) {
            return Err(refuse("one CIDR, `a.b.c.d/n`, alone on its line"));
        }
        let (net, prefix) = line
            .split_once('/')
            .ok_or_else(|| refuse("an IPv4 network and its prefix, `a.b.c.d/n`"))?;
        let net: Ipv4Addr = net
            .parse()
            .map_err(|_| refuse("an IPv4 network, `a.b.c.d/n`, never IPv6 or a name"))?;
        let prefix: u8 = Some(prefix)
            .filter(|p| !p.is_empty() && p.len() <= 2 && p.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|p| p.parse().ok())
            .ok_or_else(|| refuse("an IPv4 network and a decimal prefix"))?;
        if !(WIDEST..=NARROWEST).contains(&prefix) {
            return Err(refuse("a /16 to /30 network"));
        }
        if net.is_loopback() || !net.is_private() {
            return Err(refuse(
                "private (10/8, 172.16/12 or 192.168/16), never loopback or public space",
            ));
        }
        if net.to_bits() & mask_of(prefix).to_bits() != net.to_bits() {
            return Err(refuse(
                "named by its network address, with no host bits set",
            ));
        }
        let cidr = Self { net, prefix };
        if cidr.overlaps(HOTSPOT.0, HOTSPOT.1) {
            return Err(refuse("never a phone hotspot's range (172.20.10.0/28)"));
        }
        Ok(cidr)
    }

    /// The network's mask.
    #[must_use]
    pub fn mask(&self) -> Ipv4Addr {
        mask_of(self.prefix)
    }

    /// Whether `ip` lies inside the network.
    #[must_use]
    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        ip.to_bits() & self.mask().to_bits() == self.net.to_bits()
    }

    /// Whether this network and `net`/`prefix` share an address.
    fn overlaps(&self, net: Ipv4Addr, prefix: u8) -> bool {
        let shorter = mask_of(self.prefix.min(prefix)).to_bits();
        self.net.to_bits() & shorter == net.to_bits() & shorter
    }
}

impl std::fmt::Display for Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.net, self.prefix)
    }
}

/// The home LAN `paths`'s [`FILE`] names: `None` with no file.
///
/// # Errors
/// The file is not this user's private regular file, holds more than a short line, is not
/// UTF-8, or names no network [`Cidr::parse`] takes.
pub fn read(paths: &Paths) -> Result<Option<Cidr>> {
    let Some(bytes) = crate::fsx::read_private(&paths.dir.join(FILE), MAX_BYTES)? else {
        return Ok(None);
    };
    let text = bytes
        .as_str()
        .ok_or_else(|| Error::new("the lab home LAN file is not text"))?;
    Cidr::parse(text).map(Some)
}

/// Name `cidr` as the home LAN of the lab profile at `paths` ([`FILE`], mode 0600, written whole).
///
/// # Errors
/// The profile's vault folder is missing, or the write failed.
pub fn write(paths: &Paths, cidr: &Cidr) -> Result<()> {
    crate::fsx::write_atomic(&paths.dir.join(FILE), format!("{cidr}\n").as_bytes(), 0o600)
}

/// This lab member's home LAN, by the gates in [the module](self): `config` is the vault the
/// profile at `paths` follows, `live_genesis` whether its records pin the live ALab genesis. Read
/// on the desk's lane or a connection thread, never a window's main thread: this Mac's route comes
/// from [`aterm_netplace::facts::here_now`], which reads the path.
///
/// # Errors
/// [`FILE`] or the profile's identity cannot be read, or [`FILE`] holds no CIDR [`Cidr::parse`]
/// takes: no home either, but one a reader that keeps what it read must not keep
/// (`desk::Local::whole`).
pub fn home(paths: &Paths, config: Option<&Config>, live_genesis: bool) -> Result<Option<HomeNet>> {
    let Some(config) = config else {
        return Ok(None);
    };
    if live_genesis || client::alab_vault(config) || !matches!(config.source, Source::Url(_)) {
        return Ok(None);
    }
    let Some(cidr) = read(paths)? else {
        return Ok(None);
    };
    if client::lab_mark(paths)?.is_none() {
        return Ok(None);
    }
    Ok(home_in(&cidr, &aterm_netplace::facts::here_now()))
}

/// This Mac's home under `cidr` where `here` puts its default route ([`route_in`]).
fn home_in(cidr: &Cidr, here: &Here) -> Option<HomeNet> {
    route_in(cidr, here).map(|own| HomeNet::at(own, cidr.mask()))
}

/// This Mac's address on the home `cidr` names, where `here` puts its default route on an
/// Ethernet or Wi-Fi interface (`en` and a number, not point to point) with no tunnel holding the
/// default route, at an address inside `cidr`. That address is never loopback or unspecified: a
/// [`Cidr`] is RFC 1918 space ([`Cidr::parse`]), which holds neither.
fn route_in(cidr: &Cidr, here: &Here) -> Option<Ipv4Addr> {
    let route = here.facts.route.as_ref()?;
    let wired_or_wifi = route
        .interface
        .strip_prefix("en")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    let on = here.facts.tunnel.is_none()
        && wired_or_wifi
        && !here.point_to_point
        && cidr.contains(route.address);
    on.then_some(route.address)
}

/// This Mac's address on the home `cidr` names now ([`route_in`] of
/// [`aterm_netplace::facts::here_now`]): the lab tool's check, before it writes [`FILE`], that the
/// home it names holds this Mac's default route. Never on a window's main thread.
#[must_use]
pub fn here_in(cidr: &Cidr) -> Option<Ipv4Addr> {
    route_in(cidr, &aterm_netplace::facts::here_now())
}

#[cfg(test)]
#[path = "lab_lan_tests.rs"]
mod tests;
