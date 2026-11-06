// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Untrusted discovery on one physical attachment. A wildcard receive address
//! accepts multicast; the OS interface binding and multicast source constrain
//! transmission. Received packets never grant membership or application access.

use super::{Attachment, Scope, listener, local_attachments, native, refuse, same_subnet, unicast};
use std::{
    io,
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    sync::atomic::{AtomicBool, Ordering},
};

impl std::os::fd::AsFd for Multicast {
    /// Borrow readiness only; receiving and sending still recheck the attachment.
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        std::os::fd::AsFd::as_fd(&self.socket)
    }
}

/// Maximum discovery datagram, below an ordinary LAN MTU.
pub const MAX_PACKET: usize = 1200;

/// A nonblocking one-hop IPv4 multicast socket. An attachment or socket policy
/// change withdraws it permanently. Data still requires a checked Connection.
pub struct Multicast {
    socket: UdpSocket,
    source: Ipv4Addr,
    destination: SocketAddr,
    local: SocketAddr,
    attachment: Option<Attachment>,
    withdrawn: AtomicBool,
}
impl Multicast {
    /// Join an administrative-local group (239.255/16) on an explicit source.
    /// Peers use the same port; zero is useful for isolated tests. Loopback is
    /// an explicit local/test policy and is never inferred as a LAN. For an attachment the root
    /// LAN broker makes the socket and joins the group on macOS (`aterm_uds::lan::udp`); a
    /// loopback socket is made in this process. An attachment's binds only the ports the broker
    /// admits (`aterm_uds::lan::here::check_udp`); this process configures and checks either.
    ///
    /// # Errors
    /// A refused group, source or attachment (`PermissionDenied`), for an attachment on macOS a
    /// missing LAN broker (`NotConnected`), a port the broker does not bind, or the socket's error.
    pub fn bind(source: Ipv4Addr, group: Ipv4Addr, port: u16, scope: Scope) -> io::Result<Self> {
        crate::loopback::check(crate::loopback::Reach::Listen((source, port).into()))?;
        if group.octets()[..2] != [239, 255] {
            return Err(refuse(
                "discovery requires an administrative-local multicast group",
            ));
        }
        let local = SocketAddr::from((source, 0));
        let attachment = match scope {
            Scope::Lan => Some(
                local_attachments(Some(local), None)
                    .and_then(|attachments| listener::select(local, &attachments))?,
            ),
            Scope::Loopback if source.is_loopback() => None,
            Scope::Loopback => {
                return Err(refuse("loopback discovery requires a loopback source"));
            }
        };
        let index = match &attachment {
            Some(attachment) => attachment.index,
            None => loopback_index()?,
        };
        let socket = native::multicast::bind(
            source,
            group,
            port,
            index,
            attachment.as_ref().map(|a| a.name.as_str()),
        )?;
        let local = socket.local_addr()?;
        let result = Self {
            socket,
            source,
            destination: (group, local.port()).into(),
            local,
            attachment,
            withdrawn: AtomicBool::new(false),
        };
        result.recheck()?;
        Ok(result)
    }

    /// Joined receive port, including a kernel-assigned test port.
    pub fn port(&self) -> u16 {
        self.local.port()
    }

    /// Permanently disable this socket before its owner releases it. A later
    /// send or receive refuses even if the original attachment is still live.
    pub fn withdraw(&self) {
        self.withdrawn.store(true, Ordering::Release);
    }

    pub fn recheck(&self) -> io::Result<()> {
        if self.withdrawn.load(Ordering::Acquire) {
            return Err(refuse("multicast attachment was withdrawn; bind again"));
        }
        let checked = (|| {
            native::multicast::policy(
                &self.socket,
                self.source,
                self.attachment.as_ref().map(|a| a.index),
            )?;
            if self.socket.local_addr()? != self.local {
                return Err(refuse("multicast receive address changed"));
            }
            if let Some(attachment) = &self.attachment {
                let source = SocketAddr::from((self.source, 0));
                if listener::select(source, &local_attachments(Some(source), None)?)? != *attachment
                {
                    return Err(refuse("multicast source attachment changed"));
                }
            }
            Ok(())
        })();
        if checked.is_err() {
            self.withdrawn.store(true, Ordering::Release);
        }
        checked
    }

    /// Send one bounded hint to this fixed group, never an arbitrary peer.
    pub fn send(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > MAX_PACKET {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "discovery packet exceeds its bound",
            ));
        }
        self.recheck()?;
        if self.socket.send_to(bytes, self.destination)? != bytes.len() {
            return Err(io::Error::other("incomplete discovery datagram"));
        }
        self.recheck()
    }

    /// Receive at most one untrusted hint. Oversized packets are discarded
    /// without parsing or allocation beyond the fixed receive buffer.
    pub fn receive(&self) -> io::Result<Option<(Vec<u8>, SocketAddr)>> {
        self.recheck()?;
        let mut bytes = [0; MAX_PACKET + 1];
        let (length, source) = match self.socket.recv_from(&mut bytes) {
            Ok(result) => result,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(error) => return Err(error),
        };
        self.recheck()?;
        if length > MAX_PACKET
            || !unicast(source.ip())
            || self.attachment.as_ref().is_some_and(|attachment| {
                !source_allowed(source.ip(), attachment.address, attachment.netmask)
            })
            || (self.attachment.is_none() && !source.ip().is_loopback())
        {
            return Ok(None);
        }
        Ok(Some((bytes[..length].to_vec(), source)))
    }
}

/// The index of the loopback interface, where a loopback discovery joins its group.
fn loopback_index() -> io::Result<u32> {
    native::index(if cfg!(target_os = "linux") {
        "lo"
    } else {
        "lo0"
    })
}

