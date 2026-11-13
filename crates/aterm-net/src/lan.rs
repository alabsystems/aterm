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
pub use multicast::{Heard, Multicast};

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
    /// The peer endpoint of this exact connected socket, including an accepted one.
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.stream.peer_addr()
    }
    /// Interrupt this exact connection without exposing another reader/writer.
    pub fn cancellation(&self) -> io::Result<Cancellation> {
        let stream = aterm_uds::kept_out(|| self.stream.try_clone())?;
        Ok(Cancellation(stream))
    }
    /// Inspect an accepted or already connected socket without sending bytes.
    /// A production listener calls this before its TLS handshake.
    pub fn admit(stream: TcpStream, scope: Scope) -> io::Result<Self> {
        let proof = Self::admit_proof(&stream, scope, None)?;
        Ok(Self { stream, proof })
    }

    pub(crate) fn admit_proof(
        stream: &TcpStream,
        scope: Scope,
        listener: Option<ListenerWitness>,
    ) -> io::Result<Proof> {
        if let Some(witness) = &listener {
            witness.recheck()?;
        }
        let until = native::deadline();
        let bound = if scope == Scope::Loopback {
            None
        } else {
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
            native::ensure_binding(stream, attachment.index, &attachment.name)?;
            Some(attachment.index)
        };
        let has_listener = listener.is_some();
        let proof = Self::connected_proof(stream, scope, bound, until, listener)?;
        if has_listener {
            proof.recheck()?;
        }
        Ok(proof)
    }

    fn connected(
        stream: TcpStream,
        scope: Scope,
        bound: Option<u32>,
        until: Instant,
    ) -> io::Result<Self> {
        let proof = Self::connected_proof(&stream, scope, bound, until, None)?;
        Ok(Self { stream, proof })
    }

    fn connected_proof(
        stream: &TcpStream,
        scope: Scope,
        bound: Option<u32>,
        until: Instant,
        listener: Option<ListenerWitness>,
    ) -> io::Result<Proof> {
        aterm_uds::keep_out_of_children(stream)?;
        let local = stream.local_addr()?;
        let peer = stream.peer_addr()?;
        if scope == Scope::Lan {
            native::restrict_hops(stream)?;
        }
        native::socket_policy(stream, bound, scope == Scope::Lan)?;
        let observation = observe(local, peer, scope, bound, until)?;
        let witness = aterm_uds::kept_out(|| stream.try_clone())?;
        Ok(Proof {
            witness,
            scope,
            bound,
            observation,
            withdrawn: AtomicBool::new(false),
            listener,
        })
    }

    pub(crate) fn admitted_by(
        stream: TcpStream,
        scope: Scope,
        witness: ListenerWitness,
    ) -> io::Result<Self> {
        let proof = Self::admit_proof(&stream, scope, Some(witness))?;
        Ok(Self { stream, proof })
    }

    /// Check the selected route before dialing, then verify the actual socket.
    /// There is no DNS lookup, public fallback, relay, or route modification.
    ///
    /// A LAN dial is made by `aterm_uds::lan::connect`: on macOS the root LAN broker makes the
    /// socket, bound to the checked attachment and source with a one-hop limit, connects it and
    /// passes it here (macOS's Local Network privacy gates the act of a process of the person's,
    /// never of root's); elsewhere it is made here with the same options. Before the handshake
    /// only the selected route and those options constrain it: the exact tuple (the source port
    /// the connect chose) and the socket's policy are checked against its route and attachment
    /// once it is connected, before any payload byte is sent, and a mismatch drops the
    /// connection.
    ///
    /// # Errors
    /// A refused route, attachment or socket policy (`PermissionDenied`), an expired `within`
    /// (`TimedOut`), the connect's error, or on macOS a missing LAN broker (`NotConnected`, its
    /// words naming what installs it).
    pub fn connect(peer: SocketAddr, scope: Scope, within: Duration) -> io::Result<Self> {
        crate::loopback::check(crate::loopback::Reach::Dial(peer))?;
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
                let index = attachment.index;
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "LAN connection deadline expired before dialing",
                    ));
                }
                let options = aterm_uds::lan::ConnectOptions {
                    bound_if: Some(index),
                    // A link-local source is named by its zone, which an address alone cannot
                    // carry: the interface binding keeps the kernel's choice on the attachment,
                    // and the connected tuple's check below refuses any other source.
                    source: match initial.local {
                        SocketAddr::V6(local) if local.ip().is_unicast_link_local() => None,
                        local => Some(local.ip()),
                    },
                    hop_limit: Some(1),
                };
                let (stream, reached) = aterm_uds::lan::connect(
                    &aterm_uds::lan::Target::Addr(peer),
                    peer.port(),
                    left,
                    options,
                )
                .map_err(aterm_uds::lan::LanError::into_io)?;
                if reached != peer || stream.peer_addr()? != peer {
                    return Err(refuse("LAN connection reached another address"));
                }
                return Self::connected(stream, scope, Some(index), until);
            }
            Scope::Loopback => {}
        }
        Self::connected(aterm_uds::connect_tcp(&peer, within)?, scope, None, until)
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
        self.check(false)
    }

    /// Finish an exchange and consume its proof. A TCP reset may have removed
    /// the socket's peer tuple after the final authenticated bytes arrived. In
    /// that case, revalidate the recorded tuple's physical route and attachment;
    /// this terminal check cannot authorize any further I/O.
    pub fn finish(self) -> io::Result<()> {
        self.check(true)
    }

    fn check(&self, completed: bool) -> io::Result<()> {
        if self.withdrawn.load(Ordering::Acquire) {
            return Err(refuse("LAN observation was withdrawn; reconnect"));
        }
        let checked = (|| {
            native::socket_policy(&self.witness, self.bound, self.scope == Scope::Lan)?;
            let (local, peer) = match self.witness.peer_addr() {
                Ok(peer) => (self.witness.local_addr()?, peer),
                Err(error)
                    if completed
                        && (error.kind() == io::ErrorKind::NotConnected
                            || (cfg!(target_os = "macos")
                                && error.raw_os_error() == Some(libc::EINVAL))) =>
                {
                    // XNU reports EINVAL for getpeername after a reset; Linux
                    // reports ENOTCONN. Only the consuming finalizer uses this.
                    (self.observation.local, self.observation.peer)
                }
                Err(error) => return Err(error),
            };
            let observed = observe(local, peer, self.scope, self.bound, native::deadline())?;
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

/// This machine's attachments as [`local_attachments_from`] reads them, kept while the kernel's
/// interface generation ([`native::interface_generation`]) stands still. Every listener and
/// multicast recheck (each link's, every 100 ms tick of the Commons runtime, before and after
/// each receive) asked the OS for every interface again, a `getifaddrs` with an index and a link
/// type lookup each, to find its attachment unchanged: about 4% of a core on an idle Mac
/// (2026-11-06).
fn local_attachments(
    local: Option<SocketAddr>,
    selected: Option<u32>,
) -> io::Result<Vec<Attachment>> {
    kept_attachments(|memo| {
        memo.read(
            native::interface_generation(),
            attachments_clock(),
            local,
            selected,
            || {
                #[cfg(test)]
                ENUMERATIONS.set(ENUMERATIONS.get() + 1);
                local_attachments_from(
                    aterm_uds::network::interfaces()?,
                    local,
                    selected,
                    native::index,
                    native::physical,
                )
            },
        )
    })
}

/// The attachments [`local_attachments`] keeps: the process's, and in a test each thread's own,
/// so one test's generations never meet another's.
fn kept_attachments<R>(read: impl FnOnce(&AttachmentMemo) -> R) -> R {
    #[cfg(not(test))]
    {
        static MEMO: AttachmentMemo = AttachmentMemo::new();
        read(&MEMO)
    }
    #[cfg(test)]
    {
        thread_local! {
            static MEMO: AttachmentMemo = const { AttachmentMemo::new() };
        }
        MEMO.with(read)
    }
}

/// The instant [`local_attachments`] keeps a read by: now, or in a test the instant its thread
/// fixed ([`FIXED_NOW`]).
fn attachments_clock() -> Instant {
    #[cfg(test)]
    if let Some(now) = FIXED_NOW.get() {
        return now;
    }
    Instant::now()
}

#[cfg(test)]
thread_local! {
    /// The enumerations [`local_attachments`] made on this thread, counted (never timed).
    static ENUMERATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The instant [`local_attachments`] keeps reads by on this thread, where a test fixed one.
    static FIXED_NOW: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// How long a kept read of the attachments may stand even while the generation does: a bound on
/// what a message the kernel dropped from a full routing socket, which says nothing, could leave
/// unseen.
const ATTACHMENTS_REREAD: Duration = Duration::from_secs(10);

/// Keep no read of the attachments: the next [`local_attachments`] of each asks the OS. A
/// listener that heard a change on its own routing socket ([`Listener::drain_changes`]) judges
/// by what the OS says now, whether or not the socket that moves the generation
/// ([`native::interface_generation`], another socket) has been handed the message yet.
fn forget_attachments() {
    kept_attachments(AttachmentMemo::forget);
}

/// The instant by which a listener bound to `local` that hears nothing must judge its
/// attachment by a fresh read all the same. On macOS, where the kernel drops a routing message
/// from a full socket and says nothing, that is when the read it last judged by stops being kept
/// ([`ATTACHMENTS_REREAD`] after it was made, so the recheck then reads again), or one
/// [`crate::slots::ACCEPT_POLL`] from now where none is kept (no generation, so every recheck
/// reads). `None` on Linux: rtnetlink reports a lost message as an error on the listener's own
/// socket, which withdraws it ([`Listener::drain_changes`]).
fn unheard_look(local: SocketAddr) -> Option<Instant> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    Some(
        kept_attachments(|memo| memo.kept_until(Some(local), None))
            .unwrap_or_else(|| attachments_clock() + crate::slots::ACCEPT_POLL),
    )
}

/// The attachments read under one interface generation, for each `(local, selected)` asked.
struct AttachmentMemo {
    held: std::sync::Mutex<Vec<HeldAttachments>>,
}

struct HeldAttachments {
    generation: u64,
    read_at: Instant,
    local: Option<SocketAddr>,
    selected: Option<u32>,
    attachments: Vec<Attachment>,
}

impl AttachmentMemo {
    /// The most `(local, selected)` reads kept: a Mac's links, and a test's.
    const MAX: usize = 16;

    const fn new() -> Self {
        Self {
            held: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// When the read kept for `local` and `selected` passes [`ATTACHMENTS_REREAD`], whatever
    /// generation it was read under. `None` where none is kept.
    fn kept_until(&self, local: Option<SocketAddr>, selected: Option<u32>) -> Option<Instant> {
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|held| held.local == local && held.selected == selected)
            .map(|held| held.read_at + ATTACHMENTS_REREAD)
    }

    /// Keep nothing: the next read of each `(local, selected)` runs its `read`.
    fn forget(&self) {
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    /// The attachments `read` gives for `local` and `selected`, kept under `generation` (taken
    /// before the read, so a change after it moves the next one) for [`ATTACHMENTS_REREAD`]
    /// from `now`. With no generation every call reads; a failed read is never kept.
    fn read(
        &self,
        generation: Option<u64>,
        now: Instant,
        local: Option<SocketAddr>,
        selected: Option<u32>,
        read: impl FnOnce() -> io::Result<Vec<Attachment>>,
    ) -> io::Result<Vec<Attachment>> {
        let Some(generation) = generation else {
            return read();
        };
        let kept = |held: &HeldAttachments| {
            held.generation == generation
                && held.local == local
                && held.selected == selected
                && now.saturating_duration_since(held.read_at) < ATTACHMENTS_REREAD
        };
        if let Some(held) = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|held| kept(held))
        {
            return Ok(held.attachments.clone());
        }
        let attachments = read()?;
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.retain(|held| {
            held.generation == generation && (held.local, held.selected) != (local, selected)
        });
        if held.len() >= Self::MAX {
            held.remove(0);
        }
        held.push(HeldAttachments {
            generation,
            read_at: now,
            local,
            selected,
            attachments: attachments.clone(),
        });
        Ok(attachments)
    }
}

fn local_attachments_from(
    interfaces: impl IntoIterator<Item = aterm_uds::network::InterfaceAddress>,
    local: Option<SocketAddr>,
    selected: Option<u32>,
    mut interface_index: impl FnMut(&str) -> io::Result<u32>,
    mut interface_physical: impl FnMut(&str) -> io::Result<bool>,
) -> io::Result<Vec<Attachment>> {
    let mut candidates = Vec::new();
    for entry in interfaces {
        // An explicit IPv4 source cannot match any other raw address. Skip
        // those before an index lookup, which can enumerate interfaces again.
        // IPv6 still needs the index to validate and remove an embedded zone.
        if local.is_some_and(|local| local.is_ipv4() && entry.address != local.ip()) {
            continue;
        }
        // An unrelated interface can disappear during enumeration. If it was
        // the selected attachment, the candidate set remains empty and refuses.
        let Ok(index) = interface_index(&entry.name) else {
            continue;
        };
        if selected.is_some_and(|selected| index != selected) {
            continue;
        }
        let address = scoped_ip(entry.address, index)?;
        if local.is_some_and(|local| address != local.ip()) {
            continue;
        }
        let physical = interface_physical(&entry.name)?;
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
