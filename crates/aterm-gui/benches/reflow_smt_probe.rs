// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// THE REFLOW POOL ON A HYPER-THREADED MAC — the probe behind the x86_64 table in
// `app_render::reflow_worker_ceiling`'s doc comment.
//
// `workspace_scaling` measured the reflow ceiling on Apple silicon, where
// `hw.physicalcpu == hw.logicalcpu` and "cores" is unambiguous. On a 4-core /
// 8-thread Intel Mac `available_parallelism()` answers 8, so the ceiling faces a
// question `workspace_scaling` never had to ask: what do the workers past the
// physical cores buy on the history-back drain, and what do they cost the UI
// thread? This probe MODELS the settle to answer it. It is not the settle, and
// it does not stand in for the x86_64 rows of `workspace_scaling`.
//
// THE MODEL. N pooled workers drain a queue of `JOBS` = 120 jobs (a 30-tab x
// 4-pane settle). Each job is `REPS` = 8 rounds of LZ4-decompressing one ~250 KB
// history block (`aterm_lz4`, the codec under `aterm-scrollback`) into a REUSED
// buffer and rewrapping it: CPU-bound, no allocation per round, ~6.5 ms on an
// idle core of the 2017 MacBook Pro the table was taken on. Meanwhile the MAIN
// thread does what a live drag does — a compute quantum (the settle's own work,
// ~0.55 ms there) then a 1 ms timer sleep (an input or vsync wake) — and records
// how late each wake came back, until the pool drains. Workers are plain
// `std::thread::spawn`s, so they are `QOS_CLASS_DEFAULT` exactly as the app's
// `aterm-reflow` workers are today.
//
// A CLASS IS NOT A PRIORITY. The first line of output prints each class as
// `pthread_get_qos_class_np` reads it back — the REQUESTED class. The priority
// the kernel runs a thread at is what `ps -M -p <pid>` shows beside a run, and
// the two disagree: launched from a shell on the table's host, every thread of
// this process ran at 31, the DEFAULT workers' own base, while the main thread
// read back `USER_INTERACTIVE` (with `PROBE_MAIN_QOS=ui` too). So the wake column
// is the tail of a thread with NO priority headroom over the workers; a GUI
// app's UI thread has some (42 for the frontmost app there, 46 for Finder on
// screen behind it), and this probe does not measure that case.
//
// USAGE — a release build; the job is CPU-bound and its numbers mean nothing
// unoptimized:
//
//     cargo bench -p aterm-gui --features bench-support --bench reflow_smt_probe -- 2 4 6 8 120
//
// The positional arguments are the worker counts to sweep (default: 2 4 6 8 and
// the shipping ceiling); 120 is the unbounded shape, one worker per job. Every
// count runs `ROUNDS` = 5 times, interleaved with the others, and the medians
// are printed. Any other argument but the `--bench` cargo appends is taken for
// a criterion filter or flag aimed at the package's other benches (`cargo bench
// -p aterm-gui --features bench-support -- <args>` hands every bench target the
// same arguments): the probe prints one line and exits 0 without running.
// Optional knobs:
//
//   PROBE_UI_COMPUTE=0   the main thread only sleeps — no compute quantum. With
//                        the class capped at 31 (a shell launch), the wake tail
//                        under load follows the UI thread's OWN recent CPU use
//                        (timeshare priority decay); this is the switch that
//                        shows it. It does not show that the class never
//                        matters: a GUI app's UI thread can start above 31.
//   PROBE_MAIN_QOS=ui    the main thread requests USER_INTERACTIVE explicitly
//                        (from a shell the kernel still ran it at 31).
//   PROBE_WORKER_QOS=initiated|default
//                        each worker sets that class first (`initiated` is what
//                        `qos::Role::Responsive` would give the pool).
//
// Run it on a QUIET machine. The table in the doc comment keeps only the runs
// whose idle calibration ("one job (idle machine)") came within ~10% of 6.5 ms
// at a load average under ~3; a busy machine moves every column.

fn main() {
    #[cfg(target_os = "macos")]
    probe::main();
    #[cfg(not(target_os = "macos"))]
    eprintln!("reflow_smt_probe: macOS only — it reads hw.physicalcpu and the threads' QoS");
}

