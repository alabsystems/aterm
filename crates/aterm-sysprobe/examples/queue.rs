// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! M-QUEUE (stats deck A4, `docs/DESIGN-stats-deck-instruments-2026-11-06.md` §16): the run
//! queue the over rule reads, measured on this Mac.
//!
//! Each second, in process and as the engine reads them, it takes:
//!
//! - the busy cores, machine-wide: every core's tick delta (`host_processor_info`, the reader
//!   behind the Snapshot's `cpu_mc`);
//! - the cores' worth waiting for a core: each readable process's runnable time less its CPU
//!   time over the window (`proc_pid_rusage`'s `ri_runnable_time`, the reader behind
//!   `wait_mc`), with the count the kernel refused (another user's processes);
//! - the load average (`getloadavg`, the engine's load1);
//! - the raw fixed-point load average (`vm.loadavg`), from whose 5 s steps the kernel's own
//!   machine-wide runnable-thread count is recovered exactly: xnu's `compute_averunnable`
//!   (`osfmk/kern/sched_average.c`) sets each of the three averages to
//!   `(cexp·L + n·FSCALE·(FSCALE − cexp)) >> FSHIFT` with `n` the scheduler's count of runnable
//!   threads (`sched_run_buckets[TH_BUCKET_RUN] − 1`), so each step pins `n` to one integer,
//!   and the three averages must agree on it (the self-check: a torn read, two steps between
//!   reads or a changed formula reads as unread, never as a wrong count);
//! - the Mach load averages (`HOST_LOAD_INFO`) and the processor set's
//!   (`PROCESSOR_SET_LOAD_INFO`), for their cadence;
//! - the thermal pressure level (`notify_get_state` on `com.apple.system.thermalpressurelevel`).
//!
//! It runs bounded bursts of spin threads in a child at nice 20 and, with `--bg`, in the Darwin
//! background band (`PRIO_DARWIN_BG`) too: the threads still count in every runnable read, the
//! kernel's included, while they take the least CPU from the desk's own work. It records when
//! each burst's threads ran and ended, so `analyze` can time how fast each input falls back once
//! the queue drains.
//!
//! - `queue cadence --secs S --every-ms M --out F`: when each kernel average steps.
//! - `queue selfcheck --in F`: the processor set's step inversion over a cadence file; a double
//!   step it does not refuse fails it.
//! - `queue record --secs S --out F [--sys-out F] [--bg] [--mach] [--burst AT:HOLD:THREADS]...`:
//!   the recording; `--sys-out` adds the live engine's `aterm ctl @self sys --json` every 3 s,
//!   `--mach` the host's Mach load averages.
//! - `queue spin --threads N --secs S [--bg]`: a burst's child; it ends itself at `S`.
//! - `queue analyze F [--sys F] [--cores N]`: the M-QUEUE figures from a recording.

