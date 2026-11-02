// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Measure live IPv4 multicast attachment checks without sending a datagram.
//! Usage: lan_recheck SOURCE_IPV4 [SAMPLES=128]. Timings are not pass thresholds.

#[cfg(unix)]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use aterm_net::lan::{Multicast, Scope};
    use std::{net::Ipv4Addr, time::Instant};

    let mut args = std::env::args().skip(1);
    let source: Ipv4Addr = args
        .next()
        .ok_or("usage: lan_recheck SOURCE_IPV4 [SAMPLES=128]")?
        .parse()?;
    let count: usize = args
        .next()
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(128);
    if args.next().is_some() || !(1..=1024).contains(&count) {
        return Err("samples must be between 1 and 1024".into());
    }
    let interfaces = aterm_uds::network::interfaces()?;
    let matching = interfaces
        .iter()
        .filter(|entry| entry.address == std::net::IpAddr::V4(source))
        .count();
    let socket = Multicast::bind(source, Ipv4Addr::new(239, 255, 31, 85), 0, Scope::Lan)?;
    for _ in 0..8 {
        socket.recheck()?;
    }
    let mut samples = Vec::with_capacity(count);
    for _ in 0..count {
        let started = Instant::now();
        socket.recheck()?;
        samples.push(started.elapsed().as_nanos());
    }
    let total: u128 = samples.iter().sum();
    samples.sort_unstable();
    let percentile = |percent: usize| samples[(count * percent).div_ceil(100) - 1] as f64 / 1000.0;
    println!(
        "source={source} interface_addresses={} matching_addresses={matching} n={count} \
         total_us={:.3} p50_us={:.3} p95_us={:.3} p99_us={:.3}",
        interfaces.len(),
        total as f64 / 1000.0,
        percentile(50),
        percentile(95),
        percentile(99),
    );
    Ok(())
}

fn main() -> std::process::ExitCode {
    #[cfg(unix)]
    {
        match run() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("lan_recheck: {error}");
                std::process::ExitCode::from(2)
            }
        }
    }
    #[cfg(not(unix))]
    {
        eprintln!("lan_recheck requires the Unix LAN attachment API");
        std::process::ExitCode::from(2)
    }
}