#[cfg(target_os = "macos")]
mod probe {
    use std::collections::VecDeque;
    use std::ffi::{CStr, c_char, c_int, c_void};
    use std::hint::black_box;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use aterm_gui::bench_support::BenchApp;

    /// One job per pane of a 30-tab x 4-pane settle.
    const JOBS: usize = 120;
    /// Decompress + rewrap rounds per job: ~6.5 ms on an idle core.
    const REPS: usize = 8;
    /// Runs per worker count; the medians are reported.
    const ROUNDS: usize = 5;
    /// Lines in the history block: ~250 KB plain, ~85 KB LZ4.
    const HISTORY_LINES: usize = 2000;

    const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
    const QOS_CLASS_USER_INITIATED: u32 = 0x19;
    const QOS_CLASS_DEFAULT: u32 = 0x15;

    // libSystem. `aterm-libc` carries neither QoS reader, so the probe declares
    // the four calls it makes itself.
    unsafe extern "C" {
        fn pthread_set_qos_class_self_np(qos: u32, relative_priority: c_int) -> c_int;
        fn pthread_get_qos_class_np(
            thread: *mut c_void,
            qos: *mut u32,
            relative_priority: *mut c_int,
        ) -> c_int;
        fn pthread_self() -> *mut c_void;
        fn sysctlbyname(
            name: *const c_char,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *mut c_void,
            newlen: usize,
        ) -> c_int;
    }