#![cfg_attr(not(target_os = "macos"), allow(dead_code, unused_imports))]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("");
    let code = match mode {
        "spin" => spin(&args),
        "cadence" => run_cadence(&args),
        "record" => run_record(&args),
        "analyze" => run_analyze(&args),
        "selfcheck" => run_selfcheck(&args),
        _ => {
            eprintln!("usage: queue cadence|record|spin|analyze ... (see the file's header)");
            2
        }
    };
    std::process::exit(code);
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn flag_u64(args: &[String], name: &str, default: u64) -> u64 {
    flag(args, name)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// A burst's child: `threads` spinning threads at nice 20 until `secs` from now, then exit.
fn spin(args: &[String]) -> i32 {
    let threads = usize::try_from(flag_u64(args, "--threads", 4))
        .unwrap_or(4)
        .clamp(1, 64);
    // A burst is bounded here as well as by its parent: never past ten minutes.
    let secs = flag_u64(args, "--secs", 10).clamp(1, 600);
    #[cfg(unix)]
    {
        // SAFETY: setpriority on this process (who 0) with a constant; no pointer.
        let rc = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 20) };
        if rc != 0 {
            eprintln!(
                "spin: setpriority failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    #[cfg(target_os = "macos")]
    if args.iter().any(|a| a == "--bg") {
        /// `PRIO_DARWIN_PROCESS` and `PRIO_DARWIN_BG` (`sys/resource.h`): this process to the
        /// background band, whose threads still count as runnable but take the least CPU from
        /// everything else while they wait.
        const PRIO_DARWIN_PROCESS: libc::c_int = 4;
        const PRIO_DARWIN_BG: libc::c_int = 0x1000;
        // SAFETY: setpriority on this process (who 0) with constants; no pointer.
        let rc = unsafe { libc::setpriority(PRIO_DARWIN_PROCESS, 0, PRIO_DARWIN_BG) };
        if rc != 0 {
            eprintln!(
                "spin: PRIO_DARWIN_BG failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    let deadline = Instant::now() + Duration::from_secs(secs);
    let handles: Vec<_> = (0..threads)
        .map(|i| {
            std::thread::spawn(move || {
                let mut x = 0x9E37_79B9_7F4A_7C15_u64 ^ i as u64;
                while Instant::now() < deadline {
                    for _ in 0..200_000 {
                        x = std::hint::black_box(
                            x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1),
                        );
                    }
                }
                x
            })
        })
        .collect();
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "ready {}", unix_ms());
    let _ = out.flush();
    for h in handles {
        let _ = h.join();
    }
    let _ = writeln!(out, "done {}", unix_ms());
    let _ = out.flush();
    0
}

/// The processor set's step: xnu's `compute_averages` moves `sched_load_average` to
/// `((L << 2) + n·LOAD_SCALE) / 5` and `sched_mach_factor` to `((M << 2) + f(n)) / 5` once a
/// scheduler tick, with `f(n)` `(ncpus − n)·LOAD_SCALE`, or `ncpus·LOAD_SCALE / (n + 1)` past the
/// cores. The count one step of both took them with, where the load pins one integer and the
/// mach factor agrees; `None` where they do not (two ticks between reads, a torn read).
#[must_use]
fn pset_n_of_step(before: [i64; 2], after: [i64; 2], ncpus: i64) -> Option<i64> {
    let num = 5 * after[0] - 4 * before[0];
    let n = num.div_euclid(1000) + i64::from(num.rem_euclid(1000) != 0);
    if n < 0 || n * 1000 > num + 4 {
        return None;
    }
    let factor = if n > ncpus {
        ncpus * 1000 / (n + 1)
    } else {
        (ncpus - n) * 1000
    };
    ((4 * before[1] + factor) / 5 == after[1]).then_some(n)
}

/// The negative control for both inversions, over a cadence file: every recorded step must pass
/// its self-check, and every pair of steps read as one (what a reader that missed a step sees)
/// must be refused.
fn run_selfcheck(args: &[String]) -> i32 {
    let Some(path) = flag(args, "--in") else {
        eprintln!("selfcheck: --in CADENCE_FILE");
        return 2;
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("selfcheck: cannot read {path}");
        return 1;
    };
    let ncpus = flag_u64(args, "--cores", 10) as i64;
    let (mut ld, mut ps): (Vec<[u32; 3]>, Vec<[i64; 2]>) = (Vec::new(), Vec::new());
    for line in text.lines() {
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let raw = |i: usize| {
            v.pointer(&format!("/raw/{i}"))
                .and_then(aterm_json::Value::as_i64)
        };
        match v.get("src").and_then(aterm_json::Value::as_str) {
            Some("vm.loadavg") => {
                if let (Some(a), Some(b), Some(c)) = (raw(0), raw(1), raw(2)) {
                    ld.push([a, b, c].map(|x| u32::try_from(x).unwrap_or(0)));
                }
            }
            Some("pset_load") => {
                // A line also records a task or thread count changing alone: only the load and
                // the mach factor step.
                if let (Some(l), Some(m)) = (raw(2), raw(3))
                    && ps.last() != Some(&[l, m])
                {
                    ps.push([l, m]);
                }
            }
            _ => {}
        }
    }
    let one_ld = ld
        .windows(2)
        .filter(|w| nrun_of_step(w[0], w[1]).is_some())
        .count();
    let two_ld = ld
        .windows(3)
        .filter(|w| nrun_of_step(w[0], w[2]).is_some())
        .count();
    let one_ps = ps
        .windows(2)
        .filter(|w| pset_n_of_step(w[0], w[1], ncpus).is_some())
        .count();
    let two_ps = ps
        .windows(3)
        .filter(|w| pset_n_of_step(w[0], w[2], ncpus).is_some())
        .count();
    println!(
        "vm.loadavg: {one_ld} of {} single steps pass, {two_ld} of {} double steps pass (must be 0)",
        ld.len().saturating_sub(1),
        ld.len().saturating_sub(2)
    );
    println!(
        "processor set: {one_ps} of {} single steps pass, {two_ps} of {} double steps pass (must be 0)",
        ps.len().saturating_sub(1),
        ps.len().saturating_sub(2)
    );
    let counts: Vec<i64> = ps
        .windows(2)
        .filter_map(|w| pset_n_of_step(w[0], w[1], ncpus))
        .collect();
    if !counts.is_empty() {
        let mut s = counts.clone();
        s.sort_unstable();
        println!(
            "processor-set runnable counts per tick: min {} p50 {} max {} (n {})",
            s[0],
            s[s.len() / 2],
            s[s.len() - 1],
            s.len()
        );
    }
    i32::from(two_ld + two_ps > 0)
}

/// The runnable-thread count one step of `compute_averunnable` took the three averages from
/// `before` to `after` with, where all three agree on one integer; `None` where they do not.
#[must_use]
fn nrun_of_step(before: [u32; 3], after: [u32; 3]) -> Option<u32> {
    // `FSCALE` (`1 << FSHIFT`, `FSHIFT` 11) and xnu's `cexp`, each `(fixpt_t)(e^(-5/T) · FSCALE)`
    // for T = 60, 300, 900 s: the cast truncates, so 1884.25, 2014.15 and 2036.65 are 1884, 2014
    // and 2036.
    const FSCALE: i64 = 2048;
    const CEXP: [i64; 3] = [1884, 2014, 2036];
    let mut found: Option<i64> = None;
    for i in 0..3 {
        let (l0, l1) = (i64::from(before[i]), i64::from(after[i]));
        let num = FSCALE * l1 - CEXP[i] * l0;
        let den = FSCALE * (FSCALE - CEXP[i]);
        let n = num.div_euclid(den) + i64::from(num.rem_euclid(den) != 0);
        if n < 0 || n * den > num + (FSCALE - 1) {
            return None;
        }
        match found {
            None => found = Some(n),
            Some(m) if m == n => {}
            Some(_) => return None,
        }
    }
    found.and_then(|n| u32::try_from(n).ok())
}

#[cfg(not(target_os = "macos"))]
fn run_cadence(_args: &[String]) -> i32 {
    eprintln!("queue: the kernel readers are macOS's");
    1
}

#[cfg(not(target_os = "macos"))]
fn run_record(_args: &[String]) -> i32 {
    eprintln!("queue: the kernel readers are macOS's");
    1
}

/// The macOS kernel readers.
#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{c_char, c_int};

    /// `struct loadavg` (`sys/resource.h`): the three averages in `fixpt_t` and their scale.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct LoadAvg {
        pub ldavg: [u32; 3],
        pub fscale: std::ffi::c_long,
    }

    /// `FSCALE` (`1 << FSHIFT`, `FSHIFT` 11), the scale `vm.loadavg` must report.
    pub const FSCALE: std::ffi::c_long = 2048;

    /// `vm.loadavg` as the kernel holds it.
    pub fn loadavg_raw() -> Option<LoadAvg> {
        let mut v = LoadAvg::default();
        let mut len = std::mem::size_of::<LoadAvg>();
        // SAFETY: `v` is a writable `struct loadavg` and `len` its size; no new value is set.
        let rc = unsafe {
            libc::sysctlbyname(
                c"vm.loadavg".as_ptr(),
                (&raw mut v).cast(),
                &raw mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        (rc == 0 && len == std::mem::size_of::<LoadAvg>() && v.fscale == FSCALE).then_some(v)
    }

    unsafe extern "C" {
        fn processor_set_default(host: u32, default_set: *mut u32) -> c_int;
        fn processor_set_statistics(
            pset: u32,
            flavor: c_int,
            info: *mut i32,
            count: *mut u32,
        ) -> c_int;
        fn notify_register_check(name: *const c_char, out_token: *mut c_int) -> u32;
        fn notify_get_state(token: c_int, state: *mut u64) -> u32;
    }

    /// `HOST_LOAD_INFO` (`mach/host_info.h`): `avenrun[3]`, `mach_factor[3]`, `LOAD_SCALE` 1000.
    const HOST_LOAD_INFO: c_int = 1;
    /// `PROCESSOR_SET_LOAD_INFO` (`mach/processor_info.h`): task and thread counts,
    /// `load_average`, `mach_factor`.
    const PROCESSOR_SET_LOAD_INFO: c_int = 4;

    /// The Mach host and processor-set ports, taken once.
    pub struct Mach {
        host: u32,
        pset: Option<u32>,
        pressure: Option<c_int>,
    }

    impl Mach {
        pub fn new() -> Self {
            // SAFETY: no arguments; returns a send right this process keeps for its life.
            let host = unsafe { libc::mach_host_self() };
            let mut pset = 0u32;
            // SAFETY: `pset` is a writable port name.
            let pset = (unsafe { processor_set_default(host, &raw mut pset) } == 0).then_some(pset);
            let mut token: c_int = 0;
            // SAFETY: a NUL-terminated name and a writable token.
            let pressure = (unsafe {
                notify_register_check(
                    c"com.apple.system.thermalpressurelevel".as_ptr(),
                    &raw mut token,
                )
            } == 0)
                .then_some(token);
            Self {
                host,
                pset,
                pressure,
            }
        }

        /// `avenrun[3]` and `mach_factor[3]`.
        pub fn host_load(&self) -> Option<[i32; 6]> {
            let mut info = [0i32; 6];
            let mut count = 6u32;
            // SAFETY: `info` holds `count` integers; the host port is this process's.
            let rc = unsafe {
                libc::host_statistics(self.host, HOST_LOAD_INFO, info.as_mut_ptr(), &raw mut count)
            };
            (rc == 0 && count == 6).then_some(info)
        }

        /// The default set's task count, thread count, `load_average` and `mach_factor`.
        pub fn pset_load(&self) -> Option<[i32; 4]> {
            let pset = self.pset?;
            let mut info = [0i32; 4];
            let mut count = 4u32;
            // SAFETY: `info` holds `count` integers; `pset` is the default set's name port.
            let rc = unsafe {
                processor_set_statistics(
                    pset,
                    PROCESSOR_SET_LOAD_INFO,
                    info.as_mut_ptr(),
                    &raw mut count,
                )
            };
            (rc == 0 && count == 4).then_some(info)
        }

        /// The thermal pressure level (0 Nominal … 4 Sleeping).
        pub fn pressure(&self) -> Option<u64> {
            let token = self.pressure?;
            let mut state = 0u64;
            // SAFETY: a token this process registered and a writable state.
            (unsafe { notify_get_state(token, &raw mut state) } == 0).then_some(state)
        }
    }
}

#[cfg(target_os = "macos")]
fn run_cadence(args: &[String]) -> i32 {
    let secs = flag_u64(args, "--secs", 30).clamp(1, 600);
    let every = Duration::from_millis(flag_u64(args, "--every-ms", 50).clamp(5, 1000));
    let Some(path) = flag(args, "--out") else {
        eprintln!("cadence: --out FILE");
        return 2;
    };
    let Ok(mut out) = std::fs::File::create(path) else {
        eprintln!("cadence: cannot write {path}");
        return 1;
    };
    let mach = mac::Mach::new();
    let t0 = Instant::now();
    let mut last: (Option<mac::LoadAvg>, Option<[i32; 6]>, Option<[i32; 4]>) = (None, None, None);
    let mut n = 0u64;
    while t0.elapsed() < Duration::from_secs(secs) {
        let now = (mac::loadavg_raw(), mach.host_load(), mach.pset_load());
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        let mut line = String::new();
        if now.0 != last.0 {
            let nrun = last
                .0
                .zip(now.0)
                .and_then(|(a, b)| nrun_of_step(a.ldavg, b.ldavg));
            let _ = write!(
                line,
                "{{\"ms\":{ms:.1},\"src\":\"vm.loadavg\",\"raw\":{},\"nrun\":{}}}",
                now.0
                    .map_or_else(|| "null".to_owned(), |l| format!("{:?}", l.ldavg)),
                nrun.map_or_else(|| "null".to_owned(), |v| v.to_string())
            );
            let _ = writeln!(out, "{line}");
            line.clear();
        }
        if now.1 != last.1 {
            let _ = writeln!(
                out,
                "{{\"ms\":{ms:.1},\"src\":\"host_load\",\"raw\":{}}}",
                now.1
                    .map_or_else(|| "null".to_owned(), |h| format!("{h:?}"))
            );
        }
        if now.2 != last.2 {
            let _ = writeln!(
                out,
                "{{\"ms\":{ms:.1},\"src\":\"pset_load\",\"raw\":{}}}",
                now.2
                    .map_or_else(|| "null".to_owned(), |p| format!("{p:?}"))
            );
        }
        last = now;
        n += 1;
        std::thread::sleep(every);
    }
    eprintln!("cadence: {n} reads over {secs} s");
    0
}

/// One process's counters at the previous reading, keyed by pid.
#[derive(Clone, Copy)]
struct Prev {
    start: Option<u64>,
    cpu: u64,
    run: u64,
    at: Instant,
}

/// What one burst did, as its thread saw it.
#[derive(Clone, Debug, Default)]
struct BurstLog {
    threads: u64,
    pid: u32,
    spawned_ms: i64,
    ready_ms: Option<i64>,
    done_ms: Option<i64>,
    exited_ms: Option<i64>,
    killed: bool,
}

#[cfg(target_os = "macos")]
fn run_record(args: &[String]) -> i32 {
    let secs = flag_u64(args, "--secs", 60).clamp(1, 3600);
    let Some(path) = flag(args, "--out") else {
        eprintln!("record: --out FILE");
        return 2;
    };
    let Ok(file) = std::fs::File::create(path) else {
        eprintln!("record: cannot write {path}");
        return 1;
    };
    let out = Arc::new(Mutex::new(std::io::BufWriter::new(file)));
    let bursts: Vec<(u64, u64, u64)> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| *a == "--burst")
        .filter_map(|(i, _)| args.get(i + 1))
        .filter_map(|spec| {
            let v: Vec<u64> = spec.split(':').filter_map(|p| p.parse().ok()).collect();
            (v.len() == 3).then(|| (v[0], v[1].min(600), v[2].clamp(1, 64)))
        })
        .collect();
    let total_load: u64 = bursts.iter().map(|b| b.1).sum();
    if total_load > 1800 {
        eprintln!("record: {total_load} s of generated load is over the unit's 30 minutes");
        return 2;
    }
    let t0 = Instant::now();
    let t0_unix = unix_ms();
    {
        let mut o = out
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = writeln!(
            o,
            "{{\"kind\":\"start\",\"unix_ms\":{t0_unix},\"bursts\":[{}],\"clusters\":[{}]}}",
            bursts
                .iter()
                .map(|(a, h, n)| format!("[{a},{h},{n}]"))
                .collect::<Vec<_>>()
                .join(","),
            aterm_sysprobe::cpu_layout()
                .clusters
                .iter()
                .map(|c| format!("[\"{}\",{}]", c.name, c.logical.len()))
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    let exe = std::env::current_exe().ok();
    let bg = args.iter().any(|a| a == "--bg");
    let mut burst_threads = Vec::new();
    for (i, &(at, hold, threads)) in bursts.iter().enumerate() {
        let (out, exe) = (Arc::clone(&out), exe.clone());
        burst_threads.push(std::thread::spawn(move || {
            let Some(exe) = exe else { return };
            let start = t0 + Duration::from_secs(at);
            if let Some(d) = start.checked_duration_since(Instant::now()) {
                std::thread::sleep(d);
            }
            let spawned = aterm_uds::spawnfd::command(&exe)
                .args(["spin", "--threads", &threads.to_string(), "--secs", &hold.to_string()])
                .args(bg.then_some("--bg"))
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .stderr(aterm_uds::spawnfd::Stdio::inherit())
                .spawn();
            let Ok(mut child) = spawned else {
                eprintln!("record: burst {i} did not start");
                return;
            };
            let mut log = BurstLog {
                threads,
                pid: child.id(),
                spawned_ms: unix_ms(),
                ..BurstLog::default()
            };
            let reader = child.stdout.take().map(BufReader::new);
            // Its own deadline ends it; past that and a grace, this kills it by its pid.
            let deadline = Instant::now() + Duration::from_secs(hold + 15);
            let lines = std::thread::spawn(move || {
                let mut seen = Vec::new();
                if let Some(r) = reader {
                    for line in r.lines().map_while(Result::ok) {
                        seen.push((line, unix_ms()));
                    }
                }
                seen
            });
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() >= deadline => {
                        let _ = child.kill();
                        let _ = child.wait();
                        log.killed = true;
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => break,
                }
            }
            log.exited_ms = Some(unix_ms());
            for (line, _at) in lines.join().unwrap_or_default() {
                let mut parts = line.split_whitespace();
                match (parts.next(), parts.next().and_then(|v| v.parse::<i64>().ok())) {
                    (Some("ready"), Some(ms)) => log.ready_ms = Some(ms),
                    (Some("done"), Some(ms)) => log.done_ms = Some(ms),
                    _ => {}
                }
            }
            {
                let mut o = out.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let _ = writeln!(
                    o,
                    "{{\"kind\":\"burst\",\"i\":{i},\"threads\":{},\"pid\":{},\"spawned_ms\":{},\"ready_ms\":{},\"done_ms\":{},\"exited_ms\":{},\"killed\":{}}}",
                    log.threads,
                    log.pid,
                    log.spawned_ms,
                    opt(log.ready_ms),
                    opt(log.done_ms),
                    opt(log.exited_ms),
                    log.killed
                );
                let _ = o.flush();
            }
        }));
    }
    let sys_thread = flag(args, "--sys-out").map(|p| {
        let p = p.to_owned();
        std::thread::spawn(move || pull_sys(&p, t0, secs))
    });

    let mach_too = args.iter().any(|a| a == "--mach");
    let mach = mac::Mach::new();
    let mut probe = aterm_sysprobe::Probe::new();
    let layout = aterm_sysprobe::cpu_layout();
    let mut last_ticks = probe.core_ticks();
    let mut last_procs: HashMap<u32, Prev> = HashMap::new();
    let mut last_ld = mac::loadavg_raw();
    let mut k = 0u64;
    loop {
        k += 1;
        let due = t0 + Duration::from_secs(k);
        if due > t0 + Duration::from_secs(secs) {
            break;
        }
        if let Some(d) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(d);
        }
        let read_start = Instant::now();
        let ticks = probe.core_ticks();
        let (busy, by_cluster) = match (&last_ticks, &ticks) {
            (Some(a), Some(b)) if a.len() == b.len() => {
                let per: Vec<f64> = a
                    .iter()
                    .zip(b)
                    .map(|(x, y)| {
                        let db = y.busy().saturating_sub(x.busy()) as f64;
                        let di = y.idle.saturating_sub(x.idle) as f64;
                        if db + di > 0.0 { db / (db + di) } else { 0.0 }
                    })
                    .collect();
                let clusters: Vec<f64> = layout
                    .clusters
                    .iter()
                    .map(|c| {
                        c.logical
                            .iter()
                            .filter_map(|&i| per.get(usize::from(i)))
                            .sum()
                    })
                    .collect();
                (Some(per.iter().sum::<f64>()), clusters)
            }
            _ => (None, Vec::new()),
        };
        last_ticks = ticks;

        let sweep_start = Instant::now();
        let sweep = aterm_sysprobe::processes();
        let sweep_us = sweep_start.elapsed().as_secs_f64() * 1e6;
        let (mut wait, mut cpu_own, mut refused, mut readable, mut fresh) =
            (0.0f64, 0.0f64, 0u32, 0u32, 0u32);
        let mut next: HashMap<u32, Prev> = HashMap::new();
        if let Some(s) = &sweep {
            for p in &s.procs {
                let (Some(cpu), Some(run), Some(at)) = (p.cpu_ns, p.runnable_ns, p.read_at) else {
                    if p.cpu_ns.is_none() {
                        refused += 1;
                    }
                    continue;
                };
                readable += 1;
                match last_procs.get(&p.pid) {
                    Some(prev) if prev.start == p.start && at > prev.at => {
                        let wall = at.duration_since(prev.at).as_nanos() as f64;
                        let d_cpu = cpu.saturating_sub(prev.cpu);
                        let d_run = run.saturating_sub(prev.run);
                        wait += d_run.saturating_sub(d_cpu) as f64 / wall;
                        cpu_own += d_cpu as f64 / wall;
                    }
                    _ => fresh += 1,
                }
                next.insert(
                    p.pid,
                    Prev {
                        start: p.start,
                        cpu,
                        run,
                        at,
                    },
                );
            }
        }
        last_procs = next;

        let ld_at = Instant::now();
        let ld = mac::loadavg_raw();
        let ld_us = ld_at.elapsed().as_secs_f64() * 1e6;
        let nrun = match (last_ld, ld) {
            (Some(a), Some(b)) if a != b => Some(nrun_of_step(a.ldavg, b.ldavg)),
            _ => None,
        };
        last_ld = ld;
        let load = aterm_sysprobe::load_average();
        let host = if mach_too { mach.host_load() } else { None };
        let ps_at = Instant::now();
        let pset = mach.pset_load();
        let ps_us = ps_at.elapsed().as_secs_f64() * 1e6;
        let pressure = mach.pressure();

        let mut line = String::with_capacity(256);
        let f3 = |v: f64| (v * 1e3).round() / 1e3;
        let _ = write!(
            line,
            "{{\"kind\":\"r\",\"t\":{:.3},\"busy\":{},\"cl\":{:?},\"wait\":{:.3},\"cpu_own\":{:.3},\"rd\":{readable},\"rf\":{refused},\"fr\":{fresh},\"load\":{},\"ld\":{},\"n\":{},\"pr\":{},\"ps\":{},\"sw_us\":{sweep_us:.0},\"ld_us\":{ld_us:.1},\"ps_us\":{ps_us:.1}",
            read_start.duration_since(t0).as_secs_f64(),
            busy.map_or_else(|| "null".to_owned(), |b| format!("{:.3}", f3(b))),
            by_cluster.iter().map(|v| f3(*v)).collect::<Vec<_>>(),
            f3(wait),
            f3(cpu_own),
            load.map_or_else(
                || "null".to_owned(),
                |l| format!("[{:.3},{:.3},{:.3}]", l[0], l[1], l[2])
            ),
            ld.map_or_else(|| "null".to_owned(), |l| format!("{:?}", l.ldavg)),
            match nrun {
                None => "null".to_owned(),
                Some(None) => "\"refused\"".to_owned(),
                Some(Some(n)) => n.to_string(),
            },
            opt(pressure.map(|p| p as i64)),
            pset.map_or_else(|| "null".to_owned(), |p| format!("[{},{}]", p[2], p[3])),
        );
        if mach_too {
            let _ = write!(
                line,
                ",\"host_load\":{}",
                host.map_or_else(|| "null".to_owned(), |h| format!("{h:?}")),
            );
        }
        line.push('}');
        let mut o = out
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = writeln!(o, "{line}");
        if k % 10 == 0 {
            let _ = o.flush();
        }
    }
    for t in burst_threads {
        let _ = t.join();
    }
    if let Some(t) = sys_thread {
        let _ = t.join();
    }
    let mut o = out
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = writeln!(o, "{{\"kind\":\"end\",\"unix_ms\":{}}}", unix_ms());
    let _ = o.flush();
    0
}

fn opt(v: Option<i64>) -> String {
    v.map_or_else(|| "null".to_owned(), |v| v.to_string())
}

/// Every 3 s, the live aterm's own reading (`aterm ctl @self sys --json`): its `cpu_mc`,
/// `wait_mc` (with its unread count), load1 and thermal state, beside the in-process readings.
fn pull_sys(path: &str, t0: Instant, secs: u64) {
    let Ok(file) = std::fs::File::create(path) else {
        eprintln!("record: cannot write {path}");
        return;
    };
    let mut out = std::io::BufWriter::new(file);
    let mut k = 0u64;
    loop {
        let due = t0 + Duration::from_secs(3 * k + 1) + Duration::from_millis(500);
        k += 1;
        if due > t0 + Duration::from_secs(secs) {
            break;
        }
        if let Some(d) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(d);
        }
        let asked = unix_ms();
        let t = t0.elapsed().as_secs_f64();
        let Ok(o) = aterm_uds::spawnfd::command("aterm")
            .args(["ctl", "@self", "sys", "--json"])
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .output()
        else {
            continue;
        };
        let text = String::from_utf8_lossy(&o.stdout);
        let Some(json) = text.lines().find(|l| l.starts_with('{')) else {
            continue;
        };
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(json) else {
            continue;
        };
        let int = |p: &str| v.pointer(p).and_then(aterm_json::Value::as_i64);
        let _ = writeln!(
            out,
            "{{\"t\":{t:.3},\"asked_ms\":{asked},\"as_of_ms\":{},\"window_ms\":{},\"cpu_mc\":{},\"wait_mc\":{},\"wait_unread\":{},\"load1_m\":{},\"thermal\":\"{}\",\"procs\":{},\"by_level\":[{},{}]}}",
            opt(int("/as_of/unix_ms")),
            opt(int("/as_of/window_ms")),
            opt(int("/global/cpu_mc")),
            opt(int("/global/wait_mc/v").or_else(|| int("/global/wait_mc"))),
            opt(int("/global/wait_mc/unread")),
            opt(int("/global/load_m/0")),
            v.pointer("/global/thermal")
                .and_then(aterm_json::Value::as_str)
                .unwrap_or("-"),
            opt(int("/global/procs")),
            opt(int("/global/cpu_by_level_mc/0")),
            opt(int("/global/cpu_by_level_mc/1")),
        );
        let _ = out.flush();
    }
}

// ---------------------------------------------------------------------------------------------
// Analysis.

#[derive(Clone, Copy, Debug, Default)]
struct Reading {
    t: f64,
    busy: f64,
    wait: f64,
    load1: f64,
    /// The kernel's runnable count, where this reading saw a step that passed the self-check.
    nrun: Option<f64>,
    /// A step was seen but the three averages disagreed.
    nrun_refused: bool,
    /// The processor set's fast load average (`sched_load_average`), runnable threads.
    pset: Option<f64>,
    pressure: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Burst {
    threads: f64,
    /// Its threads all running, seconds on the recording's clock.
    start: f64,
    /// Its threads joined (the child's own "done"), seconds on the recording's clock.
    stop: f64,
}

fn quantile(v: &[f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let pos = q * (s.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    s[lo] + (s[hi] - s[lo]) * (pos - lo as f64)
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        f64::NAN
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

fn run_analyze(args: &[String]) -> i32 {
    let Some(path) = args.get(1) else {
        eprintln!("analyze: FILE");
        return 2;
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("analyze: cannot read {path}");
        return 1;
    };
    let cores = flag_u64(args, "--cores", 10) as f64;
    let mut t0_unix = 0i64;
    let mut rs: Vec<Reading> = Vec::new();
    let mut bursts: Vec<Burst> = Vec::new();
    for line in text.lines() {
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let num = |p: &str| v.pointer(p).and_then(aterm_json::Value::as_f64);
        match v.get("kind").and_then(aterm_json::Value::as_str) {
            Some("start") => t0_unix = num("/unix_ms").unwrap_or(0.0) as i64,
            Some("burst") => {
                let at = |p: &str| num(p).map(|ms| (ms - t0_unix as f64) / 1000.0);
                if let (Some(start), Some(stop)) =
                    (at("/ready_ms"), at("/done_ms").or(at("/exited_ms")))
                {
                    bursts.push(Burst {
                        threads: num("/threads").unwrap_or(0.0),
                        start,
                        stop,
                    });
                }
            }
            Some("r") => {
                let (Some(t), Some(busy), Some(wait), Some(load1)) =
                    (num("/t"), num("/busy"), num("/wait"), num("/load/0"))
                else {
                    continue;
                };
                let step = v.get("n");
                rs.push(Reading {
                    t,
                    busy,
                    wait,
                    load1,
                    nrun: num("/n"),
                    nrun_refused: step.and_then(aterm_json::Value::as_str) == Some("refused"),
                    pressure: num("/pr").map(|p| p as i64),
                    pset: num("/ps/0").map(|v| v / 1000.0),
                });
            }
            _ => {}
        }
    }
    bursts.sort_by(|a, b| a.start.total_cmp(&b.start));
    if rs.len() < 20 {
        eprintln!("analyze: {} readings is too few", rs.len());
        return 1;
    }
    let n = rs.len();
    // Trailing 10 s means at each reading, and the kernel count held and averaged.
    let mut q10 = vec![f64::NAN; n];
    let mut busy10 = vec![f64::NAN; n];
    let mut held = vec![f64::NAN; n];
    let mut n10 = vec![f64::NAN; n];
    let mut p10 = vec![f64::NAN; n];
    let mut last_n = f64::NAN;
    let mut steps: Vec<(f64, f64)> = Vec::new();
    for i in 0..n {
        if let Some(x) = rs[i].nrun {
            last_n = x;
            steps.push((rs[i].t, x));
        }
        held[i] = last_n;
        let win: Vec<&Reading> = rs[..=i]
            .iter()
            .rev()
            .take_while(|r| r.t > rs[i].t - 10.0)
            .collect();
        q10[i] = mean(&win.iter().map(|r| r.busy + r.wait).collect::<Vec<_>>());
        busy10[i] = mean(&win.iter().map(|r| r.busy).collect::<Vec<_>>());
        p10[i] = mean(&win.iter().filter_map(|r| r.pset).collect::<Vec<_>>());
        let in_win: Vec<f64> = steps
            .iter()
            .rev()
            .take_while(|s| s.0 > rs[i].t - 10.0)
            .map(|s| s.1)
            .collect();
        n10[i] = if in_win.is_empty() {
            last_n
        } else {
            mean(&in_win)
        };
    }
    // Lag-matched Q and P: each one's mean since the kernel's previous step, put through load1's
    // own filter where the kernel stepped (each recovered step), from the first load1 reading.
    // load1 itself is the same filter over the kernel's count, so these compare like with like.
    let mut q_ewma = vec![f64::NAN; n];
    let mut p_ewma = vec![f64::NAN; n];
    {
        let (mut qa, mut pa) = (rs[0].load1, rs[0].load1);
        let e = 1884.0 / 2048.0;
        let mut since = 0usize;
        for i in 0..n {
            if rs[i].nrun.is_some() || rs[i].nrun_refused {
                let span = &rs[since..=i];
                qa = qa * e
                    + mean(&span.iter().map(|r| r.busy + r.wait).collect::<Vec<_>>()) * (1.0 - e);
                let ps: Vec<f64> = span.iter().filter_map(|r| r.pset).collect();
                if !ps.is_empty() {
                    pa = pa * e + mean(&ps) * (1.0 - e);
                }
                since = i + 1;
            }
            q_ewma[i] = qa;
            p_ewma[i] = pa;
        }
    }
    let over = |x: f64, b: f64| x > cores && b >= 0.95 * cores;
    let refused = rs.iter().filter(|r| r.nrun_refused).count();
    let mut report = String::new();
    let _ = writeln!(
        report,
        "readings {} over {:.0} s; kernel steps recovered {} (refused by the self-check {}); cores {}",
        n,
        rs[n - 1].t - rs[0].t,
        steps.len(),
        refused,
        cores
    );
    // The step cadence.
    let gaps: Vec<f64> = steps.windows(2).map(|w| w[1].0 - w[0].0).collect();
    let _ = writeln!(
        report,
        "kernel step gap s: p50 {:.2} p95 {:.2} min {:.2} max {:.2}",
        quantile(&gaps, 0.5),
        quantile(&gaps, 0.95),
        gaps.iter().copied().fold(f64::INFINITY, f64::min),
        gaps.iter().copied().fold(0.0, f64::max)
    );
    // Check that the recovered counts, put back through the kernel's filter, give load1.
    {
        let mut l = rs[0].load1;
        let mut errs = Vec::new();
        let mut started = false;
        for r in &rs {
            if let Some(x) = r.nrun {
                if started {
                    l = l * (1884.0 / 2048.0) + x * (164.0 / 2048.0);
                    errs.push((l - r.load1).abs());
                } else {
                    l = r.load1;
                    started = true;
                }
            }
        }
        let _ = writeln!(
            report,
            "refilter check |EWMA(nrun) - load1|: p50 {:.4} p95 {:.4} max {:.4} (n {})",
            quantile(&errs, 0.5),
            quantile(&errs, 0.95),
            errs.iter().copied().fold(0.0, f64::max),
            errs.len()
        );
    }
    let _ = writeln!(report, "\nbursts:");
    for (bi, b) in bursts.iter().enumerate() {
        let sel = |lo: f64, hi: f64, xs: &[f64]| -> Vec<f64> {
            rs.iter()
                .zip(xs)
                .filter(|(r, x)| r.t >= lo && r.t < hi && x.is_finite())
                .map(|(_, x)| *x)
                .collect()
        };
        let load1: Vec<f64> = rs.iter().map(|r| r.load1).collect();
        let busy: Vec<f64> = rs.iter().map(|r| r.busy).collect();
        let q1: Vec<f64> = rs.iter().map(|r| r.busy + r.wait).collect();
        let wait: Vec<f64> = rs.iter().map(|r| r.wait).collect();
        let series: [(&str, &[f64]); 6] = [
            ("Q10", &q10),
            ("P10", &p10),
            ("N10", &n10),
            ("nrun_held", &held),
            ("load1", &load1),
            ("busy10", &busy10),
        ];
        let _ = writeln!(
            report,
            "burst {bi}: {} threads, start {:.1} s, stop {:.1} s, held {:.1} s",
            b.threads,
            b.start,
            b.stop,
            b.stop - b.start
        );
        let pre = |xs: &[f64]| mean(&sel(b.start - 60.0, b.start - 1.0, xs));
        let plat = |xs: &[f64]| mean(&sel(b.stop - 60.0, b.stop - 1.0, xs));
        let _ = writeln!(
            report,
            "  pre 60 s means: busy {:.2} wait_own {:.2} Q1 {:.2} load1 {:.2} | last 60 s held: busy {:.2} wait_own {:.2} Q1 {:.2} load1 {:.2} nrun {:.2}",
            pre(&busy),
            pre(&wait),
            pre(&q1),
            pre(&load1),
            plat(&busy),
            plat(&wait),
            plat(&q1),
            plat(&load1),
            plat(&held)
        );
        for (name, xs) in series {
            let base = pre(xs);
            let top = plat(xs);
            let step = top - base;
            let after: Vec<(f64, f64)> = rs
                .iter()
                .zip(xs.iter())
                .filter(|(r, x)| r.t >= b.stop && x.is_finite())
                .map(|(r, x)| (r.t - b.stop, *x))
                .collect();
            let t90 = after
                .iter()
                .find(|(_, x)| *x <= base + 0.1 * step)
                .map(|p| p.0);
            let t50 = after
                .iter()
                .find(|(_, x)| *x <= base + 0.5 * step)
                .map(|p| p.0);
            let below = after.iter().find(|(_, x)| *x <= cores).map(|p| p.0);
            let _ = writeln!(
                report,
                "  {name:<9} base {base:6.2} top {top:6.2} step {step:6.2} | fell 50% at {} 90% at {} | <= cores at {}",
                fmt_s(t50),
                fmt_s(t90),
                fmt_s(below)
            );
        }
        // The over rule by each input: when it last held during the burst and when it cleared.
        for (name, xs) in [
            ("Q10", &q10),
            ("P10", &p10),
            ("N10", &n10),
            ("load1", &load1),
        ] {
            let during: Vec<bool> = rs
                .iter()
                .zip(xs.iter().zip(&busy10))
                .filter(|(r, _)| r.t >= b.stop - 10.0 && r.t < b.stop)
                .map(|(_, (x, bb))| over(*x, *bb))
                .collect();
            let held_share =
                during.iter().filter(|o| **o).count() as f64 / during.len().max(1) as f64;
            let clear = rs
                .iter()
                .zip(xs.iter().zip(&busy10))
                .filter(|(r, _)| r.t >= b.stop)
                .find(|(_, (x, bb))| !over(**x, **bb))
                .map(|(r, _)| r.t - b.stop);
            let clear_q = rs
                .iter()
                .zip(xs.iter())
                .filter(|(r, _)| r.t >= b.stop)
                .find(|(_, x)| **x <= cores)
                .map(|(r, _)| r.t - b.stop);
            let _ = writeln!(
                report,
                "  over by {name:<5}: held in {:.0}% of the last 10 s; cleared at {} (queue under cores at {})",
                held_share * 100.0,
                fmt_s(clear),
                fmt_s(clear_q)
            );
        }
        // Agreement in the held window, from 240 s after the start (four of load1's time
        // constants, so it is within 2 % of settled) to the stop.
        if b.stop - b.start >= 300.0 {
            let lo = b.start + 240.0;
            let win_mean = |xs: &[f64]| {
                mean(
                    &rs.iter()
                        .zip(xs)
                        .filter(|(r, x)| r.t >= lo && r.t < b.stop && x.is_finite())
                        .map(|(_, x)| *x)
                        .collect::<Vec<_>>(),
                )
            };
            let l_mean = win_mean(&load1);
            let _ = writeln!(
                report,
                "  steady {lo:.0}-{:.0} s window means: load1 {l_mean:.2}; Q10 {:.2} ({:.3} of load1); P10 {:.2} ({:.3}); N10 {:.2} ({:.3}); busy {:.2}",
                b.stop,
                win_mean(&q10),
                win_mean(&q10) / l_mean,
                win_mean(&p10),
                win_mean(&p10) / l_mean,
                win_mean(&n10),
                win_mean(&n10) / l_mean,
                win_mean(&busy)
            );
            let ratios = |xs: &[f64]| -> Vec<f64> {
                rs.iter()
                    .zip(xs)
                    .filter(|(r, x)| r.t >= lo && r.t < b.stop && x.is_finite() && r.load1 > 0.0)
                    .map(|(r, x)| x / r.load1)
                    .collect()
            };
            for (name, xs) in [
                ("Q10", &q10),
                ("P10", &p10),
                ("N10", &n10),
                ("Q_ewma", &q_ewma),
                ("P_ewma", &p_ewma),
            ] {
                let rr = ratios(xs);
                let dev: Vec<f64> = rr.iter().map(|x| (x - 1.0).abs()).collect();
                let within =
                    dev.iter().filter(|d| **d <= 0.2).count() as f64 / dev.len().max(1) as f64;
                let _ = writeln!(
                    report,
                    "  steady {:.0}-{:.0} s: {name}/load1 p5 {:.3} p50 {:.3} p95 {:.3}; |dev| p50 {:.3} p95 {:.3}; within 20% {:.0}% (n {})",
                    lo,
                    b.stop,
                    quantile(&rr, 0.05),
                    quantile(&rr, 0.5),
                    quantile(&rr, 0.95),
                    quantile(&dev, 0.5),
                    quantile(&dev, 0.95),
                    within * 100.0,
                    rr.len()
                );
            }
        }
    }
    // The short bursts together: each input's fall after the stop, averaged over the bursts so
    // the desk's own swings cancel, against the mean step the bursts made in it.
    let short: Vec<&Burst> = bursts.iter().filter(|b| b.stop - b.start < 300.0).collect();
    if short.len() >= 2 {
        let load1: Vec<f64> = rs.iter().map(|r| r.load1).collect();
        let at = |xs: &[f64], t: f64| -> Option<f64> {
            rs.iter()
                .zip(xs)
                .filter(|(r, x)| (r.t - t).abs() <= 0.6 && x.is_finite())
                .map(|(_, x)| *x)
                .next()
        };
        let span = |xs: &[f64], lo: f64, hi: f64| -> f64 {
            mean(
                &rs.iter()
                    .zip(xs)
                    .filter(|(r, x)| r.t >= lo && r.t < hi && x.is_finite())
                    .map(|(_, x)| *x)
                    .collect::<Vec<_>>(),
            )
        };
        let _ = writeln!(
            report,
            "\nensemble of {} short bursts (each input less its 60 s before the start; the step is its last 20 s before the stop):",
            short.len()
        );
        let mut curves = String::new();
        for (name, xs) in [
            ("Q10", &q10),
            ("P10", &p10),
            ("N10", &n10),
            ("nrun_held", &held),
            ("load1", &load1),
        ] {
            let steps: Vec<f64> = short
                .iter()
                .map(|b| span(xs, b.stop - 20.0, b.stop) - span(xs, b.start - 60.0, b.start - 1.0))
                .collect();
            let step = mean(&steps);
            let curve: Vec<(f64, f64)> = (0..=150)
                .filter_map(|tau| {
                    let v: Vec<f64> = short
                        .iter()
                        .filter_map(|b| {
                            at(xs, b.stop + f64::from(tau))
                                .map(|x| x - span(xs, b.start - 60.0, b.start - 1.0))
                        })
                        .collect();
                    (v.len() == short.len()).then(|| (f64::from(tau), mean(&v)))
                })
                .collect();
            let first = |frac: f64| curve.iter().find(|(_, v)| *v <= frac * step).map(|p| p.0);
            let _ = writeln!(
                report,
                "  {name:<9} step {step:6.2} (per burst {:?}) | fell 50% at {} 90% at {}",
                steps
                    .iter()
                    .map(|s| (s * 10.0).round() / 10.0)
                    .collect::<Vec<_>>(),
                fmt_s(first(0.5)),
                fmt_s(first(0.1))
            );
            let _ = write!(curves, "{name}");
            for (tau, v) in curve
                .iter()
                .filter(|(t, _)| (*t as u32) % 5 == 0 && *t <= 150.0)
            {
                let _ = write!(curves, " {tau:.0}:{:.2}", v / step);
            }
            curves.push('\n');
        }
        let _ = writeln!(
            report,
            "  normalised ensemble curves (tau s : share of the step left)\n{curves}"
        );
    }
    // Q against the machine-wide count over the whole recording, and the over decision each
    // input makes against the one the machine-wide count makes.
    {
        let pairs: Vec<(f64, f64, f64, f64)> = (0..n)
            .filter(|&i| rs[i].t > 20.0 && q10[i].is_finite() && p10[i].is_finite() && p10[i] > 0.0)
            .map(|i| (q10[i], p10[i], rs[i].load1, busy10[i]))
            .collect();
        let ratio: Vec<f64> = pairs.iter().map(|p| p.0 / p.1).collect();
        let _ = writeln!(
            report,
            "\nQ10/P10 over the recording: p1 {:.3} p5 {:.3} p50 {:.3} p95 {:.3} p99 {:.3} max {:.2} (n {})",
            quantile(&ratio, 0.01),
            quantile(&ratio, 0.05),
            quantile(&ratio, 0.5),
            quantile(&ratio, 0.95),
            quantile(&ratio, 0.99),
            ratio.iter().copied().fold(0.0, f64::max),
            ratio.len()
        );
        // How often Q misses more than a fifth of the machine's queue: what the refused
        // processes' waiting threads cost it, over the recording and in the seconds the
        // machine-wide count is over.
        let under = ratio.iter().filter(|r| **r < 0.8).count();
        let while_over: Vec<f64> = pairs
            .iter()
            .filter(|p| over(p.1, p.3))
            .map(|p| p.0 / p.1)
            .collect();
        let under_over = while_over.iter().filter(|r| **r < 0.8).count();
        let _ = writeln!(
            report,
            "Q10 more than 20% under P10: {under} of {} readings; in the seconds over by P10, {under_over} of {}",
            ratio.len(),
            while_over.len()
        );
        let q1: Vec<f64> = rs.iter().map(|r| r.busy + r.wait).collect();
        let p1: Vec<f64> = rs.iter().filter_map(|r| r.pset).collect();
        let _ = writeln!(
            report,
            "per-second Q1: p50 {:.1} p95 {:.1} p99 {:.1} max {:.1}; per-second P: p50 {:.1} p95 {:.1} p99 {:.1} max {:.1}",
            quantile(&q1, 0.5),
            quantile(&q1, 0.95),
            quantile(&q1, 0.99),
            q1.iter().copied().fold(0.0, f64::max),
            quantile(&p1, 0.5),
            quantile(&p1, 0.95),
            quantile(&p1, 0.99),
            p1.iter().copied().fold(0.0, f64::max)
        );
        for (name, k) in [("Q10", 0usize), ("load1", 2usize)] {
            let (mut both, mut only_x, mut only_p, mut neither) = (0, 0, 0, 0);
            for p in &pairs {
                let x = [p.0, p.1, p.2][k];
                match (over(x, p.3), over(p.1, p.3)) {
                    (true, true) => both += 1,
                    (true, false) => only_x += 1,
                    (false, true) => only_p += 1,
                    (false, false) => neither += 1,
                }
            }
            let _ = writeln!(
                report,
                "over by {name} against over by P10, seconds: both {both}, {name} alone {only_x}, P10 alone {only_p}, neither {neither}"
            );
        }
        // The over value itself (`strain::over`'s CPU arm: log2(x / cores) / 3 held to
        // 0.12..=1 while busy holds at 95 %), each input's against P10's, in the seconds either
        // one is over: what the dial and the pointer would draw differently.
        let value = |x: f64, b: f64| {
            if over(x, b) {
                ((x / cores).log2() / 3.0).clamp(0.12, 1.0)
            } else {
                0.0
            }
        };
        for (name, k) in [("Q10", 0usize), ("load1", 2usize)] {
            let d: Vec<f64> = pairs
                .iter()
                .filter(|p| over([p.0, p.1, p.2][k], p.3) || over(p.1, p.3))
                .map(|p| (value([p.0, p.1, p.2][k], p.3) - value(p.1, p.3)).abs())
                .collect();
            let big = d.iter().filter(|x| **x >= 0.1).count();
            let _ = writeln!(
                report,
                "over value by {name} against P10's, seconds either is over: |diff| p50 {:.3} p95 {:.3} max {:.3}; {big} of {} at 0.1 or more",
                quantile(&d, 0.5),
                quantile(&d, 0.95),
                d.iter().copied().fold(0.0, f64::max),
                d.len()
            );
        }
        // The queue draining while the cores stay busy: each second the machine-wide count (the
        // processor set's, per second) falls to the cores or under while busy10 holds at 95 % or
        // more, after at least 5 s over the cores, and stays there 10 s. From each such moment,
        // how long each input keeps over on (the busy gate the same for every input).
        let p_sec: Vec<f64> = rs.iter().map(|r| r.pset.unwrap_or(f64::NAN)).collect();
        let load1: Vec<f64> = rs.iter().map(|r| r.load1).collect();
        let events: Vec<usize> = (5..n.saturating_sub(10))
            .filter(|&i| {
                p_sec[i - 5..i].iter().all(|p| *p > cores)
                    && p_sec[i..i + 10].iter().all(|p| *p <= cores)
                    && busy10[i] >= 0.95 * cores
            })
            .collect();
        let _ = writeln!(
            report,
            "queue drained under busy cores: {} moments (per-second machine-wide count over the cores 5 s, then at or under them 10 s, busy10 >= 95 %)",
            events.len()
        );
        for (name, xs) in [
            ("Q10", &q10),
            ("P10", &p10),
            ("N10", &n10),
            ("load1", &load1),
        ] {
            let lags: Vec<f64> = events
                .iter()
                .map(|&i| {
                    (i..n)
                        .find(|&j| !over(xs[j], busy10[j]))
                        .map_or(9999.0, |j| rs[j].t - rs[i].t)
                })
                .collect();
            let finite: Vec<f64> = lags.iter().copied().filter(|l| *l < 9999.0).collect();
            let within = lags.iter().filter(|l| **l <= 10.0).count();
            let _ = writeln!(
                report,
                "  over by {name:<5} left after the drain: p50 {:.0} s p95 {:.0} s max {:.0} s; within 10 s {within} of {} (still on at the end {}); 9999 is still on at the end",
                quantile(&lags, 0.5),
                quantile(&lags, 0.95),
                finite.iter().copied().fold(0.0, f64::max),
                lags.len(),
                lags.len() - finite.len()
            );
        }
    }
    // The whole recording outside the bursts: how Q, the kernel count and load1 compare on the
    // desk's own load.
    let quiet = |t: f64| {
        bursts
            .iter()
            .all(|b| t < b.start - 5.0 || t > b.stop + 180.0)
    };
    for (name, xs) in [
        ("Q10", &q10),
        ("P10", &p10),
        ("N10", &n10),
        ("Q_ewma", &q_ewma),
        ("P_ewma", &p_ewma),
    ] {
        let rr: Vec<f64> = rs
            .iter()
            .zip(xs.iter())
            .filter(|(r, x)| quiet(r.t) && x.is_finite() && r.load1 > 0.0 && r.t > 60.0)
            .map(|(r, x)| x / r.load1)
            .collect();
        let _ = writeln!(
            report,
            "ambient (outside bursts): {name}/load1 p5 {:.3} p50 {:.3} p95 {:.3} (n {})",
            quantile(&rr, 0.05),
            quantile(&rr, 0.5),
            quantile(&rr, 0.95),
            rr.len()
        );
    }
    {
        let sweep: Vec<f64> = text
            .lines()
            .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
            .filter_map(|v| v.pointer("/sw_us").and_then(aterm_json::Value::as_f64))
            .collect();
        let cost = |key: &str| -> (f64, f64) {
            let v: Vec<f64> = text
                .lines()
                .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
                .filter_map(|v| v.pointer(key).and_then(aterm_json::Value::as_f64))
                .collect();
            (quantile(&v, 0.5), quantile(&v, 0.95))
        };
        let (ps50, ps95) = cost("/ps_us");
        let (ld50, ld95) = cost("/ld_us");
        let _ = writeln!(
            report,
            "reader cost us: processor_set_statistics p50 {ps50:.1} p95 {ps95:.1}; vm.loadavg sysctl p50 {ld50:.1} p95 {ld95:.1}"
        );
        let pressure: Vec<i64> = rs.iter().filter_map(|r| r.pressure).collect();
        let _ = writeln!(
            report,
            "process sweep us: p50 {:.0} p95 {:.0}; pressure levels seen {:?}",
            quantile(&sweep, 0.5),
            quantile(&sweep, 0.95),
            {
                let mut p = pressure.clone();
                p.sort_unstable();
                p.dedup();
                p
            }
        );
    }
    if let Some(sys) = flag(args, "--sys") {
        sys_compare(sys, t0_unix, &rs, &mut report);
    }
    print!("{report}");
    0
}

fn fmt_s(v: Option<f64>) -> String {
    v.map_or_else(|| "never".to_owned(), |s| format!("{s:.0} s"))
}

/// The live aterm's own `cpu_mc + wait_mc` against the in-process Q at the nearest reading.
fn sys_compare(path: &str, t0_unix: i64, rs: &[Reading], report: &mut String) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let mut dq = Vec::new();
    let mut dl = Vec::new();
    let mut windows = Vec::new();
    let mut sums = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for line in text.lines() {
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let get = |p: &str| v.pointer(p).and_then(aterm_json::Value::as_i64);
        let (Some(t), Some(cpu), Some(wait), Some(l1)) = (
            get("/as_of_ms").map(|ms| (ms - t0_unix) as f64 / 1000.0),
            get("/cpu_mc"),
            get("/wait_mc"),
            get("/load1_m"),
        ) else {
            continue;
        };
        if let Some(w) = get("/window_ms") {
            windows.push(w as f64);
        }
        let Some(r) = rs
            .iter()
            .min_by(|a, b| (a.t - t).abs().total_cmp(&(b.t - t).abs()))
        else {
            continue;
        };
        let q_engine = (cpu + wait) as f64 / 1000.0;
        dq.push(q_engine / (r.busy + r.wait));
        sums.0 += q_engine;
        sums.1 += r.busy + r.wait;
        sums.2 += wait as f64 / 1000.0;
        sums.3 += r.wait;
        dl.push(l1 as f64 / 1000.0 - r.load1);
    }
    let _ = writeln!(
        report,
        "live aterm engine Q (cpu_mc+wait_mc) / in-process Q1 at the reading nearest its as_of: p5 {:.3} p50 {:.3} p95 {:.3} (n {}); ratio of the means {:.3} (wait alone {:.3}); load1 diff p50 {:.3}; engine window_ms p50 {:.0}",
        quantile(&dq, 0.05),
        quantile(&dq, 0.5),
        quantile(&dq, 0.95),
        dq.len(),
        sums.0 / sums.1,
        sums.2 / sums.3,
        quantile(&dl, 0.5),
        quantile(&windows, 0.5)
    );
}
