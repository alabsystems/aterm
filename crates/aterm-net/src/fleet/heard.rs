// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE NOTING CALL (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.2 M2, §11 B6): every
//! [`ALPN_MESH`](super::ALPN_MESH) handshake, at either end ([`super::NodeIdentity::noting`]),
//! and every re-proof of a neighbour link ([`Heard::noting_then`], [`super::link`]) notes a
//! node-proven answer into the heard ledger of the network place under this Mac's state root
//! ([`aterm_netplace::heard`]), and books the day it was heard on, so this Mac's home evidence and
//! its home days need no heartbeat: a quiet link re-proves every five minutes, inside the
//! evidence's freshness.
//!
//! An answer is booked on a network ([`Answer::network`]) only where it came over the default
//! route: this end's address on the connection is this Mac's address on that route, through its
//! interface, and the peer's lies on the route's subnet. Only such an answer goes to the
//! admission ledger, and books its day. A loopback, tunnel or second-interface connection, or one
//! whose route moved, is noted for presentation alone
//! ([`aterm_netplace::heard::note_observations`]): it adds nothing the vault's automatic admission
//! (`aterm_secrets::home_lan`) counts, and it never stands in for the member's answer over the
//! route, as the newest answer per member would. Every answer the mesh proves is logged
//! ([`Answer::logged`]): [`ALPN_MESH`](super::ALPN_MESH) admits only a key this Mac's membership
//! log seats a member under, at either end. It is keyed by that key's principal, the node id the
//! mesh names a node by ([`crate::fleet_member::node_principal`]), never by the id the peer
//! claims, which the mesh does not bind to its key: one Mac is one member, however it names
//! itself.
//!
//! WHERE THE NOTING CALL READS THIS MAC is its [`Sight`]: the clock, the default route and the
//! interface holding an address, the node service's the Mac's own ([`LiveSight`]). A test hands
//! one that scripts them, and the place its loopback connections' ends stand for, never the
//! answers: each answer is a real proof.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aterm_netplace::{Answer, Lane, NetworkId, RouteFacts, member_key};

use super::NodeProof;
use super::link::{NoteProven, Proven, ProvenBy};

/// What the noting call reads of this Mac when it notes an answer ([the module](self)).
pub trait Sight: Send + Sync {
    /// The time now (unix ms).
    fn now_ms(&self) -> u64;

    /// The IPv4 default route now; `None` with none.
    fn route(&self) -> Option<RouteFacts>;

    /// The interface that holds this Mac's address `local` now (`en0`); `None` where none does.
    fn interface(&self, local: Ipv4Addr) -> Option<String>;

    /// Where a connection's two ends stand on the network: the peer's address and this end's.
    /// This Mac's are its socket's own.
    fn ends(&self, peer: SocketAddr, local: SocketAddr) -> (IpAddr, IpAddr) {
        (peer.ip(), local.ip())
    }
}

/// This Mac's own [`Sight`]: the system clock, and the kernel's routes and interfaces as the
/// network place reads them ([`aterm_netplace::facts`]), with no packet.
#[derive(Clone, Copy, Debug, Default)]
pub struct LiveSight;

impl Sight for LiveSight {
    fn now_ms(&self) -> u64 {
        aterm_netplace::unix_ms_now()
    }

    fn route(&self) -> Option<RouteFacts> {
        aterm_netplace::facts::routes_now().0
    }

    fn interface(&self, local: Ipv4Addr) -> Option<String> {
        aterm_netplace::facts::interface_holding(local)
    }
}

/// The noting call ([the module](self)): where a node notes each peer the mesh proved.
#[derive(Clone)]
pub struct Heard {
    /// The network place's directory ([`aterm_netplace::record::dir`]).
    dir: PathBuf,
    sight: Arc<dyn Sight>,
}

