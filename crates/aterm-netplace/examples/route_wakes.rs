// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Read-only census of what the network-change socket costs a waiter at rest:
//! `route_wakes [duration_ms]` (default 600000). It opens
//! [`aterm_uds::network::RouteChanges`] and waits on it as `RouteChanges::wait_beside` does,
//! sorting each readiness by what [`aterm_uds::network::RouteChanges::read_waiting`] made of it:
//! a change, which ends a waiter's wait once its burst is read; a read that found no change,
//! which costs the waiter a read and no wake; or a failed read, which ends the census. Counts
//! only; it records no message.

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::fd::AsFd as _;
    use std::time::{Duration, Instant};

    let duration = std::env::args()
        .nth(1)
        .map_or(Ok(600_000), |arg| arg.parse::<u64>())?;
    let routes = aterm_uds::network::RouteChanges::open()?;
    let start = Instant::now();
    let deadline = start
        .checked_add(Duration::from_millis(duration))
        .ok_or("duration exceeds the monotonic clock range")?;
    let (mut changes, mut quiet, mut failed) = (0_u64, 0_u64, false);
    let mut seconds = std::collections::BTreeSet::new();
    while !aterm_uds::ready::any_until(&[routes.as_fd()], deadline)?.is_empty() {
        match routes.read_waiting() {
            Some(true) => {
                changes += 1;
                seconds.insert(start.elapsed().as_secs());
            }
            Some(false) => quiet += 1,
            None => {
                // A failed socket polls ready again at once: stop rather than spin.
                failed = true;
                break;
            }
        }
    }
    println!(
        "route_wakes elapsed_ms={} change_wakes={changes} quiet_reads={quiet} \
         seconds_with_change={} failed={failed}",
        start.elapsed().as_millis(),
        seconds.len()
    );
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("route_wakes requires macOS or Linux".into())
}
