// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A nonblocking listener configured before the kernel may answer a SYN.

use super::{
    Attachment, Connection, Scope, local_attachments, native, ready, refuse, same_subnet,
    unambiguous, unicast,
};
use aterm_uds::stop::Stop;
use std::{
    io,
    net::{IpAddr, SocketAddr, SocketAddrV6, TcpListener},
    os::fd::BorrowedFd,
    sync::Arc,
};

/// A socket bound to one observed LAN attachment, or explicitly to loopback.
/// Own this value for its whole service lifetime and drop it on withdrawal.
/// It grants no membership, table access or terminal control.
pub struct Listener {
    pub(crate) tcp: TcpListener,
    local: SocketAddr,
    pub(crate) scope: Scope,
    attachment: Option<Attachment>,
    withdrawn: Arc<Stop>,
    changes: Option<native::RouteChanges>,
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

    /// Bind exactly one source address. Wildcards, tunnels and ambiguous source
    /// attachments are refused. LAN interface binding and one-hop limits are
    /// established before listen, including the kernel's initial SYN-ACK.
    pub fn bind(local: SocketAddr, scope: Scope) -> io::Result<Self> {
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
            Scope::Lan => Some(native::RouteChanges::open()?),
            Scope::Loopback => None,
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

    /// This LAN listener's own route/interface notification descriptor. Loopback has none.
    pub(crate) fn changes_fd(&self) -> Option<BorrowedFd<'_>> {
        self.changes.as_ref().map(native::RouteChanges::fd)
    }

    /// Drain notifications without waiting. Lookup-only traffic does not ask for another
    /// recheck; losing this listener's event source withdraws it rather than leaving it asleep.
    pub(crate) fn drain_changes(&self) -> io::Result<bool> {
        let result = self
            .changes
            .as_ref()
            .map_or(Ok(false), native::RouteChanges::drain);
        if result.is_err() {
            let _ = self.withdrawn.set();
        }
        result
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

#[cfg(test)]
#[path = "listener_tests.rs"]
mod tests;
