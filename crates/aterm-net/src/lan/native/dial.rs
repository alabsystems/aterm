// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An unconnected TCP socket whose source, interface and hop limit are fixed
//! before the first SYN. The caller checks its exact tuple before `connect`.

use super::{
    refuse, socket_policy,
    tcp::{Address, bound},
};
use std::{
    io,
    net::{SocketAddr, TcpStream},
    os::fd::AsRawFd,
    time::Instant,
};

pub(in crate::lan) struct Dial {
    stream: TcpStream,
    peer: SocketAddr,
    index: u32,
    until: Instant,
}
impl Dial {
    pub(in crate::lan) fn new(
        source: SocketAddr,
        peer: SocketAddr,
        index: u32,
        name: &str,
        until: Instant,
    ) -> io::Result<Self> {
        if index == 0
            || source.is_ipv4() != peer.is_ipv4()
            || source.port() != 0
            || !super::super::unambiguous(source)
            || !super::super::unambiguous(peer)
            || [source, peer].iter().any(|address| matches!(address, SocketAddr::V6(address) if address.scope_id() != 0 && address.scope_id() != index))
        {
            return Err(refuse("invalid LAN socket binding"));
        }
        let stream = bound(source, Some((index, name)))?;
        Ok(Self {
            stream,
            peer,
            index,
            until,
        })
    }

    pub(in crate::lan) fn local_addr(&self) -> io::Result<SocketAddr> {
        self.stream.local_addr()
    }

    pub(in crate::lan) fn connect(self) -> io::Result<TcpStream> {
        self.connect_using(start)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetLanPreparedDial",
            action = "IPv4",
            project = "aterm_net::lan::native::dial::tests::dial_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetLanPreparedDial",
            action = "IPv6",
            project = "aterm_net::lan::native::dial::tests::dial_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetLanPreparedDial",
            action = "Expired",
            project = "aterm_net::lan::native::dial::tests::dial_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetLanPreparedDial",
            action = "ChangedHops",
            project = "aterm_net::lan::native::dial::tests::dial_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NetLanPreparedDial",
            action = "WrongBinding",
            project = "aterm_net::lan::native::dial::tests::dial_transition"
        )
    )]
    fn connect_using(
        self,
        start: impl FnOnce(&TcpStream, SocketAddr) -> io::Result<()>,
    ) -> io::Result<TcpStream> {
        if Instant::now() >= self.until {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "LAN connection deadline expired before dialing",
            ));
        }
        socket_policy(&self.stream, Some(self.index), true)?;
        if let Err(error) = start(&self.stream, self.peer) {
            if !matches!(
                error.raw_os_error(),
                Some(libc::EINPROGRESS | libc::EINTR | libc::EALREADY)
            ) {
                return Err(error);
            }
            loop {
                let left = self.until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "LAN connection timed out",
                    ));
                }
                let mut fd = libc::pollfd {
                    fd: self.stream.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let millis = left.as_millis().min(i32::MAX as u128).max(1) as i32;
                // SAFETY: fd is the sole writable pollfd for the count one.
                let ready = unsafe { libc::poll(&raw mut fd, 1, millis) };
                if ready < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                if ready == 0 {
                    continue;
                }
                if let Some(error) = self.stream.take_error()? {
                    return Err(error);
                }
                if fd.revents & libc::POLLOUT == 0 {
                    return Err(refuse("LAN connection became unavailable"));
                }
                break;
            }
        }
        if self.stream.peer_addr()? != self.peer {
            return Err(refuse("LAN connection reached another address"));
        }
        self.stream.set_nonblocking(false)?;
        socket_policy(&self.stream, Some(self.index), true)?;
        Ok(self.stream)
    }
}

