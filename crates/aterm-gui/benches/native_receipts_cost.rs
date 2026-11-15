// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified bench -p aterm-gui --profile opt-test --bench native_receipts_cost`.
//! Use `--profile test` for the same comparison without optimization.
//! Manual measurement with no timing assertion or live terminal state. Both algorithms
//! validate the same complete history; indexed measurements include building the index.

#[cfg(unix)]
#[path = "../src/resource_jobs_receipts.rs"]
mod receipts;

#[cfg(unix)]
fn linear(
    previous: &std::collections::HashMap<String, (u64, u64)>,
    current: &[aterm_resource_host::resource_observation::SessionUsage],
) -> bool {
    previous.iter().any(|(session, &(cpu, unread))| {
        current
            .iter()
            .find(|receipt| &receipt.session == session)
            .is_none_or(|receipt| receipt.reaped_cpu_ns < cpu || receipt.unread < unread)
    })
}

#[cfg(unix)]
fn measure(name: &str, sessions: usize, rounds: usize, mut check: impl FnMut() -> bool) {
    use aterm_bench::measurement_clock::{counters, thread_cpu_ns};
    use std::hint::black_box;

    let before = counters();
    let cpu_before = thread_cpu_ns();
    let start = std::time::Instant::now();
    for _ in 0..rounds {
        black_box(check());
    }
    let wall = start.elapsed().as_nanos();
    let cpu = cpu_before
        .zip(thread_cpu_ns())
        .and_then(|(a, b)| b.checked_sub(a));
    let instructions = before.zip(counters()).map(|(a, b)| b.since(a).instructions);
    println!(
        "sessions={sessions} algorithm={name} rounds={rounds} ns_per_round={} cpu_ns_per_round={:?} instructions_per_round={:?}",
        wall / rounds as u128,
        cpu.map(|n| n / rounds as u64),
        instructions.map(|n| n / rounds as u64),
    );
}

#[cfg(unix)]
fn main() {
    use aterm_resource_host::resource_observation::SessionUsage;
    use std::collections::HashMap;
    use std::hint::black_box;

    for sessions in [64usize, 256, 1024, 4096] {
        let current: Vec<_> = (0..sessions)
            .rev()
            .map(|n| SessionUsage {
                session: format!("s-{n:020x}"),
                reaped_cpu_ns: (n as u64 + 1) * 1_000_000,
                unread: 0,
            })
            .collect();
        let previous: HashMap<_, _> = current
            .iter()
            .map(|receipt| {
                (
                    receipt.session.clone(),
                    (receipt.reaped_cpu_ns, receipt.unread),
                )
            })
            .collect();
        let indexed = || {
            receipts::Index::new(black_box(current.as_slice()))
                .unwrap()
                .regresses(black_box(&previous))
        };
        let reference = || linear(black_box(&previous), black_box(current.as_slice()));
        assert!(!reference());
        assert!(!indexed());
        let rounds = (16_777_216 / (sessions * sessions)).clamp(1, 64);
        measure("linear", sessions, rounds, reference);
        measure("indexed", sessions, rounds, indexed);
    }
}

#[cfg(not(unix))]
fn main() {
    println!("unavailable: native resource observations require Unix");
}
