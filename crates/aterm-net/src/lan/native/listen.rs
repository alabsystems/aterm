// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The only transition from a prepared Commons socket to a listening socket.

use super::{listener_policy, socket_policy, tcp::bound, within};
use std::{
    io,
    net::{SocketAddr, TcpListener, TcpStream},
    os::fd::{AsRawFd, OwnedFd},
    time::Instant,
};

pub(in crate::lan) struct Prepared {
    stream: TcpStream,
    local: SocketAddr,
    index: Option<u32>,
    until: Instant,
}
impl Prepared {
    pub(in crate::lan) fn new(
        source: SocketAddr,
        interface: Option<(u32, &str)>,
        until: Instant,
    ) -> io::Result<Self> {
        within(until)?;
        let stream = bound(source, interface)?;
        let local = stream.local_addr()?;
        Ok(Self {
            stream,
            local,
            index: interface.map(|(index, _)| index),
            until,
        })
    }

    pub(in crate::lan) fn listen(
        self,
        current: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<TcpListener> {
        self.listen_using(current, start)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetLanPreparedListen",
            action = "IPv4",
            project = "aterm_net::lan::native::listen::tests::listen_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetLanPreparedListen",
            action = "IPv6",
            project = "aterm_net::lan::native::listen::tests::listen_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetLanPreparedListen",
            action = "Expired",
            project = "aterm_net::lan::native::listen::tests::listen_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetLanPreparedListen",
            action = "ChangedHops",
            project = "aterm_net::lan::native::listen::tests::listen_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetLanPreparedListen",
            action = "WrongBinding",
            project = "aterm_net::lan::native::listen::tests::listen_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetLanPreparedListen",
            action = "StaleAttachment",
            project = "aterm_net::lan::native::listen::tests::listen_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetLanPreparedListen",
            action = "LoopbackIPv4",
            project = "aterm_net::lan::native::listen::tests::listen_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetLanPreparedListen",
            action = "LoopbackIPv6",
            project = "aterm_net::lan::native::listen::tests::listen_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetLanPreparedListen",
            action = "IPv6ChangedHops",
            project = "aterm_net::lan::native::listen::tests::listen_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetLanPreparedListen",
            action = "IPv6WrongBinding",
            project = "aterm_net::lan::native::listen::tests::listen_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetLanPreparedListen",
            action = "WrongSource",
            project = "aterm_net::lan::native::listen::tests::listen_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetLanPreparedListen",
            action = "DualStack",
            project = "aterm_net::lan::native::listen::tests::listen_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetLanPreparedListen",
            action = "IPv6Expired",
            project = "aterm_net::lan::native::listen::tests::listen_transition"
        )
    )]
    fn listen_using(
        self,
        current: impl FnOnce() -> io::Result<()>,
        start: impl FnOnce(&TcpStream) -> io::Result<()>,
    ) -> io::Result<TcpListener> {
        current()?;
        if self.stream.local_addr()? != self.local {
            return Err(super::refuse("prepared listener source changed"));
        }
        socket_policy(&self.stream, self.index, self.index.is_some())?;
        #[cfg(target_os = "macos")]
        if super::option(&self.stream, libc::SOL_SOCKET, super::tcp::NO_SIGPIPE)? != 1 {
            return Err(super::refuse("listener lost SIGPIPE protection"));
        }
        if self.stream.local_addr()?.is_ipv6()
            && super::option(&self.stream, libc::IPPROTO_IPV6, libc::IPV6_V6ONLY)? != 1
        {
            return Err(super::refuse("dual-stack LAN listeners are unsupported"));
        }
        within(self.until)?;
        start(&self.stream)?;
        let listener = TcpListener::from(OwnedFd::from(self.stream));
        listener_policy(&listener, self.index)?;
        Ok(listener)
    }
}

fn start(stream: &TcpStream) -> io::Result<()> {
    // SAFETY: stream owns a prepared, bound socket; listen takes no pointers.
    if unsafe { libc::listen(stream.as_raw_fd(), 128) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "listen_tests.rs"]
mod tests;