fn start(stream: &TcpStream, peer: SocketAddr) -> io::Result<()> {
    let address = Address::new(peer);
    let (pointer, length) = address.parts();
    // SAFETY: address owns a correctly sized, initialized sockaddr.
    if unsafe { libc::connect(stream.as_raw_fd(), pointer, length) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::super::restrict_hops;
    #[cfg(target_os = "macos")]
    use super::super::tcp::NO_SIGPIPE;
    use super::*;
    use std::{net::TcpListener, time::Duration};

    fn interface() -> (&'static str, u32) {
        let name = if cfg!(target_os = "macos") {
            "lo0"
        } else {
            "lo"
        };
        (name, super::super::index(name).unwrap())
    }

    pub fn dial_transition(
        state: &aterm_spec::interp::State,
        action: &str,
        bypass: bool,
    ) -> aterm_spec::interp::State {
        let (name, index) = interface();
        let (scenario, source) = match action {
            "IPv4" => (1, "127.0.0.1:0"),
            "IPv6" => (2, "[::1]:0"),
            "Expired" => (3, "127.0.0.1:0"),
            "ChangedHops" => (4, "127.0.0.1:0"),
            "WrongBinding" => (5, "127.0.0.1:0"),
            _ => panic!("unknown dial action"),
        };
        let listener = TcpListener::bind(source).unwrap();
        let peer = listener.local_addr().unwrap();
        let mut dial = Dial::new(
            source.parse().unwrap(),
            peer,
            index,
            name,
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap();
        if scenario == 3 {
            dial.until = Instant::now();
        }
        if scenario == 4 {
            super::super::set_option(&dial.stream, libc::IPPROTO_IP, libc::IP_TTL, 2).unwrap();
        }
        if scenario == 5 {
            dial.index = index.checked_add(1).unwrap();
        }
        let attempted = std::cell::Cell::new(false);
        if bypass {
            attempted.set(true);
            let _ = start(&dial.stream, peer);
        } else {
            let result = dial.connect_using(|stream, peer| {
                attempted.set(true);
                start(stream, peer)
            });
            assert_eq!(result.is_ok(), scenario <= 2);
        }
        let mut next = state.clone();
        next.insert("scenario", scenario);
        next.insert("attempted", i64::from(attempted.get()));
        next
    }

    #[test]
    fn native_connect_attempts_conform_and_detect_an_unguarded_syscall() {
        let model = aterm_models_core::lan_prepared_dial_model();
        assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
        for state in aterm_spec::interp::reachable_states(&model) {
            for action in &model.actions {
                if model.action_enabled(action.name, &state) {
                    assert!(
                        model
                            .successors(action.name, &state)
                            .contains(&dial_transition(&state, action.name, false))
                    );
                }
            }
        }
        let state = std::collections::BTreeMap::from([("scenario", 0), ("attempted", 0)]);
        let bad = dial_transition(&state, "ChangedHops", true);
        assert!(!model.successors("ChangedHops", &state).contains(&bad));
        assert!(!model.check_invariant("OnlyPreparedAttempts", &bad));
    }

    #[test]
    fn source_interface_and_hop_limit_are_fixed_before_ipv4_or_ipv6_connect() {
        let (name, index) = interface();
        for source in ["127.0.0.1:0", "[::1]:0"] {
            let listener = TcpListener::bind(source).unwrap();
            listener.set_nonblocking(true).unwrap();
            let peer = listener.local_addr().unwrap();
            let dial = Dial::new(
                source.parse().unwrap(),
                peer,
                index,
                name,
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            )
            .unwrap();
            let reserved = dial.local_addr().unwrap();
            assert_ne!(reserved.port(), 0);
            assert_eq!(reserved.ip(), peer.ip());
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            socket_policy(&dial.stream, Some(index), true).unwrap();
            #[cfg(target_os = "macos")]
            assert_eq!(
                super::super::option(&dial.stream, libc::SOL_SOCKET, NO_SIGPIPE).unwrap(),
                1
            );
            assert!(socket_policy(&dial.stream, None, true).is_err());
            let stream = dial.connect().unwrap();
            assert!(
                crate::slots::wait_listener_readable(&listener, aterm_spec::wait::MUST_HAPPEN)
                    .unwrap()
            );
            let (received, source) = listener.accept().unwrap();
            assert_eq!(source, reserved);
            assert_eq!(stream.local_addr().unwrap(), reserved);
            assert_eq!(stream.peer_addr().unwrap(), peer);
            assert_eq!(received.peer_addr().unwrap(), reserved);
            socket_policy(&stream, Some(index), true).unwrap();
        }
    }

    #[test]
    fn expired_deadline_and_changed_hop_limit_refuse_before_any_connect() {
        let (name, index) = interface();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let peer = listener.local_addr().unwrap();
        let make =
            |until| Dial::new("127.0.0.1:0".parse().unwrap(), peer, index, name, until).unwrap();
        assert_eq!(
            make(Instant::now()).connect().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        let dial = make(Instant::now() + Duration::from_secs(60));
        super::super::set_option(&dial.stream, libc::IPPROTO_IP, libc::IP_TTL, 2).unwrap();
        assert_eq!(
            dial.connect().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn expired_route_lookup_cannot_restart_the_deadline() {
        assert_eq!(
            super::super::route(None, "127.0.0.1:9".parse().unwrap(), None, Instant::now())
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn accepted_sockets_acquire_or_preserve_but_never_replace_their_binding() {
        let (name, index) = interface();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        super::super::ensure_binding(&accepted, index, name).unwrap();
        super::super::ensure_binding(&accepted, index, name).unwrap();
        restrict_hops(&accepted).unwrap();
        socket_policy(&accepted, Some(index), true).unwrap();
        assert!(
            super::super::ensure_binding(&accepted, index.checked_add(1).unwrap(), name).is_err()
        );
        socket_policy(&accepted, Some(index), true).unwrap();
    }
}