fn source_allowed(
    source: std::net::IpAddr,
    local: std::net::IpAddr,
    mask: std::net::IpAddr,
) -> bool {
    if source == local || !same_subnet(source, local, mask) {
        return false;
    }
    let (std::net::IpAddr::V4(source), std::net::IpAddr::V4(mask)) = (source, mask) else {
        return false;
    };
    let host_mask = !u32::from(mask);
    let host = u32::from(source) & host_mask;
    host_mask <= 1 || (host != 0 && host != host_mask)
}

#[cfg(test)]
mod tests {
    use super::*;
    const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 31, 84);

    #[test]
    fn multicast_bounds_and_permanent_withdrawal() {
        let socket = Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback).unwrap();
        assert!(socket.port() > 0);
        socket.recheck().unwrap();
        assert!(socket.send(&[0; MAX_PACKET + 1]).is_err());
        socket.socket.set_multicast_ttl_v4(2).unwrap();
        assert!(socket.recheck().is_err());
        socket.socket.set_multicast_ttl_v4(1).unwrap();
        assert!(socket.send(b"withdrawn").is_err());
        assert!(socket.receive().is_err());
    }

    /// The hint `socket` sends to its own group comes back to it (its sends loop back).
    fn hears_its_own_hint(socket: &UdpSocket, policy: impl Fn(&UdpSocket) -> io::Result<()>) {
        let port = socket.local_addr().unwrap().port();
        socket.send_to(b"hint", (GROUP, port)).unwrap();
        let until = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let mut bytes = [0; MAX_PACKET];
        let (length, from) = loop {
            match socket.recv_from(&mut bytes) {
                Ok(heard) => break heard,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("{error}"),
            }
            assert!(
                std::time::Instant::now() < until,
                "the hint never came back"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(&bytes[..length], b"hint");
        assert!(from.ip().is_loopback(), "{from}");
        policy(socket).unwrap();
    }

    /// A loopback discovery socket reaches no LAN, so it is made here: with no LAN broker at all
    /// (this thread's seam names a socket nothing serves) it binds and hears its own hint.
    #[test]
    fn a_loopback_socket_needs_no_broker() {
        let root = aterm_tempfile::tempdir().unwrap();
        let absent = aterm_uds::lan::Seam {
            socket: Some(root.path().join("absent.sock")),
            ..aterm_uds::lan::Seam::default()
        };
        aterm_uds::lan::seam_here(absent, || {
            let socket = Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback).unwrap();
            hears_its_own_hint(&socket.socket, |socket| {
                native::multicast::policy(socket, Ipv4Addr::LOCALHOST, None)
            });
        });
    }

    /// An attachment's discovery socket is the LAN broker's (on macOS; elsewhere made here):
    /// bound to its interface (loopback standing in for one), configured here, it carries a hint
    /// to its group and back and keeps its policy.
    #[test]
    fn an_attachments_socket_is_the_brokers_and_carries_a_hint() {
        let index = loopback_index().unwrap();
        let name = if cfg!(target_os = "linux") {
            "lo"
        } else {
            "lo0"
        };
        aterm_uds::lan::seam_here(crate::lan::tests::broker(), || {
            let socket =
                native::multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, index, Some(name)).unwrap();
            hears_its_own_hint(&socket, |socket| {
                native::multicast::policy(socket, Ipv4Addr::LOCALHOST, Some(index))
            });
        });
    }

    /// Commons' discovery group binds the fixed port its peers share
    /// (`aterm_uds::lan::here::COMMONS_PORT`, group 239.255.31.84), not the kernel's choice: the
    /// LAN broker must make that socket for an attachment, or Commons has no LAN on macOS.
    #[test]
    fn an_attachment_binds_the_commons_port_through_the_broker() {
        let index = loopback_index().unwrap();
        let name = if cfg!(target_os = "linux") {
            "lo"
        } else {
            "lo0"
        };
        let port = aterm_uds::lan::here::COMMONS_PORT;
        aterm_uds::lan::seam_here(crate::lan::tests::broker(), || {
            let socket = native::multicast::bind(
                Ipv4Addr::LOCALHOST,
                Ipv4Addr::new(239, 255, 31, 84),
                port,
                index,
                Some(name),
            )
            .unwrap();
            assert_eq!(socket.local_addr().unwrap().port(), port);
            native::multicast::policy(&socket, Ipv4Addr::LOCALHOST, Some(index)).unwrap();
        });
    }

    #[test]
    fn discovery_never_infers_loopback_or_accepts_other_scopes() {
        assert!(Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Lan).is_err());
        assert!(Multicast::bind(Ipv4Addr::UNSPECIFIED, GROUP, 0, Scope::Loopback).is_err());
        assert!(
            Multicast::bind(
                Ipv4Addr::LOCALHOST,
                Ipv4Addr::new(224, 0, 0, 251),
                0,
                Scope::Loopback
            )
            .is_err()
        );
    }

    #[test]
    fn discovery_sources_exclude_self_and_subnet_broadcast_but_allow_point_to_point_pairs() {
        let ip = |s: &str| s.parse().unwrap();
        for source in ["10.1.2.0", "10.1.2.255", "10.1.2.1", "10.2.2.3"] {
            assert!(!source_allowed(
                ip(source),
                ip("10.1.2.1"),
                ip("255.255.255.0")
            ));
        }
        assert!(source_allowed(
            ip("10.1.2.3"),
            ip("10.1.2.1"),
            ip("255.255.255.0")
        ));
        assert!(source_allowed(
            ip("10.1.2.0"),
            ip("10.1.2.1"),
            ip("255.255.255.254")
        ));
    }
}
