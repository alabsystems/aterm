// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::startup_worker_tests::{legs, probe};
use super::{
    Startup, StartupAttachMilestones, StartupGpuSample, birth_to_main_ns, close_startup_gpu,
    derive_startup_worker,
};
use aterm_uds::process::{BootClock, ProcessBirth, ProcessStart};
use std::time::{Duration, Instant, UNIX_EPOCH};

#[test]
fn facts_publish_once_every_input_exists() {
    let ledger = Startup::new();
    let unset_probe = StartupGpuSample::default;

    let birth = ProcessStart {
        boot: BootClock(0),
        wall: Some(ProcessBirth {
            seconds: 1_792_118_934,
            microseconds: 927_250,
        }),
    };
    let main = UNIX_EPOCH + Duration::from_secs(1_792_118_936) + Duration::from_micros(857_000);
    let facts = ledger.facts_from(
        || unreachable!("asked the kernel with no Rust-main stamp"),
        unset_probe,
    );
    assert_eq!(facts.birth_to_main_ns, None);
    let _ = ledger.rust_main_wall.set(main);
    assert_eq!(
        ledger.facts_from(|| None, unset_probe).birth_to_main_ns,
        None
    );
    assert_eq!(
        ledger.birth_to_main.get(),
        None,
        "an unreadable record is read again"
    );
    let fresh = birth_to_main_ns(birth, None, main, None);
    assert!(fresh.is_some());
    assert_eq!(
        ledger
            .facts_from(|| Some(birth), unset_probe)
            .birth_to_main_ns,
        fresh
    );
    assert_eq!(ledger.birth_to_main.get().copied(), fresh);
    assert_eq!(
        ledger
            .facts_from(|| unreachable!("asked the kernel again"), unset_probe)
            .birth_to_main_ns,
        fresh
    );

    let spawn = Instant::now();
    let join_entry = spawn + Duration::from_millis(20);
    let done = join_entry + Duration::from_millis(10);
    let join_exit = done + Duration::from_millis(1);
    let _ = ledger.worker_spawn.set(spawn);
    let _ = ledger.worker_done.set(done);
    let _ = ledger.worker_legs.set(legs());
    let facts = ledger.facts_from(|| None, probe);
    assert!(!facts.worker.valid);
    assert_eq!(ledger.worker.get(), None);
    assert!(!facts.gpu.valid);
    assert_eq!(ledger.gpu.get(), None);
    let _ = ledger.attach.set(StartupAttachMilestones::new([
        spawn, spawn, spawn, join_entry, join_exit, join_exit, join_exit,
    ]));
    let worker = derive_startup_worker(
        Some(spawn),
        Some(done),
        Some(legs()),
        Some(join_entry),
        Some(join_exit),
    );
    assert!(worker.valid);
    let facts = ledger.facts_from(|| None, unset_probe);
    assert_eq!(facts.worker, worker);
    assert_eq!(ledger.worker.get(), Some(&worker));
    assert!(!facts.gpu.valid);
    assert_eq!(ledger.gpu.get(), None);
    let in_flight = || StartupGpuSample {
        assemble_ns: 0,
        ..probe()
    };
    assert!(!ledger.facts_from(|| None, in_flight).gpu.valid);
    assert_eq!(ledger.gpu.get(), None);

    let gpu = close_startup_gpu(probe(), worker.gpu_build_ns);
    assert!(gpu.valid);
    assert_eq!(ledger.facts_from(|| None, probe).gpu, gpu);
    assert_eq!(ledger.gpu.get(), Some(&gpu));
    assert_eq!(
        ledger
            .facts_from(|| None, || unreachable!("read the probe again"))
            .gpu,
        gpu
    );
}
