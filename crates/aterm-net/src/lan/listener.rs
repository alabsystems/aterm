// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A nonblocking listener configured before the kernel may answer a SYN.

use super::{
    Attachment, Connection, Scope, attachments_clock, forget_attachments, local_attachments,
    native, ready, refuse, same_subnet, unambiguous, unheard_look, unicast,
};
use crate::slots::ACCEPT_POLL;
use aterm_uds::stop::Stop;
use std::{
    io,
    net::{IpAddr, SocketAddr, SocketAddrV6, TcpListener},
    os::fd::BorrowedFd,
    sync::{Arc, Mutex, PoisonError},
    time::Instant,
};

/// A socket bound to one observed LAN attachment, or explicitly to loopback.
/// Own this value for its whole service lifetime and drop it on withdrawal.
/// It grants no membership, table access or terminal control. A LAN listener hears its route
/// and interface changes, for a server that waits on them ([`crate::fleet::serve_lan`]); an
/// owner that rechecks its listener on a clock of its own binds a [`RecheckedListener`].
pub struct Listener {
    pub(crate) tcp: TcpListener,
    local: SocketAddr,
    pub(crate) scope: Scope,
    attachment: Option<Attachment>,
    withdrawn: Arc<Stop>,
    /// The route and interface changes a server waits on ([`Listener::bind`]): none for a
    /// loopback listener or the one a [`RecheckedListener`] holds, whose owner waits on none.
    changes: Option<native::RouteChanges>,
    /// The one more look a heard change is owed ([`Listener::drain_changes`]), until a recheck
    /// at or after it gives it.
    settle: Mutex<Option<Instant>>,
}
impl std::os::fd::AsFd for Listener {
    /// Borrow readiness only: [`Listener::accept`] still rechecks the attachment.
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        std::os::fd::AsFd::as_fd(&self.tcp)
    }
}
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.withdrawn.set();
    }
}

/// The listener's original source attachment, without retaining its socket.
#[derive(Clone)]
pub(crate) struct Witness {
    local: SocketAddr,
    attachment: Option<Attachment>,
    withdrawn: Arc<Stop>,
}
impl Witness {
    pub(crate) fn recheck(&self) -> io::Result<()> {
        let checked = (|| {
            if self.withdrawn.is_set() {
                return Err(refuse("listener was withdrawn; bind again"));
            }
            if let Some(attachment) = &self.attachment
                && select(self.local, &local_attachments(Some(self.local), None)?)? != *attachment
            {
                return Err(refuse("listener attachment changed; bind again"));
            }
            Ok(())
        })();
        if checked.is_err() {
            let _ = self.withdrawn.set();
        } else if self.withdrawn.is_set() {
            return Err(refuse("listener was withdrawn; bind again"));
        }
        checked
    }
}

fn address(attachment: &Attachment) -> SocketAddr {
    match attachment.address {
        IpAddr::V6(ip) if ip.is_unicast_link_local() => {
            SocketAddrV6::new(ip, 0, 0, attachment.index).into()
        }
        ip => SocketAddr::new(ip, 0),
    }
}

fn eligible(local: SocketAddr, attachment: &Attachment) -> bool {
    ready(attachment)
        && unambiguous(local)
        && unicast(local.ip())
        && !local.ip().is_loopback()
        && local.ip() == attachment.address
        && same_subnet(local.ip(), local.ip(), attachment.netmask)
        && match local {
            SocketAddr::V6(v6) => {
                v6.scope_id()
                    == if v6.ip().is_unicast_link_local() {
                        attachment.index
                    } else {
                        0
                    }
            }
            _ => true,
        }
}

fn remote_supported(address: SocketAddr) -> bool {
    // XNU copies inp_ip_ttl but not in6p_hops into the child before SYN-ACK.
    // Do not claim a one-hop IPv6 listener where the kernel cannot preserve it.
    !cfg!(target_os = "macos") || address.is_ipv4()
}

