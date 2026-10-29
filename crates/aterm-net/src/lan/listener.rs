// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A nonblocking listener configured before the kernel may answer a SYN.

use super::{
    Attachment, BindFailure, Connection, Scope, local_attachments, native, ready, refuse,
    same_subnet, unambiguous, unicast,
};
use std::{
    io,
    net::{IpAddr, SocketAddr, SocketAddrV6, TcpListener},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// A socket bound to one observed LAN attachment, or explicitly to loopback.
/// Own this value for its whole service lifetime and drop it on withdrawal.
/// It grants no membership, table access or terminal control.
pub struct Listener {
    pub(crate) tcp: TcpListener,
    local: SocketAddr,
    pub(crate) scope: Scope,
    attachment: Option<Attachment>,
    withdrawn: Arc<AtomicBool>,
}
impl Drop for Listener {
    fn drop(&mut self) {
        self.withdrawn.store(true, Ordering::Release);
    }
}

/// The listener's original source attachment, without retaining its socket.
#[derive(Clone)]
pub(crate) struct Witness {
    local: SocketAddr,
    attachment: Option<Attachment>,
    withdrawn: Arc<AtomicBool>,
}
impl Witness {
    pub(crate) fn recheck(&self) -> io::Result<()> {
        let checked = (|| {
            if self.withdrawn.load(Ordering::Acquire) {
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
            self.withdrawn.store(true, Ordering::Release);
        } else if self.withdrawn.load(Ordering::Acquire) {
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
        Self::try_bind(local, scope).map_err(|failure| failure.error)
    }

    /// [`Self::bind`], saying of a failure whether it reached its socket ([`BindFailure`]): a
    /// listener whose attachment changed before its re-check fails after its bind and listen.
    pub fn try_bind(local: SocketAddr, scope: Scope) -> Result<Self, BindFailure> {
        crate::loopback::check(crate::loopback::Reach::Listen(local))
            .map_err(BindFailure::before)?;
        if scope == Scope::Lan && !remote_supported(local) {
            return Err(BindFailure::before(io::Error::new(
                io::ErrorKind::Unsupported,
                "IPv6 LAN listeners require inheritable hop limits on this platform",
            )));
        }
        let until = native::deadline();
        let attachment = match scope {
            Scope::Lan => Some(
                local_attachments(Some(local), None)
                    .and_then(|attachments| select(local, &attachments))
                    .map_err(BindFailure::before)?,
            ),
            Scope::Loopback if unambiguous(local) && local.ip().is_loopback() => None,
            Scope::Loopback => {
                return Err(BindFailure::before(refuse(
                    "local listener requires an unambiguous loopback address",
                )));
            }
        };
        (|| {
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
                withdrawn: Arc::new(AtomicBool::new(false)),
            };
            listener.recheck()?;
            Ok(listener)
        })()
        .map_err(BindFailure::after)
    }

    /// The exact bound address, including the assigned ephemeral port.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.tcp.local_addr()
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
        if self.withdrawn.load(Ordering::Acquire) {
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
            self.withdrawn.store(true, Ordering::Release);
        } else if self.withdrawn.load(Ordering::Acquire) {
            return Err(refuse("listener was withdrawn; bind again"));
        }
        result
    }

    /// Accept one connection without blocking. Both the listener and the actual
    /// connected endpoints are checked before returning a typed connection.
    pub fn accept(&self) -> io::Result<Connection> {
        self.recheck()?;
        let (stream, _) = self.tcp.accept()?;
        let connection = Connection::admitted_by(stream, self.scope, self.witness())?;
        self.recheck()?;
        Ok(connection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_selection_refuses_ambiguous_virtual_and_unscoped_addresses() {
        let mut a = Attachment {
            index: 3,
            name: "test-physical".into(),
            address: "192.0.2.1".parse().unwrap(),
            netmask: "255.255.255.0".parse().unwrap(),
            flags: super::super::REQUIRED_FLAGS,
            physical: true,
        };
        let local = address(&a);
        assert_eq!(select(local, std::slice::from_ref(&a)).unwrap(), a);
        let mut another = a.clone();
        another.index += 1;
        assert!(select(local, &[a.clone(), another.clone()]).is_err());
        another.physical = false;
        assert!(
            select(local, &[a.clone(), another.clone()]).is_err(),
            "a virtual duplicate cannot be hidden by eligibility filtering"
        );
        another.netmask = "0.0.0.0".parse().unwrap();
        assert!(
            select(local, &[a.clone(), another]).is_err(),
            "an unknown prefix does not hide a duplicate address"
        );
        a.physical = false;
        assert!(select(local, std::slice::from_ref(&a)).is_err());
        a.physical = true;
        a.flags = 0;
        assert!(select(local, std::slice::from_ref(&a)).is_err());
        a.flags = super::super::REQUIRED_FLAGS;
        a.address = "fe80::1".parse().unwrap();
        a.netmask = "ffff:ffff:ffff:ffff::".parse().unwrap();
        assert!(select("[fe80::1]:0".parse().unwrap(), std::slice::from_ref(&a)).is_err());
        assert_eq!(select(address(&a), std::slice::from_ref(&a)).unwrap(), a);
        a.netmask = "::".parse().unwrap();
        assert!(select(address(&a), &[a]).is_err());
    }

    #[test]
    fn wildcard_and_scope_confusion_never_create_a_listener() {
        for address in [
            "0.0.0.0:0",
            "[::]:0",
            "[::ffff:127.0.0.1]:0",
            "[::1%3]:0",
            "224.0.0.1:0",
            "[ff02::1%3]:0",
        ] {
            for scope in [Scope::Loopback, Scope::Lan] {
                assert!(
                    Listener::bind(address.parse().unwrap(), scope).is_err(),
                    "{address} {scope:?}"
                );
            }
        }
        assert!(Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Lan).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unsupported_ipv6_lan_listener_is_withheld_before_bind() {
        let error = Listener::bind("[2001:db8::1]:0".parse().unwrap(), Scope::Lan)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(!remote_supported("[fe80::1%3]:0".parse().unwrap()));
        assert!(remote_supported("192.0.2.1:0".parse().unwrap()));
    }

    #[test]
    fn withdrawal_is_permanent_and_service_drops_the_listening_socket() {
        let mut listener = Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap();
        let address = listener.local_addr().unwrap();
        listener.local.set_port(0);
        assert!(listener.recheck().is_err());
        listener.local = address;
        assert!(
            listener.recheck().is_err(),
            "restoring an observation cannot restore a withdrawn listener"
        );
        let stopped = std::sync::Arc::new(AtomicBool::new(false));
        let event = stopped.clone();
        crate::fleet::serve_lan(
            listener,
            &crate::fleet::Listening {
                secret: crate::fleet::FleetSecret::new(&[7; 32]).unwrap(),
                scope: crate::fleet::FleetScope::Facts,
                node: None,
                serves: &[crate::fleet::FleetOp::Survey],
                slots: 2,
                per_source: 1,
                lifetime: aterm_spec::wait::MUST_HAPPEN,
            },
            &AtomicBool::new(true),
            |_, _| panic!("withdrawn listener must not admit a peer"),
            move |e| {
                if matches!(e, crate::fleet::FleetEvent::Stopped(_)) {
                    assert!(
                        std::net::TcpStream::connect_timeout(
                            &address,
                            aterm_spec::wait::MUST_HAPPEN
                        )
                        .is_err(),
                        "Stopped is delivered only after the kernel listener closes"
                    );
                    event.store(true, Ordering::Release);
                }
            },
        );
        assert!(stopped.load(Ordering::Acquire));
        assert!(
            std::net::TcpStream::connect_timeout(&address, aterm_spec::wait::MUST_HAPPEN).is_err()
        );
    }

    #[test]
    fn explicit_loopback_accept_returns_a_rechecked_connection() {
        for address in ["127.0.0.1:0", "[::1]:0"] {
            let listener = Listener::bind(address.parse().unwrap(), Scope::Loopback).unwrap();
            let _peer = std::net::TcpStream::connect_timeout(
                &listener.local_addr().unwrap(),
                aterm_spec::wait::MUST_HAPPEN,
            )
            .unwrap();
            assert!(
                crate::slots::wait_listener_readable(&listener.tcp, aterm_spec::wait::MUST_HAPPEN)
                    .unwrap()
            );
            listener.accept().unwrap().into_parts().1.recheck().unwrap();
            assert!(
                matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
            );
        }
    }

    #[test]
    fn dropping_a_listener_withdraws_its_workers_without_retaining_the_socket() {
        let listener = Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap();
        let address = listener.local_addr().unwrap();
        let witness = listener.witness();
        witness.recheck().unwrap();
        drop(listener);
        assert!(witness.recheck().is_err());
        assert!(
            std::net::TcpStream::connect_timeout(&address, aterm_spec::wait::MUST_HAPPEN).is_err()
        );
    }

    #[test]
    fn accepted_connection_cannot_outlive_its_listener_authority() {
        let listener = Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap();
        let _peer = std::net::TcpStream::connect_timeout(
            &listener.local_addr().unwrap(),
            aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap();
        assert!(
            crate::slots::wait_listener_readable(&listener.tcp, aterm_spec::wait::MUST_HAPPEN)
                .unwrap()
        );
        let (_, proof) = listener.accept().unwrap().into_parts();
        proof.recheck().unwrap();
        drop(listener);
        assert!(proof.recheck().is_err());
    }
}