impl std::fmt::Debug for Heard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Heard")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl Heard {
    /// Noting into the network place under the state root `state_dir`, reading this Mac through
    /// `sight`.
    #[must_use]
    pub fn under(state_dir: &Path, sight: Arc<dyn Sight>) -> Self {
        Self {
            dir: aterm_netplace::record::dir(state_dir),
            sight,
        }
    }

    /// Noting into the network place under the state root `state_dir`, reading this Mac itself
    /// ([`LiveSight`]).
    #[must_use]
    pub fn live(state_dir: &Path) -> Self {
        Self::under(state_dir, Arc::new(LiveSight))
    }

    /// The time now on this call's clock (unix ms): when a handshake it will note began.
    #[must_use]
    pub fn now_ms(&self) -> u64 {
        self.sight.now_ms()
    }

    /// NOTES ONE NODE-PROVEN ANSWER: `peer` proved its key, as a member's, on the connection from
    /// `addr` to this end's `local`, in an exchange begun at `began_ms` ([`Self::now_ms`]) and
    /// proven now. An answer that came over the default route goes into the heard ledger
    /// ([`aterm_netplace::heard::note_answers`], coalesced per member) and its day into the
    /// home-day ledger ([`aterm_netplace::heard::note_days`]); any other is noted for presentation
    /// alone ([the module](self)). Call it only for a peer whose key the membership log seats a
    /// member under.
    pub fn note(&self, peer: &NodeProof, (addr, local): (SocketAddr, SocketAddr), began_ms: u64) {
        let at_ms = self.sight.now_ms();
        let (addr, local) = self.sight.ends(addr, local);
        let via = ipv4(local);
        let answer = answer_of(
            peer,
            (addr, via),
            (
                self.sight.route().as_ref(),
                via.and_then(|via| self.sight.interface(via)),
            ),
            (began_ms.min(at_ms), at_ms),
        );
        if answer.network.is_some() {
            aterm_netplace::heard::note_answers(&self.dir, vec![answer.clone()]);
            aterm_netplace::heard::note_days(&self.dir, &[answer]);
        } else {
            aterm_netplace::heard::note_observations(&self.dir, vec![answer]);
        }
    }

    /// Notes a neighbour link's proof ([`Proven`]), proven now: a re-proof's exchange began within
    /// its answer's minute ([`super::link::REPROVE_WITHIN`]), so it is noted as begun when it was
    /// proven.
    pub fn proven(&self, proven: &Proven) {
        self.note(&proven.peer, (proven.addr, proven.local), self.now_ms());
    }

    /// This call as a neighbour link's [`NoteProven`], each proof then handed to `then`: it notes
    /// each re-proof ([`ProvenBy::Reproof`]) on a thread of its own, since a link hands its proofs
    /// on the read loop every stream of the link waits on, and hands it to `then` once noted. The
    /// handshake that opened the link was noted where it completed, by the node identity of the
    /// end that dialed or took it ([`super::NodeIdentity::noting`]), so each proof is noted once;
    /// it goes to `then` at once.
    #[must_use]
    pub fn noting_then(&self, then: NoteProven) -> NoteProven {
        let heard = self.clone();
        Arc::new(move |proven: &Proven| {
            if proven.by != ProvenBy::Reproof {
                then(proven);
                return;
            }
            let task = {
                let (heard, then, proven) = (heard.clone(), Arc::clone(&then), proven.clone());
                move || {
                    heard.proven(&proven);
                    then(&proven);
                }
            };
            let spawned = std::thread::Builder::new()
                .name("aterm-mesh-heard".to_owned())
                .spawn(task);
            // With no thread to spare, the link's own thread notes it.
            if spawned.is_err() {
                heard.proven(proven);
                then(proven);
            }
        })
    }
}

/// A [`Sight`] a test scripts: its clock, the default route (whose interface holds the route's
/// address, and nothing else), and where every connection's ends stand. Loopback connections
/// stand for a LAN's this way: the route facts are scripted, never the answers.
#[cfg(any(test, debug_assertions))]
#[derive(Clone)]
pub struct ScriptedSight {
    /// The time now (unix ms).
    pub now: Arc<dyn Fn() -> u64 + Send + Sync>,
    /// The IPv4 default route.
    pub route: Option<RouteFacts>,
    /// Where every connection's two ends stand, the peer's and this end's; `None` leaves each
    /// socket's own.
    pub ends: Option<(IpAddr, IpAddr)>,
}

#[cfg(any(test, debug_assertions))]
impl Sight for ScriptedSight {
    fn now_ms(&self) -> u64 {
        (self.now)()
    }

    fn route(&self) -> Option<RouteFacts> {
        self.route.clone()
    }

    fn interface(&self, local: Ipv4Addr) -> Option<String> {
        self.route
            .as_ref()
            .filter(|route| route.address == local)
            .map(|route| route.interface.clone())
    }

    fn ends(&self, peer: SocketAddr, local: SocketAddr) -> (IpAddr, IpAddr) {
        self.ends.unwrap_or((peer.ip(), local.ip()))
    }
}

/// `ip` as an IPv4 address (an IPv4-mapped one too); `None` for IPv6.
fn ipv4(ip: IpAddr) -> Option<Ipv4Addr> {
    match ip.to_canonical() {
        IpAddr::V4(v4) => Some(v4),
        IpAddr::V6(_) => None,
    }
}

/// THE ANSWER A PROOF IS NOTED AS: `peer`'s, keyed by its key's principal, lane fleet, logged,
/// from `addr` through this Mac's address `via` (held by `iface`), begun and answered at
/// `began_ms` and `at_ms`, booked on the network of `route` only where it came over that route:
/// `via` is the route's address, `iface` its interface, and `addr` another host on its subnet
/// ([the module](self)).
#[must_use]
pub fn answer_of(
    peer: &NodeProof,
    (addr, via): (IpAddr, Option<Ipv4Addr>),
    (route, iface): (Option<&RouteFacts>, Option<String>),
    (began_ms, at_ms): (u64, u64),
) -> Answer {
    let addr = addr.to_canonical();
    let network = route
        .filter(|route| {
            via == Some(route.address)
                && iface.as_deref() == Some(route.interface.as_str())
                && addr != IpAddr::V4(route.address)
                && route.on_link(addr)
        })
        .and_then(NetworkId::of);
    Answer {
        lane: Lane::Fleet,
        member: member_key(Lane::Fleet, &crate::fleet_member::node_principal(peer.key)),
        addr: Some(addr),
        via,
        iface,
        network,
        logged: true,
        began_ms,
        at_ms,
    }
}

#[cfg(test)]
#[path = "heard_tests.rs"]
mod tests;
