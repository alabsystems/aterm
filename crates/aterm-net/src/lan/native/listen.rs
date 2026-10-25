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
mod tests {
    use super::*;
    use std::cell::Cell;

    fn interface() -> (&'static str, u32) {
        let name = if cfg!(target_os = "macos") {
            "lo0"
        } else {
            "lo"
        };
        (name, super::super::index(name).unwrap())
    }

    pub fn listen_transition(
        state: &aterm_spec::interp::State,
        action: &str,
        bypass: bool,
    ) -> aterm_spec::interp::State {
        let (name, index) = interface();
        let (scenario, address) = match action {
            "IPv4" => (1, "127.0.0.1:0"),
            "IPv6" => (2, "[::1]:0"),
            "Expired" => (3, "127.0.0.1:0"),
            "ChangedHops" => (4, "127.0.0.1:0"),
            "WrongBinding" => (5, "127.0.0.1:0"),
            "StaleAttachment" => (6, "127.0.0.1:0"),
            "LoopbackIPv4" => (7, "127.0.0.1:0"),
            "LoopbackIPv6" => (8, "[::1]:0"),
            "IPv6ChangedHops" => (9, "[::1]:0"),
            "IPv6WrongBinding" => (10, "[::1]:0"),
            "WrongSource" => (11, "127.0.0.1:0"),
            "DualStack" => (12, "[::1]:0"),
            "IPv6Expired" => (13, "[::1]:0"),
            _ => panic!("unknown listen action"),
        };
        let mut prepared = Prepared::new(
            address.parse().unwrap(),
            if [7, 8].contains(&scenario) {
                None
            } else {
                Some((index, name))
            },
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap();
        if scenario == 3 || scenario == 13 {
            prepared.until = Instant::now();
        }
        if scenario == 4 {
            super::super::set_option(&prepared.stream, libc::IPPROTO_IP, libc::IP_TTL, 2).unwrap();
        }
        if scenario == 9 {
            super::super::set_option(
                &prepared.stream,
                libc::IPPROTO_IPV6,
                libc::IPV6_UNICAST_HOPS,
                2,
            )
            .unwrap();
        }
        if scenario == 5 || scenario == 10 {
            prepared.index = Some(index.checked_add(1).unwrap());
        }
        if scenario == 11 {
            prepared.local.set_port(0);
        }
        if scenario == 12 {
            prepared = dual_stack(index, name);
        }
        let attempted = Cell::new(false);
        if bypass {
            attempted.set(true);
            start(&prepared.stream).unwrap();
        } else {
            let result = prepared.listen_using(
                || {
                    if scenario == 6 {
                        Err(super::super::refuse("attachment changed"))
                    } else {
                        Ok(())
                    }
                },
                |stream| {
                    attempted.set(true);
                    start(stream)
                },
            );
            assert_eq!(
                result.is_ok(),
                scenario <= 2 || [7, 8].contains(&scenario),
                "{action}"
            );
        }
        let mut next = state.clone();
        next.insert("scenario", scenario);
        next.insert("attempted", i64::from(attempted.get()));
        next
    }

    #[test]
    fn listen_guard_conforms_and_detects_an_unguarded_syscall() {
        let model = aterm_models_core::lan_prepared_listen_model();
        assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
        for state in aterm_spec::interp::reachable_states(&model) {
            for action in &model.actions {
                if model.action_enabled(action.name, &state) {
                    assert!(
                        model
                            .successors(action.name, &state)
                            .contains(&listen_transition(&state, action.name, false))
                    );
                }
            }
        }
        let state = std::collections::BTreeMap::from([("scenario", 0), ("attempted", 0)]);
        for action in [
            "ChangedHops",
            "StaleAttachment",
            "IPv6ChangedHops",
            "IPv6WrongBinding",
            "WrongSource",
            "DualStack",
            "IPv6Expired",
        ] {
            let bad = listen_transition(&state, action, true);
            assert!(!model.successors(action, &state).contains(&bad));
            assert!(!model.check_invariant("OnlyPreparedAttempts", &bad));
        }
    }

    fn dual_stack(index: u32, name: &str) -> Prepared {
        let stream = TcpStream::from(
            super::super::Socket::of_kind(libc::AF_INET6, libc::SOCK_STREAM, libc::IPPROTO_TCP)
                .unwrap()
                .0,
        );
        #[cfg(target_os = "macos")]
        super::super::set_option(&stream, libc::SOL_SOCKET, super::super::tcp::NO_SIGPIPE, 1)
            .unwrap();
        super::super::set_option(&stream, libc::IPPROTO_IPV6, libc::IPV6_V6ONLY, 0).unwrap();
        super::super::bind_interface(&stream, index, name).unwrap();
        super::super::restrict_hops(&stream).unwrap();
        // Linux can make a socket IPv6-only when bound to a specific IPv6
        // address. Keep this deliberate invalid fixture dual-stack on both OSes.
        let address = super::super::tcp::Address::new("[::]:0".parse().unwrap());
        let (pointer, length) = address.parts();
        // SAFETY: address owns the initialized sockaddr for this complete call.
        assert_eq!(
            unsafe { libc::bind(stream.as_raw_fd(), pointer, length) },
            0
        );
        let local = stream.local_addr().unwrap();
        assert_eq!(
            super::super::option(&stream, libc::IPPROTO_IPV6, libc::IPV6_V6ONLY).unwrap(),
            0
        );
        Prepared {
            stream,
            local,
            index: Some(index),
            until: Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        }
    }

    #[test]
    fn kernel_listener_and_accepted_sockets_keep_the_prepared_policy() {
        let (name, index) = interface();
        let sources: &[&str] = if cfg!(target_os = "macos") {
            &["127.0.0.1:0"]
        } else {
            &["127.0.0.1:0", "[::1]:0"]
        };
        for source in sources {
            let prepared = Prepared::new(
                source.parse().unwrap(),
                Some((index, name)),
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            )
            .unwrap();
            let address = prepared.stream.local_addr().unwrap();
            // Linux exposes this kernel state directly; Darwin returns
            // ENOPROTOOPT. Do not probe a closed TCB with a timed-out connect.
            #[cfg(target_os = "linux")]
            assert_eq!(
                super::super::option(&prepared.stream, libc::SOL_SOCKET, libc::SO_ACCEPTCONN)
                    .unwrap(),
                0
            );
            let listener = prepared.listen(|| Ok(())).unwrap();
            #[cfg(target_os = "linux")]
            assert_eq!(
                super::super::option(&listener, libc::SOL_SOCKET, libc::SO_ACCEPTCONN).unwrap(),
                1
            );
            listener_policy(&listener, Some(index)).unwrap();
            let _peer =
                TcpStream::connect_timeout(&address, aterm_spec::wait::MUST_HAPPEN).unwrap();
            assert!(
                crate::slots::wait_listener_readable(&listener, aterm_spec::wait::MUST_HAPPEN)
                    .unwrap()
            );
            let (stream, _) = listener.accept().unwrap();
            socket_policy(&stream, Some(index), true).unwrap();
            #[cfg(target_os = "macos")]
            assert_eq!(
                super::super::option(&stream, libc::SOL_SOCKET, super::super::tcp::NO_SIGPIPE)
                    .unwrap(),
                1
            );
        }
    }
}
