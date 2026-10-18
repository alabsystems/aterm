// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT A SPAWN COSTS A PROCESS SHAPED LIKE THE WINDOW — a benchmark, not a
//! gate: it maps 800 MB and prints its numbers.
//!
//! This process is shaped like the window (about 6 400 VM regions, 400 MB
//! resident), then times the `spawn()` call and the parent's bill after it
//! (rewriting its resident memory once: the copy-on-write faults a fork
//! leaves) for a plain `std` spawn (the `posix_spawn` baseline), a forked
//! `std` + strip ([`InheritNothing`]), and [`command`]. Run it by hand:
//!
//! ```text
//! targo --unverified bench -p aterm-uds --bench spawn_cost
//! ```
//!
//! macOS only (the region walk is `PROC_PIDREGIONINFO`); elsewhere it says so.
//! A bench binary is its own fresh process, so the shape is this process's.

#[cfg(target_vendor = "apple")]
mod bench {
    use aterm_uds::spawnfd::{InheritNothing as _, command};
    use std::time::{Duration, Instant};

    /// The window's shape, measured: 6 585 VM regions and about 400 MB
    /// resident. The bench maps `CHUNKS` chunks of `CHUNK` bytes and makes
    /// every other one inaccessible, so each is a region of its own; the
    /// writable half is touched resident.
    const CHUNK: usize = 128 * 1024;
    const CHUNKS: usize = 6_400;
    /// Spawns timed per lane, and rewrites timed after a spawn.
    const SPAWNS: usize = 40;
    const REWRITES: usize = 9;

    /// How many VM regions this process has (`PROC_PIDREGIONINFO`, walked).
    fn region_count() -> usize {
        unsafe extern "C" {
            fn proc_pidinfo(
                pid: i32,
                flavor: i32,
                arg: u64,
                buffer: *mut core::ffi::c_void,
                size: i32,
            ) -> i32;
            fn getpid() -> i32;
        }
        const PROC_PIDREGIONINFO: i32 = 7;
        // `struct proc_regioninfo`: 96 bytes, `pri_address` at 80, `pri_size` at 88.
        let mut info = [0u8; 96];
        let (mut at, mut count) = (0u64, 0usize);
        loop {
            // SAFETY: `info` is 96 writable bytes, the size passed.
            let got = unsafe {
                proc_pidinfo(
                    getpid(),
                    PROC_PIDREGIONINFO,
                    at,
                    info.as_mut_ptr().cast(),
                    96,
                )
            };
            if got != 96 {
                return count;
            }
            let word = |o: usize| u64::from_ne_bytes(info[o..o + 8].try_into().expect("8 bytes"));
            count += 1;
            at = word(80) + word(88);
        }
    }

    /// The p50 and p90 of `samples`, in milliseconds.
    fn percentiles(mut samples: Vec<Duration>) -> (f64, f64) {
        samples.sort_unstable();
        let at = |q: usize| samples[(samples.len() - 1) * q / 100].as_secs_f64() * 1e3;
        (at(50), at(90))
    }

    pub fn run() {
        unsafe extern "C" {
            fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64)
            -> *mut u8;
            fn mprotect(addr: *mut u8, len: usize, prot: i32) -> i32;
            fn getpagesize() -> i32;
        }
        const PROT_NONE: i32 = 0;
        const PROT_READ_WRITE: i32 = 1 | 2;
        const MAP_PRIVATE_ANON: i32 = 0x0002 | 0x1000;
        let len = CHUNK * CHUNKS;
        // SAFETY: an anonymous private mapping this function alone uses and
        // never unmaps (the process exits with it).
        let base = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                PROT_READ_WRITE,
                MAP_PRIVATE_ANON,
                -1,
                0,
            )
        };
        assert!(!base.is_null() && base as isize != -1, "mmap");
        // SAFETY: `getpagesize` takes nothing and cannot fail.
        let page = usize::try_from(unsafe { getpagesize() }).expect("a page size");
        let writable: Vec<*mut u8> = (0..CHUNKS)
            .step_by(2)
            // SAFETY: every chunk start lies inside the mapping.
            .map(|i| unsafe { base.add(i * CHUNK) })
            .collect();
        for i in (1..CHUNKS).step_by(2) {
            // SAFETY: a chunk of the mapping; made inaccessible, splitting it off.
            assert_eq!(
                unsafe { mprotect(base.add(i * CHUNK), CHUNK, PROT_NONE) },
                0,
                "mprotect"
            );
        }
        let rewrite = || {
            let started = Instant::now();
            for &chunk in &writable {
                for offset in (0..CHUNK).step_by(page) {
                    // SAFETY: inside a writable chunk of the mapping.
                    unsafe { chunk.add(offset).write_volatile(1) };
                }
            }
            started.elapsed()
        };
        rewrite();
        let regions = region_count();
        let resident_mb = writable.len() * CHUNK / (1024 * 1024);
        println!("regions={regions} resident={resident_mb}MB page={page}");
        let baseline = percentiles((0..REWRITES).map(|_| rewrite()).collect());
        println!(
            "rewrite with no spawn: p50 {:.2} ms p90 {:.2} ms",
            baseline.0, baseline.1
        );
        type Spawn = Box<dyn Fn() -> Box<dyn FnOnce()>>;
        let lanes: [(&str, Spawn); 3] = [
            (
                "std Command, no hook (posix_spawn baseline)",
                Box::new(|| {
                    #[expect(
                        clippy::disallowed_types,
                        reason = "the measured baseline: std's own spawn"
                    )]
                    let mut child = std::process::Command::new("/usr/bin/true")
                        .spawn()
                        .expect("spawn");
                    Box::new(move || {
                        child.wait().expect("wait");
                    })
                }),
            ),
            (
                "std Command + InheritNothing (fork + strip)",
                Box::new(|| {
                    #[expect(
                        clippy::disallowed_types,
                        reason = "the measured lane: std's fork with the strip installed"
                    )]
                    let mut child = std::process::Command::new("/usr/bin/true")
                        .inherit_nothing()
                        .spawn()
                        .expect("spawn");
                    Box::new(move || {
                        child.wait().expect("wait");
                    })
                }),
            ),
            (
                "spawnfd::command (posix_spawn + CLOEXEC_DEFAULT)",
                Box::new(|| {
                    let mut child = command("/usr/bin/true").spawn().expect("spawn");
                    Box::new(move || {
                        child.wait().expect("wait");
                    })
                }),
            ),
        ];
        for (name, spawn) in &lanes {
            let spawns = percentiles(
                (0..SPAWNS)
                    .map(|_| {
                        let started = Instant::now();
                        let reap = spawn();
                        let took = started.elapsed();
                        reap();
                        took
                    })
                    .collect(),
            );
            let bills = percentiles(
                (0..REWRITES)
                    .map(|_| {
                        spawn()();
                        rewrite()
                    })
                    .collect(),
            );
            println!(
                "{name}: spawn() p50 {:.3} ms p90 {:.3} ms; \
                 rewrite after it p50 {:.2} ms p90 {:.2} ms",
                spawns.0, spawns.1, bills.0, bills.1
            );
        }
    }
}

fn main() {
    #[cfg(target_vendor = "apple")]
    bench::run();
    #[cfg(not(target_vendor = "apple"))]
    println!("spawn_cost measures macOS's region walk; nothing to measure on this target");
}
