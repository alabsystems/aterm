// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fail-closed physical-link policy, separate from cryptographic membership.
//! Route and interface facts come from the local OS. Private addresses alone
//! grant nothing. These observations cannot prove that a hostile LAN has not
//! forwarded or tunneled packets, nor eliminate changes between observations.
//! Applications still authenticate the peer and recheck before releasing data.

use std::{
    io,
    net::{IpAddr, Ipv6Addr, SocketAddr, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

mod listener;
mod multicast;
mod native;
pub use listener::Listener;
pub(crate) use listener::Witness as ListenerWitness;
pub use multicast::Multicast;

/// Loopback is an explicit local/test transport, never inferred as a LAN.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Lan,
    Loopback,
}

fn refuse(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, why)
}

const REQUIRED_FLAGS: u32 = (libc::IFF_UP | libc::IFF_RUNNING | libc::IFF_MULTICAST) as u32;
const FORBIDDEN_FLAGS: u32 = (libc::IFF_LOOPBACK | libc::IFF_POINTOPOINT) as u32;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Attachment {
    index: u32,
    name: String,
    address: IpAddr,
    netmask: IpAddr,
    flags: u32,
    physical: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Route {
    index: u32,
    // RTA_PREFSRC / RTA_IFA is a preference, not the connected socket's source.
    // inspect verifies the actual source independently against local addresses.
    source: Option<IpAddr>,
    direct: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Observation {
    local: SocketAddr,
    peer: SocketAddr,
    attachment: Option<Attachment>,
    route: Option<Route>,
}

/// A connected socket admitted for one explicit locality policy. The socket
/// and its observation travel together; this type grants no cryptographic role.
pub struct Connection {
    stream: TcpStream,
    proof: Proof,
}
impl Connection {
    /// The exact local endpoint selected and checked for this connection.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.stream.local_addr()
    }
    /// Interrupt this exact connection without exposing another reader/writer.
    pub fn cancellation(&self) -> io::Result<Cancellation> {
        let stream = self.stream.try_clone()?;
        aterm_uds::keep_out_of_children(&stream)?;
        Ok(Cancellation(stream))
    }
    /// Inspect an accepted or already connected socket without sending bytes.
    /// A production listener calls this before its TLS handshake.
    pub fn admit(stream: TcpStream, scope: Scope) -> io::Result<Self> {
        let until = native::deadline();
        if scope == Scope::Loopback {
            return Self::connected(stream, scope, None, until);
        }
        let local = stream.local_addr()?;
        let peer = stream.peer_addr()?;
        endpoints(Some(local), peer)?;
        let mut candidates = attachments(Some(local), peer, None)?;
        if candidates.len() != 1 {
            return Err(refuse("accepted source has no unique LAN attachment"));
        }
        let attachment = candidates.remove(0);
        if !ready(&attachment) {
            return Err(refuse("accepted source is not a live physical LAN"));
        }
        native::ensure_binding(&stream, attachment.index, &attachment.name)?;
        Self::connected(stream, scope, Some(attachment.index), until)
    }

    fn connected(
        stream: TcpStream,
        scope: Scope,
        bound: Option<u32>,
        until: Instant,
    ) -> io::Result<Self> {
        aterm_uds::keep_out_of_children(&stream)?;
        let local = stream.local_addr()?;
        let peer = stream.peer_addr()?;
        if scope == Scope::Lan {
            native::restrict_hops(&stream)?;
        }
        native::socket_policy(&stream, bound, scope == Scope::Lan)?;
        let observation = observe(local, peer, scope, bound, until)?;
        let witness = stream.try_clone()?;
        aterm_uds::keep_out_of_children(&witness)?;
        Ok(Self {
            stream,
            proof: Proof {
                witness,
                scope,
                bound,
                observation,
                withdrawn: AtomicBool::new(false),
                listener: None,
            },
        })
    }

    pub(crate) fn admitted_by(
        stream: TcpStream,
        scope: Scope,
        witness: ListenerWitness,
    ) -> io::Result<Self> {
        witness.recheck()?;
        let mut connection = Self::admit(stream, scope)?;
        connection.proof.listener = Some(witness);
        connection.proof.recheck()?;
        Ok(connection)
    }

    /// Check the selected route before dialing, then verify the actual socket.
    /// There is no DNS lookup, public fallback, relay, or route modification.
    pub fn connect(peer: SocketAddr, scope: Scope, within: Duration) -> io::Result<Self> {
        if within.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "zero LAN connection timeout",
            ));
        }
        let until = Instant::now()
            .checked_add(within)
            .ok_or_else(|| refuse("LAN connection timeout overflow"))?;
        match scope {
            Scope::Loopback if !unambiguous(peer) || !peer.ip().is_loopback() => {
                return Err(refuse("loopback transport requires a loopback destination"));
            }
            Scope::Lan => {
                let initial = inspect(None, peer, None, until)?;
                let attachment = initial
                    .attachment
                    .as_ref()
                    .ok_or_else(|| refuse("missing LAN attachment"))?;
                let dial = native::Dial::new(
                    initial.local,
                    peer,
                    attachment.index,
                    &attachment.name,
                    until,
                )?;
                // Binding reserves the source port before this exact tuple is
                // submitted to the kernel route lookup. No SYN has been sent.
                let local = dial.local_addr()?;
                inspect(Some(local), peer, Some(attachment.index), until)?;
                return Self::connected(dial.connect()?, scope, Some(attachment.index), until);
            }
            Scope::Loopback => {}
        }
        Self::connected(
            TcpStream::connect_timeout(&peer, within)?,
            scope,
            None,
            until,
        )
    }

    /// Transfer the socket to TLS and keep its bound observation for repeated
    /// checks. `Proof` owns a clone of this exact connection, not just an address.
    /// Drop both parts, or shut the socket down, when ending the connection.
    pub fn into_parts(self) -> (TcpStream, Proof) {
        (self.stream, self.proof)
    }
}