    /// One `u32`-wide sysctl by name; `None` if the kernel refuses it or answers
    /// at another width.
    fn sysctl_u32(name: &CStr) -> Option<u32> {
        let mut value: u32 = 0;
        let mut len = std::mem::size_of::<u32>();
        // SAFETY: `name` is NUL-terminated and outlives the call; `value` and
        // `len` are live locals the kernel writes only for the call's duration,
        // and `len` starts at the buffer's real size, so the copy is bounded. The
        // new-value pair is null/0: this reads the sysctl and never sets it.
        let rc = unsafe {
            sysctlbyname(
                name.as_ptr(),
                (&mut value as *mut u32).cast::<c_void>(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        (rc == 0 && len == std::mem::size_of::<u32>()).then_some(value)
    }

    /// The calling thread's QoS class, as the kernel reports it.
    fn qos_self() -> u32 {
        let mut qos = 0u32;
        let mut relative: c_int = 0;
        // SAFETY: reads the calling thread's own QoS into two live locals; the
        // handle is this thread's, alive for the whole call.
        unsafe { pthread_get_qos_class_np(pthread_self(), &mut qos, &mut relative) };
        qos
    }

    /// Sets the calling thread's own QoS class; the result code is returned.
    fn set_qos_self(qos: u32) -> c_int {
        // SAFETY: sets this thread's own class; no pointers, no shared state.
        unsafe { pthread_set_qos_class_self_np(qos, 0) }
    }

    fn qos_name(qos: u32) -> &'static str {
        match qos {
            0x21 => "USER_INTERACTIVE",
            0x19 => "USER_INITIATED",
            0x15 => "DEFAULT",
            0x11 => "UTILITY",
            0x09 => "BACKGROUND",
            0x00 => "UNSPECIFIED",
            _ => "?",
        }
    }

    /// `PROBE_WORKER_QOS`: unset leaves the pthread default (what
    /// `std::thread::spawn` gives the real pool); `initiated` is
    /// `USER_INITIATED` (`qos::Role::Responsive`); `default` sets
    /// `QOS_CLASS_DEFAULT` explicitly.
    fn apply_worker_qos() {
        match std::env::var("PROBE_WORKER_QOS").as_deref() {
            Ok("initiated") => {
                set_qos_self(QOS_CLASS_USER_INITIATED);
            }
            Ok("default") => {
                set_qos_self(QOS_CLASS_DEFAULT);
            }
            _ => {}
        }
    }

    /// Pseudo terminal text from a small vocabulary (build logs, `ls` output,
    /// paths), so LZ4 finds matches the way it does in real scrollback.
    fn make_history(lines: usize) -> Vec<u8> {
        const WORDS: &[&str] = &[
            "Compiling",
            "aterm-gui",
            "v0.75.0",
            "(/Users//tester/aterm/crates/aterm-gui)",
            "warning:",
            "unused",
            "variable:",
            "`x`",
            "-->",
            "src/lib.rs:2318:9",
            "drwxr-xr-x",
            "12",
            "tester",
            "staff",
            "384",
            "Sep",
            "6",
            "18:48",
            "target/",
            "Finished",
            "`dev`",
            "profile",
            "[unoptimized",
            "+",
            "debuginfo]",
            "target(s)",
            "in",
            "3.21s",
            "error[E0308]:",
            "mismatched",
            "types",
            "expected",
            "`usize`,",
            "found",
            "`u32`",
            "|",
            "^^^^",
            "help:",
            "you",
            "can",
            "convert",
            "héllo",
            "wörld",
            "—",
            "…",
            "\t",
            "0x7ffee3",
            "1234",
            "cargo",
            "check",
            "-p",
            "test",
        ];
        let mut out = Vec::with_capacity(lines * 130);
        let mut x: u32 = 0x9E37_79B9;
        for i in 0..lines {
            let len = 40 + (i * 37) % 160;
            let mut line = String::new();
            while line.len() < len {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                line.push_str(WORDS[(x as usize >> 8) % WORDS.len()]);
                line.push(' ');
            }
            out.extend_from_slice(line.as_bytes());
            out.push(b'\n');
        }
        out
    }

    /// The rewrap pass: walk the bytes, count cells (UTF-8 lead bytes only),
    /// wrap at `cols`.
    fn rewrap(buf: &[u8], cols: usize) -> u64 {
        let mut rows: u64 = 0;
        let mut col = 0usize;
        let mut acc: u64 = 0;
        for &b in buf {
            if b == b'\n' {
                rows += 1;
                col = 0;
                acc = acc.wrapping_mul(31).wrapping_add(rows);
                continue;
            }
            if b & 0xC0 == 0x80 {
                continue; // continuation byte
            }
            let w = if b == b'\t' {
                8 - (col % 8)
            } else if b >= 0x80 {
                2
            } else {
                1
            };
            col += w;
            if col >= cols {
                rows += 1;
                col -= cols;
            }
        }
        acc ^ rows
    }

    /// One job: `REPS` x (LZ4 decompress into a REUSED buffer + rewrap). No
    /// allocation per round, so the work is CPU-bound, not page-fault-bound.
    fn job(compressed: &[u8], out: &mut Vec<u8>, cols: usize) -> u64 {
        let mut sink = 0u64;
        let (size, rest) = aterm_lz4::uncompressed_size(compressed).expect("lz4 size prefix");
        out.resize(size, 0);
        for r in 0..REPS {
            let n = aterm_lz4::decompress_into(rest, out).expect("lz4 block");
            sink ^= rewrap(&out[..n], cols + r);
        }
        sink
    }

    fn spin_quantum(iters: u64) -> u64 {
        let mut x = 0x1234_5678_9abc_def0u64;
        for i in 0..iters {
            x = x.rotate_left(7) ^ i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
        x
    }

    /// The `p` quantile (nearest rank) of `v`, sorting it in place; NaN when
    /// empty (no compute quanta under `PROBE_UI_COMPUTE=0`).
    fn pct(v: &mut [f64], p: f64) -> f64 {
        if v.is_empty() {
            return f64::NAN;
        }
        v.sort_by(f64::total_cmp);
        let i = ((v.len() as f64 - 1.0) * p).round() as usize;
        v[i]
    }

    struct Run {
        drain_ms: f64,
        /// Mean per-job wall time as seen by the worker running it.
        job_ms: f64,
        q_p50: f64,
        q_p99: f64,
        q_max: f64,
        w_p50: f64,
        w_p99: f64,
        w_max: f64,
    }

    fn run(n: usize, compressed: &Arc<Vec<u8>>, quantum_iters: u64, ui_compute: bool) -> Run {
        let queue: Arc<Mutex<VecDeque<usize>>> = Arc::new(Mutex::new((0..JOBS).collect()));
        let done = Arc::new(AtomicBool::new(false));
        let remaining = Arc::new(AtomicU64::new(JOBS as u64));
        let sink = Arc::new(AtomicU64::new(0));
        let busy_ns = Arc::new(AtomicU64::new(0));
        let t0 = Instant::now();
        let mut workers = Vec::with_capacity(n);
        for _ in 0..n {
            let (queue, compressed, remaining, done, sink, busy_ns) = (
                queue.clone(),
                compressed.clone(),
                remaining.clone(),
                done.clone(),
                sink.clone(),
                busy_ns.clone(),
            );
            workers.push(std::thread::spawn(move || {
                apply_worker_qos();
                let mut out = Vec::new();
                loop {
                    let next = queue.lock().expect("job queue").pop_front();
                    let Some(j) = next else { break };
                    let t = Instant::now();
                    let s = job(&compressed, &mut out, 80 + (j % 120));
                    busy_ns.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
                    sink.fetch_xor(s, Ordering::Relaxed);
                    if remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
                        done.store(true, Ordering::Release);
                    }
                }
            }));
        }
        // The main thread: a compute quantum (the settle's own work) then a timer
        // wake (an input or vsync event needing a core), until the pool drains.
        let mut quanta = Vec::new();
        let mut wakes = Vec::new();
        while !done.load(Ordering::Acquire) {
            if ui_compute {
                let t = Instant::now();
                black_box(spin_quantum(quantum_iters));
                quanta.push(t.elapsed().as_secs_f64() * 1e3);
            }
            let t = Instant::now();
            std::thread::sleep(Duration::from_millis(1));
            wakes.push(t.elapsed().as_secs_f64() * 1e3 - 1.0);
        }
        let drain_ms = t0.elapsed().as_secs_f64() * 1e3;
        for worker in workers {
            worker.join().expect("reflow probe worker");
        }
        black_box(sink.load(Ordering::Relaxed));
        Run {
            drain_ms,
            job_ms: busy_ns.load(Ordering::Relaxed) as f64 / 1e6 / JOBS as f64,
            q_p50: pct(&mut quanta, 0.5),
            q_p99: pct(&mut quanta, 0.99),
            q_max: pct(&mut quanta, 1.0),
            w_p50: pct(&mut wakes, 0.5),
            w_p99: pct(&mut wakes, 0.99),
            w_max: pct(&mut wakes, 1.0),
        }
    }

    /// The worker counts on the command line, or the first argument that is
    /// not one. `cargo bench` hands every bench target of the package the same
    /// arguments and appends `--bench`, and the package's other benches are
    /// criterion: `cargo bench -p aterm-gui --features bench-support --
    /// resize_settle` hands this probe `resize_settle --bench`, and `--
    /// --save-baseline eight` hands it `--save-baseline eight --bench`. A count
    /// is a bare integer and `--bench` is cargo's; anything else belongs to
    /// another bench, and the caller steps aside rather than guess which of a
    /// flag's values are counts.
    fn worker_counts(args: impl Iterator<Item = String>) -> Result<Vec<usize>, String> {
        args.filter(|arg| arg != "--bench")
            .map(|arg| arg.parse::<usize>().map_err(|_| arg))
            .collect()
    }

    pub(super) fn main() {
        let mut counts = match worker_counts(std::env::args().skip(1)) {
            Ok(counts) => counts,
            Err(arg) => {
                eprintln!(
                    "reflow_smt_probe: not run — `{arg}` is not a worker count, so these arguments \
                     are another bench's; `--bench reflow_smt_probe -- 2 4 6 8 120` runs the probe"
                );
                return;
            }
        };
        if std::env::var("PROBE_MAIN_QOS").as_deref() == Ok("ui") {
            assert_eq!(
                set_qos_self(QOS_CLASS_USER_INTERACTIVE),
                0,
                "pthread_set_qos_class_self_np"
            );
        }
        let ui_compute = std::env::var("PROBE_UI_COMPUTE").as_deref() != Ok("0");
        let worker_qos = std::thread::spawn(|| {
            let before = qos_self();
            apply_worker_qos();
            (before, qos_self())
        })
        .join()
        .expect("qos read-back thread");
        let main_qos = qos_self();
        println!(
            "qos: main={} ({main_qos:#x}); spawned worker as pthread default={} ({:#x}), \
             after PROBE_WORKER_QOS={} ({:#x})",
            qos_name(main_qos),
            qos_name(worker_qos.0),
            worker_qos.0,
            qos_name(worker_qos.1),
            worker_qos.1
        );
        let logical = std::thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get);
        let shipping = BenchApp::reflow_thread_ceiling(JOBS);
        println!(
            "available_parallelism={logical} hw.physicalcpu={:?} hw.logicalcpu={:?} \
             hw.ncpu={:?}; shipping reflow ceiling for a {JOBS}-job settle: {shipping}",
            sysctl_u32(c"hw.physicalcpu"),
            sysctl_u32(c"hw.logicalcpu"),
            sysctl_u32(c"hw.ncpu"),
        );

        let history = make_history(HISTORY_LINES);
        let compressed = Arc::new(aterm_lz4::compress_prepend_size(&history));
        println!(
            "history: {} B plain, {} B lz4",
            history.len(),
            compressed.len()
        );

        // Calibrate: one job single-threaded, and the main-thread quantum —
        // doubled until it takes at least 0.3 ms, which lands near 0.55 ms at
        // `-O3` on the table's host.
        let mut out = Vec::new();
        black_box(job(&compressed, &mut out, 100)); // warm
        let t = Instant::now();
        black_box(job(&compressed, &mut out, 100));
        let job_ms = t.elapsed().as_secs_f64() * 1e3;
        let mut quantum_iters = 1_000_000u64;
        loop {
            let t = Instant::now();
            black_box(spin_quantum(quantum_iters));
            if t.elapsed().as_secs_f64() * 1e3 >= 0.3 {
                break;
            }
            quantum_iters *= 2;
        }
        // An unoptimized build spends ~40 ms on the million-iteration start;
        // halve back under 1 ms so its UI thread still does what the model says
        // the settle does. Never taken where the table was measured: a million
        // iterations took 0.51-0.69 ms there.
        loop {
            let t = Instant::now();
            black_box(spin_quantum(quantum_iters));
            if t.elapsed().as_secs_f64() * 1e3 <= 1.0 || quantum_iters == 1 {
                break;
            }
            quantum_iters /= 2;
        }
        let t = Instant::now();
        black_box(spin_quantum(quantum_iters));
        let q_ms = t.elapsed().as_secs_f64() * 1e3;
        println!(
            "one job (idle machine): {job_ms:.2} ms; main-thread quantum (idle): {q_ms:.3} ms; \
             ui compute {}",
            if ui_compute {
                "ON"
            } else {
                "OFF (PROBE_UI_COMPUTE=0)"
            }
        );

        if counts.is_empty() {
            counts = vec![2, 4, 6, 8];
            if !counts.contains(&shipping) {
                counts.push(shipping);
                counts.sort_unstable();
            }
        }
        assert!(
            counts.iter().all(|&n| n > 0),
            "a pool of zero workers never drains"
        );
        println!(
            "\nworkers | drain({JOBS} jobs) ms | job ms (idle {job_ms:.2}) | main quantum \
             p50/p99/max ms (idle {q_ms:.2}) | wake overshoot p50/p99/max ms"
        );
        let mut table: Vec<(usize, Vec<Run>)> = counts.iter().map(|&n| (n, Vec::new())).collect();
        for _ in 0..ROUNDS {
            for (n, runs) in &mut table {
                runs.push(run(*n, &compressed, quantum_iters, ui_compute));
                std::thread::sleep(Duration::from_millis(300)); // let the box cool between runs
            }
        }
        for (n, runs) in &table {
            let med = |f: &dyn Fn(&Run) -> f64| {
                let mut v: Vec<f64> = runs.iter().map(f).collect();
                pct(&mut v, 0.5)
            };
            let mark = if *n == shipping { "  <- shipping" } else { "" };
            println!(
                "{n:>7} | {:>18.1} | {:>18.2} | {:>6.3} / {:>6.3} / {:>6.3} | {:>6.3} / {:>6.3} / \
                 {:>6.3}{mark}",
                med(&|r| r.drain_ms),
                med(&|r| r.job_ms),
                med(&|r| r.q_p50),
                med(&|r| r.q_p99),
                med(&|r| r.q_max),
                med(&|r| r.w_p50),
                med(&|r| r.w_p99),
                med(&|r| r.w_max)
            );
        }
    }
}
