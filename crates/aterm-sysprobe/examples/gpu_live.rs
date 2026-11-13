// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GPU on this host, read `--reads <n>` times (default 10) `--every <ms>` apart (default
//! 1000): each read's device-wide reading and what the read cost, then the clients that ran
//! across the whole run, busiest first, and the throttle level. For holding the reader to the
//! OS's own monitor (Task Manager's GPU graph, Activity Monitor's GPU history) by eye.

use std::collections::HashMap;
use std::time::{Duration, Instant};

fn arg(args: &[String], name: &str, default: u64) -> u64 {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|n| n.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let reads = arg(&args, "--reads", 10).max(1);
    let every = Duration::from_millis(arg(&args, "--every", 1000));

    let at = Instant::now();
    let first = aterm_sysprobe::gpu();
    println!("first read  {:>9.3?}  {first:?}", at.elapsed());
    println!("driver      {:?}", aterm_sysprobe::gpu_driver());
    let at = Instant::now();
    let before = aterm_sysprobe::gpu_clients();
    println!(
        "clients     {:>9.3?}  {:?} rows",
        at.elapsed(),
        before.as_ref().map(|c| c.rows.len())
    );
    let started = Instant::now();
    let mut costs = Vec::new();
    for _ in 0..reads {
        std::thread::sleep(every);
        let at = Instant::now();
        let read = aterm_sysprobe::gpu();
        let cost = at.elapsed();
        costs.push(cost);
        match read {
            Some(g) => println!(
                "read        {cost:>9.3?}  busy={:?}% in_use={:?} alloc={:?} name={:?} unread={:?}",
                g.device_pct, g.in_use_bytes, g.alloc_bytes, g.name, g.unread
            ),
            None => println!("read        {cost:>9.3?}  None"),
        }
    }
    costs.sort_unstable();
    println!(
        "cost        median {:?}, max {:?} over {} reads",
        costs[costs.len() / 2],
        costs[costs.len() - 1],
        costs.len()
    );
    let window = started.elapsed();
    if let (Some(before), Some(after)) = (before, aterm_sysprobe::gpu_clients()) {
        let then: HashMap<u64, u64> = before.rows.iter().map(|r| (r.entry, r.gpu_ns)).collect();
        let mut ran: HashMap<u32, (String, u64)> = HashMap::new();
        for r in &after.rows {
            let d = r
                .gpu_ns
                .saturating_sub(then.get(&r.entry).copied().unwrap_or(r.gpu_ns));
            let e = ran.entry(r.pid).or_insert_with(|| (r.name.clone(), 0));
            e.1 += d;
        }
        let mut top: Vec<(u64, u32, String)> = ran
            .into_iter()
            .filter(|(_, (_, ns))| *ns > 0)
            .map(|(pid, (name, ns))| (ns, pid, name))
            .collect();
        top.sort_unstable_by(|a, b| b.cmp(a));
        println!("clients that ran over {window:.1?} (share of one engine):");
        for (ns, pid, name) in top.iter().take(8) {
            println!(
                "  pid={pid:<7} {:>5.1}%  {name}",
                *ns as f64 / window.as_nanos() as f64 * 100.0
            );
        }
    }
    let mut probe = aterm_sysprobe::Probe::new();
    let at = Instant::now();
    let throttle = probe.throttle();
    println!("throttle    {:>9.3?}  {throttle:?}", at.elapsed());
    let at = Instant::now();
    let throttle = probe.throttle();
    println!("throttle    {:>9.3?}  {throttle:?}", at.elapsed());
}