pub(super) fn select(local: SocketAddr, attachments: &[Attachment]) -> io::Result<Attachment> {
    let mut candidates = attachments.iter().filter(|entry| {
        entry.address == local.ip()
            && match local {
                SocketAddr::V6(v6) if v6.ip().is_unicast_link_local() => {
                    v6.scope_id() == entry.index
                }
                _ => true,
            }
    });
    let chosen = candidates
        .next()
        .ok_or_else(|| refuse("listener has no live physical LAN attachment"))?;
    if candidates.next().is_some() {
        return Err(refuse("listener has ambiguous LAN attachments"));
    }
    if !eligible(local, chosen) {
        return Err(refuse("listener source is not a live physical LAN"));
    }
    Ok(chosen.clone())
}

impl Listener {
    /// Enumerate eligible local addresses without sending traffic or opening a
    /// listener. Port zero requests an ephemeral port; IPv6 link-local addresses
    /// carry their interface zone. Each bind rechecks these untrusted hints.
    pub fn candidates() -> io::Result<Vec<SocketAddr>> {
        let attachments = local_attachments(None, None)?;
        let mut addresses = attachments
            .iter()
            .map(address)
            .filter(|local| remote_supported(*local))
            .filter(|local| select(*local, &attachments).is_ok())
            .collect::<Vec<_>>();
        addresses.sort();
        addresses.dedup();
        Ok(addresses)
    }

    /// Bind exactly one source address for a server that waits on this listener's route and
    /// interface changes ([`Listener::changes_fd`]), as [`crate::fleet::serve_lan`] does: a LAN
    /// listener subscribes to them before it first observes its attachment. Wildcards, tunnels
    /// and ambiguous source attachments are refused. LAN interface binding and one-hop limits are
    /// established before listen, including the kernel's initial SYN-ACK.
    pub fn bind(local: SocketAddr, scope: Scope) -> io::Result<Self> {
        Self::bind_heard(local, scope, true)
    }

