// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The converge runner's readings of its own process (`Ports::spent` on
//! [`crate::converge::Live`]), read around real port runs in a process of their own, where no
//! other test's runs or CPU land.

use aterm_machines::port::{Cmd, System as _};
use aterm_machines::sync::Ports as _;
use std::time::{Duration, Instant};

/// The part of the law this process plays: unset for the test itself, [`PROBE`] for the
/// private process that reads the runner, [`SPIN`] for the child that spends CPU.
const ROLE: &str = "ATERM_TEST_CONVERGE_SPENT";
const PROBE: &str = "probe";
const SPIN: &str = "spin";
/// This law's name, by which its test binary runs it again.
const NAME: &str =
    "converge::tests::spent::a_pass_reads_its_ports_runs_and_cpu_each_in_its_own_field";
/// The CPU the spending child runs up, by its own `getrusage`, before it ends.
const SPIN_MS: u64 = 100;

/// This test binary run again as `role`, for this law alone and on one thread.
fn this_law(role: &str) -> Cmd {
    Cmd::new(std::env::current_exe().unwrap())
        .args(["--exact", NAME, "--test-threads=1", "--nocapture"])
        .env(ROLE, role)
}

/// A PASS READS WHAT ITS PORTS STARTED AND WHAT ITS CHILDREN SPENT, EACH IN ITS OWN FIELD: the
/// runner's `Ports::spent`, read before and after two runs through fresh
/// `aterm_machines::port::Live`s (a program named `git`, then a child that runs up
/// [`SPIN_MS`] of CPU by its own `getrusage` before it ends), rises by exactly those two in
/// `spawns` and the one `git` in `gits`, by at least [`SPIN_MS`] in `child_cpu_ms`, and by less
/// than that in `cpu_ms`, this process's own, which only waited. It runs in a process of its
/// own, so no other test's runs or CPU land in the readings. No wait is a budget: the child
/// spins only until its own counter passes the mark, and its counter standing still for a
/// minute fails it; the private process's port runs take `Duration::MAX` as their budget,
/// which the port's `deadline_after` turns into a deadline a year away, far past the
/// one-minute hang detectors here; and the test waits on the private process for as long as
/// its tree makes progress, failing it after a minute without any.
/// NEGATIVE CONTROLS: `let (gits, spawns) = …runs()` in `Ports::spent` reads `(1, 2)` where
/// `(2, 1)` is wanted; `let (child_cpu_ms, cpu_ms) = …cpu_ms()` reads `child_cpu_ms: 3,
/// cpu_ms: 101`, the child's CPU in this process's field, where at least [`SPIN_MS`] is wanted
/// in `child_cpu_ms`.
#[test]
fn a_pass_reads_its_ports_runs_and_cpu_each_in_its_own_field() {
    match std::env::var(ROLE).as_deref() {
        Ok(SPIN) => spin(),
        Ok(PROBE) => probe(),
        _ => {
            let probe = this_law(PROBE);
            let mut command = aterm_uds::spawnfd::command(&probe.program);
            command.args(&probe.args).env(ROLE, PROBE);
            let output = aterm_uds::deadline::output_while_process_progressing(
                &mut command,
                aterm_spec::wait::MUST_HAPPEN,
                "the converge runner's readings around two port runs",
            )
            .unwrap_or_else(|error| panic!("the private process: {error}"));
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("spent read"),
                "the private process {:?}; stdout: {}; stderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

/// The private process: the runner's readings around a `git` and a spending child.
fn probe() {
    let runner = crate::converge::Live::here();
    let dir = aterm_tempfile::tempdir().unwrap();
    let git = dir.path().join("git");
    std::os::unix::fs::symlink("/usr/bin/true", &git).unwrap();
    let before = runner.spent();
    let ran = aterm_machines::port::Live::new()
        .run(&Cmd::new(&git).budget(Duration::MAX))
        .unwrap();
    assert_eq!(ran.status, Some(0), "{ran:?}");
    let spun = aterm_machines::port::Live::new()
        .run(&this_law(SPIN).budget(Duration::MAX))
        .unwrap();
    assert!(
        spun.status == Some(0) && String::from_utf8_lossy(&spun.stdout).contains("spun"),
        "the spending child {:?}; stdout: {}; stderr: {}",
        spun.status,
        String::from_utf8_lossy(&spun.stdout),
        String::from_utf8_lossy(&spun.stderr)
    );
    let spent = runner.spent().since(before);
    assert_eq!(
        (spent.spawns, spent.gits),
        (2, 1),
        "two runs, one a git: {spent:?}"
    );
    assert!(
        spent.child_cpu_ms >= SPIN_MS,
        "the child's {SPIN_MS} ms are in child_cpu_ms: {spent:?}"
    );
    assert!(
        spent.cpu_ms < SPIN_MS,
        "this process only waited, so the child's CPU is not in cpu_ms: {spent:?}"
    );
    println!("spent read: {spent:?}");
}

/// The spending child: CPU until its own `getrusage` passes [`SPIN_MS`].
fn spin() {
    let mut seen = (0, Instant::now());
    let mut sum = 0_u64;
    loop {
        let (own, _) = aterm_machines::sync::cost::cpu_ms();
        if own >= SPIN_MS {
            break;
        }
        if own != seen.0 {
            seen = (own, Instant::now());
        }
        assert!(
            seen.1.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "this child's own CPU stood at {own} ms for a minute"
        );
        for step in 0..100_000_u64 {
            sum = std::hint::black_box(sum.wrapping_mul(31).wrapping_add(step));
        }
    }
    println!("spun ({sum})");
}
