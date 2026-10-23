// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dev-only probe child that prints nanosecond readings for each clock on every input line.

#[cfg(target_os = "macos")]
mod probe {
    use std::io::{BufRead as _, Write as _};
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    #[repr(C)]
    struct Timespec {
        tv_sec: i64,
        tv_nsec: i64,
    }

    #[repr(C)]
    struct Timeval {
        tv_sec: i64,
        tv_usec: i32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }

    unsafe extern "C" {
        fn clock_gettime(clock: u32, tp: *mut Timespec) -> i32;
        safe fn clock_gettime_nsec_np(clock: u32) -> u64;
        fn gettimeofday(tv: *mut Timeval, tz: *mut core::ffi::c_void) -> i32;
        fn time(out: *mut i64) -> i64;
        safe fn mach_continuous_time() -> u64;
        safe fn mach_continuous_approximate_time() -> u64;
        safe fn mach_absolute_time() -> u64;
        fn mach_timebase_info(info: *mut Timebase) -> i32;
    }

    const REALTIME: u32 = 0;
    const MONOTONIC_RAW: u32 = 4;
    const MONOTONIC_RAW_APPROX: u32 = 5;
    const MONOTONIC: u32 = 6;
    const UPTIME_RAW: u32 = 8;
    const UPTIME_RAW_APPROX: u32 = 9;

    fn timespec(clock: u32) -> i128 {
        let mut t = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: a timespec of our own to fill.
        let _ = unsafe { clock_gettime(clock, &raw mut t) };
        i128::from(t.tv_sec) * 1_000_000_000 + i128::from(t.tv_nsec)
    }

    /// One line of readings, in nanoseconds (`instant`: since the probe began).
    fn readings(tb: &Timebase, began: Instant) -> String {
        let ticks = |t: u64| i128::from(t) * i128::from(tb.numer) / i128::from(tb.denom);
        let mut tv = Timeval {
            tv_sec: 0,
            tv_usec: 0,
        };
        // SAFETY: a timeval of our own to fill; no zone.
        let _ = unsafe { gettimeofday(&raw mut tv, std::ptr::null_mut()) };
        // SAFETY: no out pointer.
        let secs = unsafe { time(std::ptr::null_mut()) };
        let system = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i128::try_from(d.as_nanos()).unwrap_or(0));
        let words: [(&str, i128); 14] = [
            ("realtime", timespec(REALTIME)),
            (
                "realtime_nsec_np",
                i128::from(clock_gettime_nsec_np(REALTIME)),
            ),
            (
                "gettimeofday",
                (i128::from(tv.tv_sec) * 1_000_000 + i128::from(tv.tv_usec)) * 1000,
            ),
            ("time", i128::from(secs) * 1_000_000_000),
            ("systemtime", system),
            ("monotonic", timespec(MONOTONIC)),
            ("monotonic_raw", timespec(MONOTONIC_RAW)),
            ("monotonic_raw_approx", timespec(MONOTONIC_RAW_APPROX)),
            ("continuous", ticks(mach_continuous_time())),
            (
                "continuous_approximate",
                ticks(mach_continuous_approximate_time()),
            ),
            ("uptime_raw", timespec(UPTIME_RAW)),
            ("uptime_raw_approx", timespec(UPTIME_RAW_APPROX)),
            ("absolute", ticks(mach_absolute_time())),
            (
                "instant",
                i128::try_from(began.elapsed().as_nanos()).unwrap_or(0),
            ),
        ];
        words
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub(super) fn run() {
        let mut tb = Timebase::default();
        // SAFETY: a timebase of our own to fill.
        if unsafe { mach_timebase_info(&raw mut tb) } != 0 || tb.denom == 0 {
            tb = Timebase { numer: 1, denom: 1 };
        }
        let began = Instant::now();
        let mut out = std::io::stdout().lock();
        for line in std::io::stdin().lock().lines() {
            if line.is_err() {
                return;
            }
            if writeln!(out, "{}", readings(&tb, began)).is_err() || out.flush().is_err() {
                return;
            }
        }
    }
}

fn main() {
    #[cfg(target_os = "macos")]
    probe::run();
}
