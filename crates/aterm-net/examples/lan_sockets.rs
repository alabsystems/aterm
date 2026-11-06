// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One-shot socket diagnostics without a vault, configuration change or repair.
//! Usage: `lan_sockets [--loopback] <239.255.x.x> <port>`.
//! Each source sends one opaque datagram, never a discovery beacon or peer dial.

use std::process::ExitCode;

#[cfg(unix)]
fn main() -> ExitCode {
    match probe::run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(2),
        Err(error) => {
            probe::failure("-", "preflight", &error);
            ExitCode::from(2)
        }
    }
}

#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("lan_sockets: the physical LAN attachment API is unavailable on this platform");
    ExitCode::from(2)
}

#[cfg(unix)]
mod probe {
    use aterm_net::lan::{Listener, Multicast, Scope};
    use std::{
        fmt::Display,
        io,
        net::{Ipv4Addr, SocketAddr},
    };

    const MAX_SOURCES: usize = 32;
    const USAGE: &str = "usage: lan_sockets [--loopback] <239.255.x.x> <port>";

    pub(super) fn failure(source: impl Display, stage: &str, error: &io::Error) {
        let detail: String = error.to_string().chars().take(160).collect();
        println!(
            "source={source} stage={stage} result=failed kind={:?} errno={:?} detail={detail:?}",
            error.kind(),
            error.raw_os_error(),
        );
    }

    pub(super) fn run() -> io::Result<bool> {
        let args: Vec<_> = std::env::args().skip(1).collect();
        let (scope, args) = match args.as_slice() {
            [flag, rest @ ..] if flag == "--loopback" => (Scope::Loopback, rest),
            rest => (Scope::Lan, rest),
        };
        let [group, port] = args else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, USAGE));
        };
        let group: Ipv4Addr = group
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, USAGE))?;
        let port: u16 = port
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, USAGE))?;
        if group.octets()[..2] != [239, 255] {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, USAGE));
        }
        let sources: Vec<SocketAddr> = match scope {
            Scope::Loopback => vec![(Ipv4Addr::LOCALHOST, 0).into()],
            Scope::Lan => Listener::candidates()?
                .into_iter()
                .filter(SocketAddr::is_ipv4)
                .collect(),
        };
        if sources.len() > MAX_SOURCES {
            return Err(io::Error::other(
                "eligible IPv4 sources exceed the probe's bound",
            ));
        }
        println!(
            "mode=socket-probe scope={scope:?} group={group} port={port} sources={}",
            sources.len()
        );
        let mut ready = 0;
        for address in &sources {
            let std::net::IpAddr::V4(source) = address.ip() else {
                continue;
            };
            let listener = match Listener::bind(*address, scope) {
                Ok(listener) => {
                    println!(
                        "source={source} stage=tcp-bind result=bound local={:?}",
                        listener.local_addr()?
                    );
                    Some(listener)
                }
                Err(error) => {
                    failure(source, "tcp-bind", &error);
                    None
                }
            };
            match Multicast::bind(source, group, port, scope) {
                Ok(socket) => {
                    println!(
                        "source={source} stage=multicast-bind result=bound port={}",
                        socket.port()
                    );
                    match socket.send(b"aterm-lan-socket-probe") {
                        Ok(()) => {
                            println!("source={source} stage=multicast-send result=sent");
                            ready += usize::from(listener.is_some());
                        }
                        Err(error) => failure(source, "multicast-send", &error),
                    }
                }
                Err(error) => failure(source, "multicast-bind", &error),
            }
        }
        println!(
            "outcome={} ready_sources={ready} sources={} peer_delivery_observed=false repair_started=false",
            if ready == 0 {
                "unavailable"
            } else {
                "sockets-ready"
            },
            sources.len()
        );
        Ok(ready != 0)
    }
}
