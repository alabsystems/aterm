// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Measure the fleet listener's local deny-list reader without starting a
//! listener, changing vault state or printing node identities. Consecutive
//! reads in one process include both cold and warmed-cache observations.

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use aterm_json::json;
    use aterm_secrets::client::{Paths, swarm_denied_nodes};
    use std::{io, time::Instant};

    let args: Vec<_> = std::env::args().skip(1).collect();
    let samples = match args.as_slice() {
        [] => 3,
        [count] => count.parse::<usize>()?,
        _ => return Err(io::Error::other("usage: fleet_policy_probe [1..16]").into()),
    };
    if !(1..=16).contains(&samples) {
        return Err(io::Error::other("samples must be between 1 and 16").into());
    }
    let paths = Paths::live()?;
    for sample in 1..=samples {
        let started = Instant::now();
        let result = swarm_denied_nodes(&paths);
        let elapsed_us = started.elapsed().as_micros();
        let denied = match result {
            Ok(denied) => denied,
            Err(_) => {
                println!(
                    "{}",
                    json!({"sample": sample, "elapsed_us": elapsed_us, "outcome": "failed"})
                );
                return Err(io::Error::other("fleet policy read failed").into());
            }
        };
        println!(
            "{}",
            json!({
                "sample": sample,
                "elapsed_us": elapsed_us,
                "outcome": "read",
                "available": denied.is_some(),
                "denied_nodes": denied.map(|nodes| nodes.len()),
            })
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("fleet_policy_probe requires Unix vault support");
    std::process::ExitCode::from(2)
}