/// A runtime's handle for interrupting one owned network worker.
pub struct Cancellation(TcpStream);
impl Cancellation {
    pub fn cancel(&self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

/// Rechecks the exact connection and the admitted interface/route observation.
/// Any error permanently withdraws this proof; reconnect to obtain a new one.
pub struct Proof {
    witness: TcpStream,
    scope: Scope,
    bound: Option<u32>,
    observation: Observation,
    withdrawn: AtomicBool,
    listener: Option<ListenerWitness>,
}
impl Proof {
    pub fn recheck(&self) -> io::Result<()> {
        if self.withdrawn.load(Ordering::Acquire) {
            return Err(refuse("LAN observation was withdrawn; reconnect"));
        }
        let checked = (|| {
            native::socket_policy(&self.witness, self.bound, self.scope == Scope::Lan)?;
            let observed = observe(
                self.witness.local_addr()?,
                self.witness.peer_addr()?,
                self.scope,
                self.bound,
                native::deadline(),
            )?;
            if observed != self.observation {
                return Err(refuse("LAN route or attachment changed; reconnect"));
            }
            if let Some(listener) = &self.listener {
                listener.recheck()?;
            }
            Ok(())
        })();
        if checked.is_err() {
            self.withdrawn.store(true, Ordering::Release);
        } else if self.withdrawn.load(Ordering::Acquire) {
            return Err(refuse("LAN observation was withdrawn; reconnect"));
        }
        checked
    }
}

fn observe(
    local: SocketAddr,
    peer: SocketAddr,
    scope: Scope,
    bound: Option<u32>,
    until: Instant,
) -> io::Result<Observation> {
    native::within(until)?;
    if scope == Scope::Loopback {
        if !unambiguous(local)
            || !unambiguous(peer)
            || !local.ip().is_loopback()
            || !peer.ip().is_loopback()
            || local.is_ipv4() != peer.is_ipv4()
        {
            return Err(refuse(
                "local transport requires two unambiguous loopback endpoints",
            ));
        }
        return Ok(Observation {
            local,
            peer,
            attachment: None,
            route: None,
        });
    }
    inspect(Some(local), peer, bound, until)
}

fn endpoints(local: Option<SocketAddr>, peer: SocketAddr) -> io::Result<()> {
    if !unambiguous(peer)
        || !unicast(peer.ip())
        || peer.ip().is_loopback()
        || local.is_some_and(|local| {
            !unambiguous(local)
                || !unicast(local.ip())
                || local.ip().is_loopback()
                || local.is_ipv4() != peer.is_ipv4()
        })
    {
        return Err(refuse(
            "LAN endpoints must be unambiguous unicast addresses",
        ));
    }
    Ok(())
}

fn attachments(
    local: Option<SocketAddr>,
    peer: SocketAddr,
    selected: Option<u32>,
) -> io::Result<Vec<Attachment>> {
    Ok(local_attachments(local, selected)?
        .into_iter()
        .filter(|entry| same_subnet(entry.address, peer.ip(), entry.netmask))
        .collect())
}

fn local_attachments(
    local: Option<SocketAddr>,
    selected: Option<u32>,
) -> io::Result<Vec<Attachment>> {
    let mut candidates = Vec::new();
    for entry in aterm_uds::network::interfaces()? {
        // An unrelated interface can disappear during enumeration. If it was
        // the selected attachment, the candidate set remains empty and refuses.
        let Ok(index) = native::index(&entry.name) else {
            continue;
        };
        if selected.is_some_and(|selected| index != selected) {
            continue;
        }
        let address = scoped_ip(entry.address, index)?;
        if local.is_some_and(|local| address != local.ip()) {
            continue;
        }
        let physical = native::physical(&entry.name)?;
        // Preserve unknown masks as ineligible evidence. A listener must still
        // detect another interface using its address, even when that interface
        // has no usable prefix. Connected-route filtering rejects a zero mask.
        let netmask = entry.netmask.unwrap_or_else(|| match address {
            IpAddr::V4(_) => std::net::Ipv4Addr::UNSPECIFIED.into(),
            IpAddr::V6(_) => std::net::Ipv6Addr::UNSPECIFIED.into(),
        });
        candidates.push(Attachment {
            index,
            name: entry.name,
            address,
            netmask,
            flags: entry.flags & (REQUIRED_FLAGS | FORBIDDEN_FLAGS),
            physical,
        });
    }
    candidates.sort();
    candidates.dedup();
    Ok(candidates)
}

fn ready(attachment: &Attachment) -> bool {
    attachment.physical
        && attachment.flags & REQUIRED_FLAGS == REQUIRED_FLAGS
        && attachment.flags & FORBIDDEN_FLAGS == 0
}

fn inspect(
    local: Option<SocketAddr>,
    peer: SocketAddr,
    bound: Option<u32>,
    until: Instant,
) -> io::Result<Observation> {
    endpoints(local, peer)?;
    let mut route = native::route(local, peer, bound, until)?;
    if bound.is_some_and(|index| index != route.index) {
        return Err(refuse("route differs from the bound interface"));
    }
    let mut candidates = attachments(local, peer, Some(route.index))?;
    if local.is_none() {
        // Preflight needs one viable source on the selected attachment. The
        // kernel chooses the actual source at connect, and admit checks it.
        candidates.sort_by_key(|entry| (Some(entry.address) != route.source, entry.address));
        candidates.truncate(1);
    }
    if candidates.len() != 1 {
        return Err(refuse(
            "route source has no unique local interface attachment",
        ));
    }
    let attachment = candidates.remove(0);
    let local = local.unwrap_or_else(|| match (attachment.address, peer) {
        (IpAddr::V6(ip), SocketAddr::V6(peer)) => {
            SocketAddr::V6(std::net::SocketAddrV6::new(ip, 0, 0, peer.scope_id()))
        }
        (source, _) => SocketAddr::new(source, 0),
    });
    validate(local, peer, &attachment, &route)?;
    // The connected source was verified above. A changing kernel preference
    // (for example, a new IPv6 privacy address) does not alter this connection.
    route.source = None;
    native::within(until)?;
    Ok(Observation {
        local,
        peer,
        attachment: Some(attachment),
        route: Some(route),
    })
}

fn unambiguous(address: SocketAddr) -> bool {
    match address {
        SocketAddr::V4(_) => true,
        SocketAddr::V6(address) => {
            address.ip().to_ipv4_mapped().is_none()
                && address.flowinfo() == 0
                && if address.ip().is_unicast_link_local() {
                    address.scope_id() != 0
                        && address.ip().octets()[..8] == [0xfe, 0x80, 0, 0, 0, 0, 0, 0]
                } else {
                    address.scope_id() == 0
                }
        }
    }
}
fn unicast(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.octets()[0] > 0 && address.octets()[0] < 224,
        IpAddr::V6(address) => !address.is_unspecified() && !address.is_multicast(),
    }
}
fn scoped_ip(address: IpAddr, index: u32) -> io::Result<IpAddr> {
    let IpAddr::V6(address) = address else {
        return Ok(address);
    };
    let mut octets = address.octets();
    // Darwin's interface enumeration can carry the link-local zone in bytes
    // 2..4. It must match the separately observed interface index.
    if address.is_unicast_link_local() && (octets[2] != 0 || octets[3] != 0) {
        if u32::from(u16::from_be_bytes([octets[2], octets[3]])) != index {
            return Err(refuse("embedded IPv6 zone differs from the interface"));
        }
        octets[2] = 0;
        octets[3] = 0;
    }
    Ok(Ipv6Addr::from(octets).into())
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NetLanBoundary",
        action = "Physical",
        project = "aterm_net::lan::tests::lan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NetLanBoundary",
        action = "Routed",
        project = "aterm_net::lan::tests::lan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NetLanBoundary",
        action = "Virtual",
        project = "aterm_net::lan::tests::lan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NetLanBoundary",
        action = "WrongInterface",
        project = "aterm_net::lan::tests::lan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NetLanBoundary",
        action = "WrongSource",
        project = "aterm_net::lan::tests::lan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NetLanBoundary",
        action = "OffLink",
        project = "aterm_net::lan::tests::lan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NetLanBoundary",
        action = "Down",
        project = "aterm_net::lan::tests::lan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NetLanBoundary",
        action = "MissingScope",
        project = "aterm_net::lan::tests::lan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NetLanBoundary",
        action = "MappedAddress",
        project = "aterm_net::lan::tests::lan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NetLanBoundary",
        action = "ScopedIPv6",
        project = "aterm_net::lan::tests::lan_transition"
    )
)]
fn validate(
    local: SocketAddr,
    peer: SocketAddr,
    attachment: &Attachment,
    route: &Route,
) -> io::Result<()> {
    if !route.direct
        || route.index == 0
        || route.index != attachment.index
        || !ready(attachment)
        || local.ip() != attachment.address
        || !unambiguous(local)
        || !unambiguous(peer)
        || !unicast(local.ip())
        || !unicast(peer.ip())
        || local.ip() == peer.ip()
        || local.ip().is_loopback()
        || peer.ip().is_loopback()
        || !same_subnet(local.ip(), peer.ip(), attachment.netmask)
    {
        return Err(refuse(
            "peer is not on the selected live physical LAN attachment",
        ));
    }
    for address in [local, peer] {
        if let SocketAddr::V6(address) = address
            && address.scope_id() != 0
            && address.scope_id() != attachment.index
        {
            return Err(refuse("IPv6 zone must identify the admitted interface"));
        }
    }
    Ok(())
}

fn same_subnet(local: IpAddr, peer: IpAddr, mask: IpAddr) -> bool {
    match (local, peer, mask) {
        (IpAddr::V4(local), IpAddr::V4(peer), IpAddr::V4(mask)) => {
            let mask = u32::from(mask);
            if mask == 0 || (!mask).checked_add(1).is_none_or(|n| n & !mask != 0) {
                return false;
            }
            let local = u32::from(local);
            let peer = u32::from(peer);
            let source_host = local & !mask;
            let host = peer & !mask;
            local & mask == peer & mask
                && (mask.count_ones() >= 31
                    || (source_host != 0 && source_host != !mask && host != 0 && host != !mask))
        }
        (IpAddr::V6(local), IpAddr::V6(peer), IpAddr::V6(mask)) => {
            let mask = u128::from(mask);
            mask != 0
                && (!mask).checked_add(1).is_some_and(|n| n & !mask == 0)
                && u128::from(local) & mask == u128::from(peer) & mask
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests;