    /// [`Listener::bind`], a LAN listener subscribing to its route and interface changes where
    /// `heard` says a server waits on them ([`RecheckedListener::bind`] says none does).
    fn bind_heard(local: SocketAddr, scope: Scope, heard: bool) -> io::Result<Self> {
        crate::loopback::check(crate::loopback::Reach::Listen(local))?;
        if scope == Scope::Lan && !remote_supported(local) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "IPv6 LAN listeners require inheritable hop limits on this platform",
            ));
        }
        let until = native::deadline();
        // Subscribe before the first observation so changes during bind remain queued.
        let changes = match scope {
            Scope::Lan if heard => Some(native::RouteChanges::open()?),
            Scope::Lan | Scope::Loopback => None,
        };
        let attachment = match scope {
            Scope::Lan => Some(
                local_attachments(Some(local), None)
                    .and_then(|attachments| select(local, &attachments))?,
            ),
            Scope::Loopback if unambiguous(local) && local.ip().is_loopback() => None,
            Scope::Loopback => {
                return Err(refuse(
                    "local listener requires an unambiguous loopback address",
                ));
            }
        };
        let prepared = native::Prepared::new(
            local,
            attachment.as_ref().map(|a| (a.index, a.name.as_str())),
            until,
        )?;
        let tcp = prepared.listen(|| {
            if let Some(attachment) = &attachment
                && select(local, &local_attachments(Some(local), None)?)? != *attachment
            {
                return Err(refuse("listener attachment changed before listen"));
            }
            Ok(())
        })?;
        let local = tcp.local_addr()?;
        let listener = Self {
            tcp,
            local,
            scope,
            attachment,
            withdrawn: Arc::new(Stop::new()),
            changes,
            settle: Mutex::new(None),
        };
        listener.recheck()?;
        Ok(listener)
    }

    /// The exact bound address, including the assigned ephemeral port.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.tcp.local_addr()
    }

    /// Borrow this listener's permanent withdrawal bell. It stays ready after withdrawal;
    /// observing it never consumes another waiter's notification.
    pub(crate) fn withdrawal_bell(&self) -> io::Result<BorrowedFd<'_>> {
        self.withdrawn.bell()
    }

    /// This LAN listener's own route/interface notification descriptor. A loopback listener has
    /// none, nor has the one a [`RecheckedListener`] holds, whose owner waits on no change.
    pub(crate) fn changes_fd(&self) -> Option<BorrowedFd<'_>> {
        self.changes.as_ref().map(native::RouteChanges::fd)
    }

    /// Drain notifications without waiting, and say whether the server must recheck. Lookup-only
    /// traffic does not ask for another recheck; losing this listener's event source withdraws
    /// it rather than leaving it asleep. A heard change is what wakes an idle listener, which
    /// keeps no timer of its own. One that can change an attachment (an interface or address
    /// message, or so much queued that the kernel may have dropped one) has the recheck that
    /// follows read the attachments afresh ([`forget_attachments`]) and owes one more look
    /// [`ACCEPT_POLL`] later ([`Listener::validity_deadline`]), should the message have come
    /// before the state it announces. A change of the route table alone asks for the recheck
    /// and no more: [`Listener::recheck`] judges no route, and no interface or address moved, so
    /// the read it keeps stands, as it does under the interface generation the same types leave
    /// still.
    pub(crate) fn drain_changes(&self) -> io::Result<bool> {
        let heard = self
            .changes
            .as_ref()
            .map_or(Ok(native::Heard::Nothing), native::RouteChanges::drain);
        match heard {
            Err(_) => {
                let _ = self.withdrawn.set();
            }
            Ok(native::Heard::Attachments) => {
                forget_attachments();
                let owed = attachments_clock() + ACCEPT_POLL;
                *self.settle.lock().unwrap_or_else(PoisonError::into_inner) = Some(owed);
            }
            Ok(native::Heard::Routes | native::Heard::Nothing) => {}
        }
        heard.map(|heard| heard != native::Heard::Nothing)
    }

    /// When a server that has heard nothing more must run [`Listener::recheck`] again: the look
    /// a heard change is still owed, and where the kernel can lose a change silently the instant
    /// the attachments this listener judges by are read again ([`unheard_look`]). `None` is no
    /// timer: a loopback listener has no attachment to lose, and a listener's withdrawal bell and
    /// change descriptor wake its server for everything else. The only LAN listener with no
    /// change descriptor is the one a [`RecheckedListener`] holds, and nothing asks for its
    /// deadline: its owner rechecks it on a clock of its own.
    pub(crate) fn validity_deadline(&self) -> Option<Instant> {
        let settle = *self.settle.lock().unwrap_or_else(PoisonError::into_inner);
        let unheard = self
            .attachment
            .as_ref()
            .and_then(|_| unheard_look(self.local));
        match (settle, unheard) {
            (Some(settle), Some(unheard)) => Some(settle.min(unheard)),
            (settle, unheard) => settle.or(unheard),
        }
    }

    /// This listener hearing its changes from `socket`, a datagram socket a test delivers the
    /// kernel's routing messages to.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn hearing(self, socket: std::os::fd::OwnedFd) -> Self {
        self.hearing_in(socket, 64 * 1024)
    }

    /// [`Listener::hearing`], the socket's receive room taken as `room` bytes.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn hearing_in(mut self, socket: std::os::fd::OwnedFd, room: usize) -> Self {
        self.changes = Some(native::RouteChanges::over(socket, room));
        self
    }

    pub(crate) fn witness(&self) -> Witness {
        Witness {
            local: self.local,
            attachment: self.attachment.clone(),
            withdrawn: self.withdrawn.clone(),
        }
    }

    /// Withdraw permanently if the socket or its attachment changed. The owner
    /// must stop service and drop this listener on error, then bind a fresh one.
    pub fn recheck(&self) -> io::Result<()> {
        if self.withdrawn.is_set() {
            return Err(refuse("listener was withdrawn; bind again"));
        }
        let now = attachments_clock();
        if self
            .settle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take_if(|at| now >= *at)
            .is_some()
        {
            // The look a heard change was owed: by what the OS says now.
            forget_attachments();
        }
        let result = (|| {
            if self.tcp.local_addr()? != self.local {
                return Err(refuse("listener source changed"));
            }
            native::listener_policy(&self.tcp, self.attachment.as_ref().map(|a| a.index))?;
            self.witness().recheck()
        })();
        if result.is_err() {
            let _ = self.withdrawn.set();
        } else if self.withdrawn.is_set() {
            return Err(refuse("listener was withdrawn; bind again"));
        }
        result
    }

    /// Accept one connection without blocking. Both the listener and the actual
    /// connected endpoints are checked before returning a typed connection.
    pub fn accept(&self) -> io::Result<Connection> {
        self.recheck()?;
        // The listener stays nonblocking so accept never waits under the lock. The connection is
        // blocking because its TLS exchange uses read/write deadlines and waits for peer bytes.
        let (stream, _) = aterm_uds::accept(
            &self.tcp,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        )?;
        let connection = Connection::admitted_by(stream, self.scope, self.witness())?;
        self.recheck()?;
        Ok(connection)
    }

    /// Whether a connection waits in this listener's queue within `timeout`, without accepting
    /// it: a test's proof that a peer's handshake reached the queue, which the kernel may finish
    /// after the peer's `connect` returns.
    ///
    /// # Errors
    /// The wait on the socket fails.
    #[cfg(any(test, debug_assertions))]
    pub fn pending_within(&self, timeout: std::time::Duration) -> io::Result<bool> {
        crate::slots::listener_pending_within(&self.tcp, timeout)
    }
}

