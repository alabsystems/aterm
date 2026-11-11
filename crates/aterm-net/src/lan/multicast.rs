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
#[path = "multicast_tests.rs"]
mod tests;
