// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A live sample of the system-awareness readers on this host, and what each
//! costs: every reader once (its answer printed), then the median, 90th
//! percentile and worst wall time of `--calls N` back-to-back calls (default
//! 200). Dev only; nothing ships it.
//!
//! ```sh
//! targo --unverified run -q -p aterm-sysprobe --example live
//! targo --unverified run -q -p aterm-sysprobe --example live -- --qos-check
//! ```
//!
//! `--qos-check` (macOS) checks that the layout's logical ids are the CPU
//! numbers the kernel runs threads on: threads at each QoS class spin for
//! two seconds, asking `pthread_cpu_number_np` (public since macOS 11) which
//! CPU they are on, and the counts are printed by CPU number with the
//! layout's cluster for it. The scheduler keeps background-QoS threads on
//! the efficiency cores, so every background count should fall in the
//! efficiency cluster, under any load. The kernel's CPU number is the
//! processor index `host_processor_info` answers by and the device tree's
//! `logical-cpu-id` (xnu builds its processor array from that id).

use std::path::Path;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let calls = args
        .iter()
        .position(|a| a == "--calls")
        .and_then(|i| args.get(i + 1))
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(200)
        .max(1);

    let mut probe = aterm_sysprobe::Probe::new();
    let layout = aterm_sysprobe::cpu_layout();
    println!("cpu_layout   {layout:?}");
    println!("core_ticks   {:?}", probe.core_ticks());
    println!("memory       {:?}", aterm_sysprobe::memory_detail());
    println!("gpu          {:?}", aterm_sysprobe::gpu());
    println!("volume /     {:?}", aterm_sysprobe::volume(Path::new("/")));
    if let Some(home) = std::env::var_os("HOME") {
        println!(
            "volume ~     {:?}",
            aterm_sysprobe::volume(Path::new(&home))
        );
    }

    println!("\ncost over {calls} calls (median / p90 / max):");
    report(
        "core_ticks",
        cost(calls, || probe.core_ticks().map(|c| c.len())),
    );
    report("cpu_layout", cost(calls, aterm_sysprobe::cpu_layout));
    report("memory_detail", cost(calls, aterm_sysprobe::memory_detail));
    report("gpu", cost(calls, aterm_sysprobe::gpu));
    report(
        "volume /",
        cost(calls, || aterm_sysprobe::volume(Path::new("/"))),
    );

    if args.iter().any(|a| a == "--qos-check") {
        qos_check(&layout);
    }
}

/// Every call's wall time, sorted.
fn cost<T>(calls: usize, mut f: impl FnMut() -> T) -> Vec<Duration> {
    let mut times: Vec<Duration> = (0..calls)
        .map(|_| {
            let t = Instant::now();
            std::hint::black_box(f());
            t.elapsed()
        })
        .collect();
    times.sort_unstable();
    times
}

fn report(name: &str, times: Vec<Duration>) {
    let at = |q: usize| times[(times.len() - 1) * q / 100];
    println!(
        "  {name:<14} {:>9.1?} / {:>9.1?} / {:>9.1?}",
        at(50),
        at(90),
        at(100)
    );
}

#[cfg(target_os = "macos")]
fn qos_check(layout: &aterm_sysprobe::CpuLayout) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    unsafe extern "C" {
        /// `<pthread/pthread.h>`, macOS 11: the CPU number the calling thread
        /// is on, in `[0, hw.ncpu)`; 0 or an errno.
        fn pthread_cpu_number_np(cpu_number_out: *mut usize) -> libc::c_int;
    }

    const MAX: usize = 256;
    let classes = [
        ("background", libc::qos_class_t::QOS_CLASS_BACKGROUND),
        ("utility", libc::qos_class_t::QOS_CLASS_UTILITY),
        (
            "user-interactive",
            libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE,
        ),
    ];
    let cores = layout.logical_cores().max(1);
    let stop = Arc::new(AtomicBool::new(false));
    let threads: Vec<_> = classes
        .iter()
        .flat_map(|&(name, class)| {
            let stop = &stop;
            (0..cores).map(move |_| {
                let stop = Arc::clone(stop);
                let handle = std::thread::spawn(move || {
                    // SAFETY: sets this thread's own QoS class; no pointers.
                    unsafe { libc::pthread_set_qos_class_self_np(class, 0) };
                    let mut seen = [0u64; MAX];
                    while !stop.load(Ordering::Relaxed) {
                        let mut cpu = 0usize;
                        // SAFETY: `cpu` is a writable `size_t`.
                        if unsafe { pthread_cpu_number_np(&raw mut cpu) } == 0 && cpu < MAX {
                            seen[cpu] += 1;
                        }
                        for _ in 0..1000 {
                            std::hint::spin_loop();
                        }
                    }
                    seen
                });
                (name, handle)
            })
        })
        .collect();
    std::thread::sleep(Duration::from_secs(2));
    stop.store(true, Ordering::Relaxed);
    let mut by_class: Vec<(&str, [u64; MAX])> =
        classes.iter().map(|(n, _)| (*n, [0; MAX])).collect();
    for (name, handle) in threads {
        let seen = handle.join().expect("a spinner");
        if let Some((_, sum)) = by_class.iter_mut().find(|(n, _)| *n == name) {
            for (s, v) in sum.iter_mut().zip(seen) {
                *s += v;
            }
        }
    }
    let cluster_of = |cpu: usize| {
        layout
            .clusters
            .iter()
            .find(|c| c.logical.iter().any(|&id| usize::from(id) == cpu))
            .map_or("?", |c| c.name.as_str())
    };
    println!("\nqos check: which CPU numbers {cores} spinners per QoS class ran on");
    for (name, seen) in &by_class {
        let total: u64 = seen.iter().sum();
        print!("  {name:<17}");
        for (cpu, n) in seen.iter().enumerate().filter(|(_, n)| **n > 0) {
            print!(" cpu{cpu}({}) {}%", cluster_of(cpu), n * 100 / total.max(1));
        }
        println!();
    }
}

#[cfg(not(target_os = "macos"))]
fn qos_check(_: &aterm_sysprobe::CpuLayout) {
    println!("\nqos check: macOS only");
}
