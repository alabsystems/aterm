// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stats deck A4, step N2, the flow unit's per-reading costs
//! (`docs/DESIGN-stats-deck-instruments-2026-11-06.md` §13, §17), each read in process as aterm
//! reads it, with the 1-minute load beside it:
//!
//! - **The task tables with the run queue** ([`aterm_sysprobe::task_room`]): P comes from the
//!   `processor_set_statistics(PROCESSOR_SET_LOAD_INFO)` call that already counts the machine's
//!   threads, plus its self-check (`runq::pset_holds`). Timed on a utility-QoS thread at
//!   [`TICK`], thread time and wall time a reading.
//! - **The own-thread D-state census** ([`aterm_sysprobe::dstate::Census`] over the kernel), on a
//!   utility-QoS thread of its own, a pass every [`PASS_EVERY`] (aterm's is every 30 s; a shorter
//!   gap takes more passes in the same minutes, and a pass's cost does not depend on the gap):
//!   its thread time a pass, passes over the 5 ms budget, processes listed and read, and covers.
//!
//! `flow_costs [--secs 60]` prints one line a measure.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("flow_costs: a macOS measurement (the processor set's load and libproc threads)");
    std::process::exit(2);
}

#[cfg(target_os = "macos")]
fn main() {
    std::process::exit(mac::run());
}

#[cfg(target_os = "macos")]
mod mac {
    use std::time::{Duration, Instant};

    use aterm_sysprobe::dstate::{Census, Kernel, PASS_BUDGET_NS, thread_ns};
    use aterm_sysprobe::runq::RunQueue;
    use aterm_sysprobe::{load_average, task_room};

    /// The stand-in probe thread's reading gap.
    const TICK: Duration = Duration::from_millis(100);
    /// The census's gap between passes in this measure.
    const PASS_EVERY: Duration = Duration::from_secs(1);

    fn set_utility() {
        // SAFETY: changes only the calling thread's QoS class.
        let _ =
            unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0) };
    }

    /// Nearest-rank p50, p95 and max.
    fn stats(values: &[f64]) -> (usize, f64, f64, f64) {
        let mut v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
        if v.is_empty() {
            return (0, f64::NAN, f64::NAN, f64::NAN);
        }
        v.sort_by(f64::total_cmp);
        let rank = |q: f64| {
            let i = ((q * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1;
            v[i]
        };
        (v.len(), rank(0.5), rank(0.95), v[v.len() - 1])
    }

    fn line(name: &str, unit: &str, values: &[f64]) {
        let (n, p50, p95, max) = stats(values);
        println!("{name}: n={n} p50={p50:.3}{unit} p95={p95:.3}{unit} max={max:.3}{unit}");
    }

    fn load1() -> String {
        load_average().map_or_else(|| "unread".into(), |l| format!("{:.2}", l[0]))
    }

    pub(super) fn run() -> i32 {
        let mut secs = 60u64;
        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            match (a.as_str(), args.next()) {
                ("--secs", Some(v)) => secs = v.parse().unwrap_or(secs),
                _ => {
                    eprintln!("usage: flow_costs [--secs 60]");
                    return 2;
                }
            }
        }
        let span = Duration::from_secs(secs);
        println!("load1 at start: {}", load1());
        // SAFETY: `geteuid` cannot fail.
        let uid = unsafe { libc::geteuid() };

        let census = std::thread::spawn(move || {
            set_utility();
            let mut c = Census::new(Kernel::new());
            let (mut thread_us, mut listed, mut read) = (Vec::new(), Vec::new(), Vec::new());
            let (mut over, mut covers, mut cover_passes, mut marked) = (0, 0, Vec::new(), 0);
            let end = Instant::now() + span;
            while Instant::now() < end {
                let p = c.pass();
                thread_us.push(p.thread_ns as f64 / 1e3);
                listed.push(f64::from(p.listed));
                read.push(f64::from(p.read));
                over += usize::from(p.thread_ns > PASS_BUDGET_NS);
                marked += p.marked.len();
                if p.cover_done {
                    covers += 1;
                    cover_passes.push(f64::from(p.passes_in_cover));
                }
                std::thread::sleep(PASS_EVERY);
            }
            (thread_us, listed, read, over, covers, cover_passes, marked)
        });

        set_utility();
        let (mut thread_us, mut wall_us) = (Vec::new(), Vec::new());
        let (mut read_p, mut unread) = (0usize, 0usize);
        let end = Instant::now() + span;
        while Instant::now() < end {
            let (t0, w0) = (thread_ns(), Instant::now());
            let room = task_room(uid);
            wall_us.push(w0.elapsed().as_secs_f64() * 1e6);
            thread_us.push(thread_ns().saturating_sub(t0) as f64 / 1e3);
            match room.run_queue {
                RunQueue::Milli(_) => read_p += 1,
                RunQueue::Failed | RunQueue::UnreadFormat | RunQueue::Unsupported => unread += 1,
            }
            std::thread::sleep(TICK);
        }
        line("task_room with P, thread", "us", &thread_us);
        line("task_room with P, wall", "us", &wall_us);
        println!("run queue: read {read_p}, unread {unread}");

        let Ok((t, listed, read, over, covers, cover_passes, marked)) = census.join() else {
            eprintln!("flow_costs: the census thread panicked");
            return 1;
        };
        line("census pass, thread", "us", &t);
        line("census listed", "", &listed);
        line("census read a pass", "", &read);
        line("census passes a cover", "", &cover_passes);
        println!(
            "census: {} passes, {over} over {PASS_BUDGET_NS} ns, {covers} covers, {marked} marks",
            t.len()
        );
        println!("load1 at end: {}", load1());
        0
    }
}
