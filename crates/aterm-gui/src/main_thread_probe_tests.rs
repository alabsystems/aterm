// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_walk_follows_frame_records_upward_and_stops_cleanly() {
    let mem = |fp: u64| match fp {
        0x1000 => Some([0x1100, 0xA1]),
        0x1100 => Some([0x1200, 0xA2]),
        0x1200 => Some([0, 0xA3]),
        _ => None,
    };
    let mut out = [0u64; MAX_FRAMES];
    let n = walk(0xA0, 0x1000, mem, |a| a, &mut out);
    assert_eq!(&out[..n], &[0xA0, 0xA1, 0xA2, 0xA3]);
    let looped = |_fp: u64| Some([0x1000, 0xB1]);
    let n = walk(0xB0, 0x1000, looped, |a| a, &mut out);
    assert_eq!(&out[..n], &[0xB0, 0xB1]);
    assert_eq!(walk(0xC0, 0x1003, mem, |a| a, &mut out), 1);
    assert_eq!(walk(0xC0, 0x9000, mem, |a| a, &mut out), 1);
    let signed = |fp: u64| (fp == 0x1000).then_some([0, 0x00AB_0000_0000_00A1]);
    let n = walk(
        0x0012_0000_0000_00A0,
        0x1000,
        signed,
        |a| a & 0xFFFF,
        &mut out,
    );
    assert_eq!(&out[..n], &[0xA0, 0xA1]);
}

#[cfg(target_os = "macos")]
#[test]
fn the_probe_reads_a_spinning_threads_cpu_and_stack() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let stop = Arc::new(AtomicBool::new(false));
    let registered = Arc::new(AtomicBool::new(false));
    let (s, r) = (stop.clone(), registered.clone());
    let spinner = std::thread::spawn(move || {
        register();
        r.store(true, Ordering::SeqCst);
        let mut x = 0u64;
        while !s.load(Ordering::Relaxed) {
            x = std::hint::black_box(x.wrapping_add(1));
        }
        x
    });
    while !registered.load(Ordering::SeqCst) {
        aterm_uds::deadline::poll_yield();
    }
    let before = cpu_time().expect("thread_info on the registered thread");
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let mut used = Duration::ZERO;
    while used < Duration::from_millis(50) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
        used = cpu_time().expect("thread_info") - before;
    }
    let lines = stack().expect("a stack for the registered thread");
    stop.store(true, Ordering::Relaxed);
    let _ = spinner.join();
    imp::unregister();
    assert!(
        used >= Duration::from_millis(50),
        "a spinning thread's CPU time advances: {used:?}"
    );
    assert!(lines[0].starts_with("load address 0x"), "{lines:?}");
    assert!(
        lines.len() >= 3,
        "a frame chain, not just the pc: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("libsystem_pthread.dylib")),
        "the spawned thread's root frames are pthread's: {lines:?}"
    );
}