/// A [`Listener`] for an owner that rechecks it on a clock of its own and waits on none of its
/// changes, as the Commons transport rechecks each link's listener at every turn: it subscribes
/// to no route or interface change. A routing socket nobody reads is handed every routing
/// message on the machine, the kernel copying each, until its room fills.
/// [`RecheckedListener::recheck`] and [`RecheckedListener::accept`] judge its attachment as a
/// [`Listener`]'s are judged, by the attachments as this process reads them
/// (`local_attachments`), so an attachment that moved withdraws it at its owner's next recheck.
///
/// It is not a [`Listener`], because a server that waits on a listener's changes
/// ([`crate::fleet::serve_lan`]) would hear nothing from it: with no change descriptor, and off
/// macOS no timer either (`Listener::validity_deadline`), nothing would wake that server when
/// this listener's address went away. Such a server takes a [`Listener`] only:
///
/// ```compile_fail,E0308
/// # fn serve(
/// #     listening: &aterm_net::fleet::Listening,
/// #     stop: &aterm_uds::stop::Stop,
/// # ) -> std::io::Result<()> {
/// use aterm_net::{fleet, lan::{RecheckedListener, Scope}};
/// let listener = RecheckedListener::bind("192.0.2.1:0".parse().unwrap(), Scope::Lan)?;
/// fleet::serve_lan(listener, listening, stop, |_, _| Ok(()), |_| {});
/// # Ok(())
/// # }
/// ```
pub struct RecheckedListener(Listener);
impl std::os::fd::AsFd for RecheckedListener {
    /// Borrow readiness only: [`RecheckedListener::accept`] still rechecks the attachment.
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl RecheckedListener {
    /// [`Listener::bind`], with no subscription to the listener's route and interface changes.
    pub fn bind(local: SocketAddr, scope: Scope) -> io::Result<Self> {
        Listener::bind_heard(local, scope, false).map(Self)
    }

    /// The exact bound address, including the assigned ephemeral port.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }

    /// [`Listener::recheck`]: withdraw permanently if the socket or its attachment changed. The
    /// owner must stop service and drop this listener on error, then bind a fresh one.
    pub fn recheck(&self) -> io::Result<()> {
        self.0.recheck()
    }

    /// [`Listener::accept`]: one connection without blocking, the listener and the actual
    /// connected endpoints checked before it is returned.
    pub fn accept(&self) -> io::Result<Connection> {
        self.0.accept()
    }

    /// [`Listener::pending_within`]: whether a connection waits in this listener's queue within
    /// `timeout`, without accepting it (tests).
    ///
    /// # Errors
    /// The wait on the socket fails.
    #[cfg(any(test, debug_assertions))]
    pub fn pending_within(&self, timeout: std::time::Duration) -> io::Result<bool> {
        self.0.pending_within(timeout)
    }
}

#[cfg(test)]
#[path = "listener_tests.rs"]
mod tests;
