// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private instance memory and scripted facts for observing the actual away-entry deadline.
//! Remembered history affects words only; the debug facts seam cannot script LAN admission.

use std::path::PathBuf;

use aterm_netplace::place::{Known, Stay, StayAt};
use aterm_netplace::{NetworkId, Record, RouteFacts, record, words};

use super::World;

pub(super) struct Fixture {
    pub facts: PathBuf,
    pub due_ms: u64,
}

pub(super) fn delay(value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .ok()
        .filter(|value| (1..=words::AWAY_ENTRY_AFTER_MS).contains(value))
        .ok_or_else(|| {
            format!(
                "instance: --network-away-in-ms takes 1..={} milliseconds",
                words::AWAY_ENTRY_AFTER_MS
            )
        })
}

const HOME: &str = "route en0 192.168.44.2/24 via 192.168.44.1 hw 02:00:00:00:00:01\n";
const REMOTE: &str = "route en0 172.20.10.2/28 via 172.20.10.1 hw 02:00:00:00:00:02\n";

fn stay(route: &RouteFacts, start_ms: u64, end_ms: Option<u64>, at: StayAt) -> Stay {
    let (network, prefix) = route.subnet();
    Stay {
        start_ms,
        end_ms,
        at,
        network: NetworkId::of(route),
        subnet: Some(format!("{network}/{prefix}")),
        gateway: route.gateway,
        from_start: false,
    }
}

pub(super) fn prepare(world: &World, delay_ms: u64, now: u64) -> Result<Fixture, String> {
    delay(&delay_ms.to_string())?;
    let dir = record::dir(&world.state());
    if dir.exists() {
        return Err("instance: network away fixture refuses existing network-place memory".into());
    }
    let home = aterm_netplace::facts::parse_script(HOME)
        .facts
        .route
        .ok_or("instance: home fixture has no route")?;
    let remote = aterm_netplace::facts::parse_script(REMOTE)
        .facts
        .route
        .ok_or("instance: remote fixture has no route")?;
    let start = now
        .checked_sub(words::AWAY_ENTRY_AFTER_MS - delay_ms)
        .ok_or("instance: network away fixture precedes the clock epoch")?;
    let home_start = start
        .checked_sub(60_000)
        .ok_or("instance: network away history precedes the clock epoch")?;
    let due_ms = start
        .checked_add(words::AWAY_ENTRY_AFTER_MS)
        .ok_or("instance: network away deadline exceeds the clock")?;
    let written = record::Written {
        revision: 1,
        written_ms: now,
        record: Record {
            stays: vec![
                stay(&home, home_start, Some(start), StayAt::AlabLan),
                stay(&remote, start, None, StayAt::Remote),
            ],
            remembered: vec![Known {
                network: NetworkId::of(&home).ok_or("instance: home fixture has no strong id")?,
                learned_ms: home_start,
                confirmed_ms: start,
            }],
        },
        ..record::Written::default()
    };
    let facts = world.dir.join("network-away.facts");
    crate::util::write_fresh(&facts, REMOTE).map_err(|error| error.to_string())?;
    record::write(&dir, &written).map_err(|error| error.to_string())?;
    Ok(Fixture { facts, due_ms })
}

#[cfg(test)]
#[path = "instance_network_away_tests.rs"]
mod tests;
