// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Compare an anonymous fleet dial with plain TCP to the same explicit endpoint.
//! The fleet join-ask handshake can be unavailable on an older fleet listener.

#[cfg(unix)]
fn run() -> Result<(), String> {
    use aterm_net::fleet_member::{CONNECT_WITHIN, Unreached, dial_ask};
    use std::net::{SocketAddr, TcpStream};
    use std::time::{Duration, Instant};

    let mut args = std::env::args().skip(1);
    let peer: SocketAddr = args
        .next()
        .ok_or("usage: connection_probe IP:PORT [TIMEOUT_SECONDS]")?
        .parse()
        .map_err(|e| format!("endpoint: {e}"))?;
    let seconds = args
        .next()
        .map(|s| s.parse::<u64>())
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or(3);
    if args.next().is_some() || !(1..=60).contains(&seconds) {
        return Err("usage: connection_probe IP:PORT [TIMEOUT_SECONDS=1..60]".into());
    }
    // Respect the same diagnostic partition guard as the production connector.
    aterm_uds::partition::check(peer).map_err(|e| e.to_string())?;
    let within = Duration::from_secs(seconds);
    let tcp_within = within.min(CONNECT_WITHIN);
    println!(
        "endpoint={peer} overall_budget_ms={} connect_budget_ms={}",
        within.as_millis(),
        tcp_within.as_millis()
    );
    let (host, recorded) = match peer {
        SocketAddr::V6(address) if address.scope_id() != 0 => {
            (format!("{}%{}", address.ip(), address.scope_id()), None)
        }
        _ => (peer.ip().to_string(), Some(peer.ip())),
    };
    let started = Instant::now();
    match dial_ask(&host, recorded, peer.port(), within) {
        Ok((_transport, reached)) => println!(
            "fleet=connected endpoint={reached} elapsed_ms={}",
            started.elapsed().as_millis()
        ),
        Err(Unreached::Connect(at, error)) => println!(
            "fleet=connect-error endpoint={at} kind={:?} errno={:?} message={:?} elapsed_ms={}",
            error.kind(),
            error.raw_os_error(),
            error.to_string(),
            started.elapsed().as_millis()
        ),
        Err(error @ (Unreached::NotFleet(_) | Unreached::Local(_))) => {
            return Err(error.to_string());
        }
        Err(error) => println!(
            "fleet=other-error message={:?} elapsed_ms={}",
            error.to_string(),
            started.elapsed().as_millis()
        ),
    }
    let started = Instant::now();
    match TcpStream::connect_timeout(&peer, tcp_within) {
        Ok(stream) => println!(
            "tcp=connected local={} peer={} elapsed_ms={}",
            stream.local_addr().map_err(|e| e.to_string())?,
            peer,
            started.elapsed().as_millis()
        ),
        Err(error) => println!(
            "tcp=connect-error endpoint={peer} kind={:?} errno={:?} message={:?} elapsed_ms={}",
            error.kind(),
            error.raw_os_error(),
            error.to_string(),
            started.elapsed().as_millis()
        ),
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    #[cfg(unix)]
    {
        match run() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                std::process::ExitCode::from(2)
            }
        }
    }
    #[cfg(not(unix))]
    {
        eprintln!("connection_probe needs Unix");
        std::process::ExitCode::from(3)
    }
}
