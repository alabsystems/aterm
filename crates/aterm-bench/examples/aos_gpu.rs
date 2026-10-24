// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aos_gpu --arm F|I|T|G --run-id ID --out FILE [--reps N,N,N,N] [--iso-ms N]`, design §12.9's measured child (one arm per process): FILE is one `key=value` line per [`FORMAT`] row in its order, each value running to the line's end, and `FILE.raw` holds every repetition's figures.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Duration;

use aterm_effects::int_bag_mlp::{self, Batch, ParamsView};
use aterm_effects::tone::{self, ToneScratch};
use aterm_effects::tone_int;

/// `System`, counted: the bytes live and their peak.
struct Counting;

static LIVE: AtomicU64 = AtomicU64::new(0);
static PEAK: AtomicU64 = AtomicU64::new(0);

fn grew(by: u64) {
    let live = LIVE.fetch_add(by, Relaxed).wrapping_add(by);
    PEAK.fetch_max(live, Relaxed);
}

// SAFETY: forwards arguments to `System` and updates atomic counters.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's `GlobalAlloc::alloc` contract, passed through.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grew(layout.size() as u64);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's `GlobalAlloc::alloc_zeroed` contract, passed through.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            grew(layout.size() as u64);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's `GlobalAlloc::dealloc` contract, passed through.
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size() as u64, Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller's `GlobalAlloc::realloc` contract, passed through.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            let (old, new) = (layout.size() as u64, new_size as u64);
            if new >= old {
                grew(new - old);
            } else {
                LIVE.fetch_sub(old - new, Relaxed);
            }
        }
        p
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// The summary's first value of `format=`.
const FORMAT_NAME: &str = "aos-gpu-v1";

/// The summary's keys in order and what each value is: `<s>` is a series of [`SERIES`], `<part>` a part of [`PARTS`] and `<p>` a point of [`POINTS`]; `-` stands for a figure the arm does not have, and times are nanoseconds on `timebase`.
const FORMAT: &[(&str, &str)] = &[
    (
        "entry_ticks",
        "mach_absolute_time at main's entry, the first stamp taken",
    ),
    ("format", FORMAT_NAME),
    ("arm", "F, I, T or G"),
    ("run_id", "the --run-id"),
    ("build_commit", "12 hex digits, -dirty on a dirty tree"),
    ("build_profile", "cargo's PROFILE and opt-level=OPT_LEVEL"),
    ("build_toolchain", "the first line of `$RUSTC -vV`"),
    ("debug_assertions", "true or false"),
    ("hw_model", "sysctl hw.model"),
    ("kern_osversion", "sysctl kern.osversion"),
    ("timebase", "mach_timebase_info, numer/denom"),
    ("parallelism", "available_parallelism(), T's pool"),
    (
        "qos",
        "the main thread's QoS class (hex) and relative priority",
    ),
    (
        "pool_qos_set",
        "T: pool threads that took the main thread's QoS",
    ),
    (
        "pool_qos_wanted",
        "T: pool threads asked to take it (all of them when set_qos can give its class, else 0)",
    ),
    ("reps", "repetitions of b1 b1iso b371 b16384"),
    ("iso_ms", "the gap between b1iso repetitions"),
    (
        "thermal_start",
        "com.apple.system.thermalpressurelevel at start",
    ),
    (
        "cold.model_load_ns",
        "builtin() (F), or tone_int::v1() and its check",
    ),
    ("cold.pool_ns", "T: the pool's threads made and waiting"),
    (
        "cold.device_open_ns",
        "G: ComputeDevice::open, the listing and the queue",
    ),
    (
        "cold.library_salted_ns",
        "G: Library::compile_salted, a library-cache miss",
    ),
    (
        "cold.pipeline_salted_ns",
        "G: Pipeline::new_salted, a function-cache miss",
    ),
    ("cold.params_ns", "G: Params::new"),
    (
        "cold.qualify_salted_ns",
        "G: the salted pipeline's qualification",
    ),
    (
        "cold.first_b1_salted_ns",
        "G: the first B1 job, on the salted pipeline",
    ),
    ("cold.start_cold_ns", "G: the six cold-cache steps summed"),
    (
        "cold.library_ns",
        "G: Library::compile of the canonical source",
    ),
    ("cold.pipeline_ns", "G: Pipeline::new, canonical"),
    (
        "cold.qualify_ns",
        "G: the canonical pipeline's qualification",
    ),
    (
        "cold.first_b1_ns",
        "the first B1 job (G: on the canonical pipeline)",
    ),
    ("cold.start_warm_ns", "G: the four warm-cache steps summed"),
    (
        "cold.cache_hit",
        "G: 1 when the salted pipeline took under 2 ms",
    ),
    (
        "cold.reference_ns",
        "checked forward of B371, or F's classify_opt of the 371 texts",
    ),
    ("cold.presize_ns", "every warm buffer made at B16384's size"),
    ("device_name", "G: the GPU's name"),
    ("device_registry_id", "G: its registryID"),
    ("library_salt", "G: the comment salt"),
    ("run_salt", "G: the RUN_SALT function constant"),
    ("pipeline_identity", "G: the canonical pipeline's identity"),
    (
        "threadgroup_width",
        "G: the dispatch's threads per threadgroup",
    ),
    (
        "limits",
        "G: params job domain max_buffer_length working_set, bytes in force",
    ),
    ("params_bytes", "G: tone-int v1's params blob"),
    ("job_bytes", "G: B16384's four job buffers, summed"),
    ("qualified", "G: salted canonical, 1 passed, 0 refused"),
    ("<s>.reps", "repetitions"),
    ("<s>.rows", "rows per repetition"),
    (
        "<s>.e2e_ns",
        "p50 p95 p99 by nearest rank: texts in to result rows readable",
    ),
    ("<s>.e2e_sum_ns", "the end-to-end times summed"),
    (
        "<s>.packed_ns",
        "p50 p95 p99 from the packed batch to result rows readable",
    ),
    ("<s>.packed_sum_ns", "the from-packed times summed"),
    (
        "<s>.cpu_ns",
        "RUSAGE_SELF user and system moved over the series",
    ),
    (
        "<s>.compare_ns",
        "the output memcmps against the reference: sum and max",
    ),
    (
        "<s>.wall_ns",
        "the series' first stamp to its last result, gaps included",
    ),
    (
        "<s>.split_valid",
        "G: repetitions whose ten stamps are monotone",
    ),
    (
        "<s>.part_invalid",
        "G: per part in order, repetitions where it is not monotone",
    ),
    ("<s>.part.<part>_ns", "G: p50 p95 p99 over the valid splits"),
    ("<s>.gpu_busy_ns", "G: GPUEndTime - GPUStartTime summed"),
    (
        "jobs",
        "G: jobs committed and completed outside qualification",
    ),
    (
        "declared_max",
        "the most declared buffer bytes at any point",
    ),
    (
        "mem.<p>",
        "phys_footprint, currentAllocatedSize, declared buffer bytes, heap live, heap peak",
    ),
    (
        "lifetime_max_footprint",
        "ri_lifetime_max_phys_footprint at the end",
    ),
    (
        "workload_digest",
        "tone_int::workload_digest() of the embedded texts",
    ),
    ("workload_digest_pin", "tone_int::WORKLOAD_DIGEST"),
    ("tone_int_id", "I, T, G: tone_int::id of v1"),
    ("tone_int_id_pin", "tone_int::V1_ID"),
    (
        "b371_out_digest",
        "I, T, G: out_digest of the run's reference",
    ),
    ("b371_out_digest_pin", "tone_int::B371_OUT_DIGEST"),
    (
        "f_argmax",
        "F: the run's reference, one class code per text",
    ),
    ("f_argmax_pin", "tone_int::F_ARGMAX_371"),
    (
        "thermal_end",
        "com.apple.system.thermalpressurelevel at the end",
    ),
    (
        "gpu_failure",
        "none, or the G3 failure that stopped a G run (exit 3)",
    ),
    (
        "last_ticks",
        "mach_absolute_time just before this file is written, the last stamp",
    ),
];

/// The warm phase's series in their fixed order: name, rows per repetition and registered repetitions.
const SERIES: [(&str, usize, usize); 4] = [
    ("b1", 1, 1000),
    ("b1iso", 1, 100),
    ("b371", 371, 500),
    ("b16384", 16_384, 200),
];

/// The series repeated with a gap.
const ISO: usize = 1;

/// The registered gap between isolated B1 repetitions.
const ISO_MS: u64 = 100;

/// The memory points.
const POINTS: [&str; 8] = ["P0", "P1", "P2", "P3", "P4", "P5", "P6", "P7"];

/// The split's parts in order, each from one stamp of the chain to a later one.
const PARTS: [(&str, usize, usize); 8] = [
    ("pack", 0, 1),
    ("encode", 1, 3),
    ("schedule", 3, 4),
    ("queue", 4, 5),
    ("gpu", 5, 6),
    ("notify", 6, 7),
    ("wake", 7, 8),
    ("readback", 8, 9),
];

/// A salted pipeline built faster than this hit Metal's function cache.
const COLD_PIPELINE_MIN_NS: u64 = 2_000_000;

/// Exit status of a run stopped by a G3 failure, with its summary written.
const EXIT_GPU: u8 = 3;

const USAGE: &str =
    "usage: aos_gpu --arm F|I|T|G --run-id ID --out FILE [--reps N,N,N,N] [--iso-ms N]";

/// One arm of design §12.9.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arm {
    /// The shipped `ToneModel::classify_opt` per text.
    F,
    /// `forward_fast` on one thread.
    I,
    /// `forward_fast_rows` over the pool.
    T,
    /// The GPU job.
    G,
}

impl Arm {
    fn parse(s: &str) -> Option<Arm> {
        match s {
            "F" => Some(Arm::F),
            "I" => Some(Arm::I),
            "T" => Some(Arm::T),
            "G" => Some(Arm::G),
            _ => None,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Arm::F => "F",
            Arm::I => "I",
            Arm::T => "T",
            Arm::G => "G",
        }
    }
}

/// What the child was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Args {
    arm: Arm,
    run_id: String,
    out: String,
    reps: [usize; 4],
    iso_ms: u64,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let (mut arm, mut run_id, mut out) = (None, None, None);
    let mut reps = SERIES.map(|(_, _, n)| n);
    let mut iso_ms = ISO_MS;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| format!("{flag} takes a value"))?;
        match flag.as_str() {
            "--arm" => arm = Some(Arm::parse(value).ok_or_else(|| format!("no arm {value}"))?),
            "--run-id" => {
                if value.is_empty()
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                {
                    return Err(format!(
                        "--run-id {value}: letters, digits, '.', '_' and '-'"
                    ));
                }
                run_id = Some(value.clone());
            }
            "--out" => out = Some(value.clone()),
            "--reps" => {
                let n: Vec<usize> = value
                    .split(',')
                    .map(|x| x.parse().ok().filter(|&n: &usize| n >= 1))
                    .collect::<Option<_>>()
                    .ok_or_else(|| format!("--reps {value}: four counts of at least 1"))?;
                reps = n
                    .try_into()
                    .map_err(|_| format!("--reps {value}: four counts"))?;
            }
            "--iso-ms" => {
                iso_ms = value
                    .parse()
                    .map_err(|_| format!("--iso-ms {value}: milliseconds"))?;
            }
            _ => return Err(format!("unexpected {flag}")),
        }
    }
    Ok(Args {
        arm: arm.ok_or("--arm is required")?,
        run_id: run_id.ok_or("--run-id is required")?,
        out: out.ok_or("--out is required")?,
        reps,
        iso_ms,
    })
}

/// Why a run stopped.
#[derive(Debug)]
enum Stop {
    /// A defect: a C0 mismatch, a refusal or a failure no gate voids (exit 1).
    Defect(String),
    /// A G3 failure: a job not `Completed` or a qualification refused (exit 3).
    Gpu(String),
}

type R<T> = Result<T, Stop>;

fn defect(why: impl Into<String>) -> Stop {
    Stop::Defect(why.into())
}

/// The summary, collected by key and written in [`FORMAT`]'s order.
struct Out {
    keys: Vec<String>,
    values: BTreeMap<String, String>,
}

/// [`FORMAT`]'s keys, the placeholders expanded, in the order the summary prints them.
fn format_keys() -> Vec<String> {
    let mut keys = Vec::new();
    let mut i = 0;
    while i < FORMAT.len() {
        let (k, _) = FORMAT[i];
        if k.starts_with("<s>.") {
            let end = FORMAT[i..]
                .iter()
                .position(|(k, _)| !k.starts_with("<s>."))
                .map_or(FORMAT.len(), |n| i + n);
            for (s, _, _) in SERIES {
                for (k, _) in &FORMAT[i..end] {
                    let k = k.replace("<s>", s);
                    if k.contains("<part>") {
                        keys.extend(PARTS.iter().map(|(p, _, _)| k.replace("<part>", p)));
                    } else {
                        keys.push(k);
                    }
                }
            }
            i = end;
        } else if k.contains("<p>") {
            keys.extend(POINTS.iter().map(|p| k.replace("<p>", p)));
            i += 1;
        } else {
            keys.push(k.to_owned());
            i += 1;
        }
    }
    keys
}

impl Out {
    fn new() -> Out {
        Out {
            keys: format_keys(),
            values: BTreeMap::new(),
        }
    }

    fn put(&mut self, key: impl Into<String>, value: impl std::fmt::Display) {
        let key = key.into();
        assert!(
            self.keys.contains(&key),
            "aos_gpu: `{key}` is not a FORMAT key"
        );
        let value = value.to_string().replace(['\n', '\r'], " ");
        self.values.insert(key, value);
    }

    fn cold(&mut self, step: &str, ns: u64) {
        self.put(format!("cold.{step}_ns"), ns);
    }

    fn absent(&mut self, keys: &[&str]) {
        for k in keys {
            self.put(*k, "-");
        }
    }

    fn text(&self) -> String {
        let mut s = String::new();
        for k in &self.keys {
            if let Some(v) = self.values.get(k) {
                let _ = writeln!(s, "{k}={v}");
            }
        }
        s
    }
}

/// The platform's clock, counters and facts: macOS reads them, elsewhere they read as absent.
#[cfg(target_os = "macos")]
mod os {
    use std::ffi::{CString, c_char, c_int};

    /// `struct timeval`.
    #[repr(C)]
    #[derive(Default)]
    struct Timeval {
        sec: i64,
        usec: i32,
        pad: i32,
    }

    /// `struct rusage`: two `timeval`s, then fourteen `long`s.
    #[repr(C)]
    #[derive(Default)]
    struct Rusage {
        user: Timeval,
        system: Timeval,
        rest: [i64; 14],
    }

    const RUSAGE_SELF: c_int = 0;

    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
        fn getrusage(who: c_int, usage: *mut Rusage) -> c_int;
        fn pthread_self() -> libc::pthread_t;
        fn pthread_get_qos_class_np(
            thread: libc::pthread_t,
            class: *mut u32,
            relative: *mut c_int,
        ) -> c_int;
        fn notify_register_check(name: *const c_char, token: *mut c_int) -> u32;
        fn notify_get_state(token: c_int, state: *mut u64) -> u32;
        fn notify_cancel(token: c_int) -> u32;
    }

    pub(super) fn ticks() -> u64 {
        // SAFETY: no arguments; it reads the clock.
        unsafe { mach_absolute_time() }
    }

    pub(super) fn timebase() -> Option<(u32, u32)> {
        let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: `tb` is writable for one `mach_timebase_info`.
        if unsafe { libc::mach_timebase_info(&mut tb) } != 0 || tb.denom == 0 {
            return None;
        }
        Some((tb.numer, tb.denom))
    }

    /// `phys_footprint` and `lifetime_max_phys_footprint`.
    pub(super) fn footprint() -> Option<(u64, u64)> {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        let pid = libc::c_int::try_from(std::process::id()).ok()?;
        // SAFETY: `info` is writable for one `rusage_info_v4`, the flavor asked for.
        let rc = unsafe {
            libc::proc_pid_rusage(
                pid,
                libc::RUSAGE_INFO_V4,
                info.as_mut_ptr().cast::<libc::rusage_info_t>(),
            )
        };
        if rc != 0 {
            return None;
        }
        // SAFETY: zeroed at creation, so initialized whatever the call did.
        let info = unsafe { info.assume_init() };
        Some((info.ri_phys_footprint, info.ri_lifetime_max_phys_footprint))
    }

    /// This process's user and system CPU, ns.
    pub(super) fn cpu() -> Option<(u64, u64)> {
        let mut u = Rusage::default();
        // SAFETY: `u` is writable for one `struct rusage` in the native layout, which the call does not retain.
        if unsafe { getrusage(RUSAGE_SELF, &raw mut u) } != 0 {
            return None;
        }
        let ns = |t: &Timeval| {
            u64::try_from(t.sec).unwrap_or(0) * 1_000_000_000
                + u64::try_from(t.usec).unwrap_or(0) * 1_000
        };
        Some((ns(&u.user), ns(&u.system)))
    }

    fn sysctl_raw(name: &str) -> Option<Vec<u8>> {
        let cname = CString::new(name).ok()?;
        let mut len: libc::size_t = 0;
        // SAFETY: a size query: no buffer, `len` writable, nothing set.
        let rc = unsafe {
            libc::sysctlbyname(
                cname.as_ptr(),
                std::ptr::null_mut(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len];
        // SAFETY: `buf` is `len` writable bytes and `len` says so; nothing is set.
        let rc = unsafe {
            libc::sysctlbyname(
                cname.as_ptr(),
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        buf.truncate(len);
        Some(buf)
    }

    pub(super) fn sysctl_str(name: &str) -> Option<String> {
        let mut buf = sysctl_raw(name)?;
        while buf.last() == Some(&0) {
            buf.pop();
        }
        String::from_utf8(buf).ok()
    }

    /// The thermal pressure level notify(3) posts.
    pub(super) fn thermal() -> Option<u64> {
        let mut token: c_int = 0;
        // SAFETY: a NUL-terminated name and a writable token, neither retained.
        let rc = unsafe {
            notify_register_check(
                c"com.apple.system.thermalpressurelevel".as_ptr(),
                &raw mut token,
            )
        };
        if rc != 0 {
            return None;
        }
        let mut state = 0u64;
        // SAFETY: `token` was just registered and `state` is writable.
        let rc = unsafe { notify_get_state(token, &raw mut state) };
        // SAFETY: `token` was registered above and is not used again.
        unsafe { notify_cancel(token) };
        (rc == 0).then_some(state)
    }

    /// The calling thread's QoS class and relative priority.
    pub(super) fn qos() -> Option<(u32, i32)> {
        let (mut class, mut relative) = (0u32, 0 as c_int);
        // SAFETY: the calling thread's own handle, and two writable outputs (`qos_class_t` is an unsigned int).
        let rc =
            unsafe { pthread_get_qos_class_np(pthread_self(), &raw mut class, &raw mut relative) };
        (rc == 0).then_some((class, relative))
    }

    /// Give the calling thread QoS `class` at `relative`; false for an unspecified or unknown class.
    pub(super) fn set_qos(class: u32, relative: i32) -> bool {
        let Some(class) = settable(class) else {
            return false;
        };
        // SAFETY: a known class and a priority read from a live thread; it changes only the calling thread.
        unsafe { libc::pthread_set_qos_class_self_np(class, relative) == 0 }
    }

    /// The `qos_class_t` that `set_qos` gives a thread for `class`, `None` for an unspecified or unknown one.
    pub(super) fn settable(class: u32) -> Option<libc::qos_class_t> {
        match class {
            0x21 => Some(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE),
            0x19 => Some(libc::qos_class_t::QOS_CLASS_USER_INITIATED),
            0x15 => Some(libc::qos_class_t::QOS_CLASS_DEFAULT),
            0x11 => Some(libc::qos_class_t::QOS_CLASS_UTILITY),
            0x09 => Some(libc::qos_class_t::QOS_CLASS_BACKGROUND),
            _ => None,
        }
    }
}

/// The platform's clock, counters and facts: macOS reads them, elsewhere they read as absent.
#[cfg(not(target_os = "macos"))]
mod os {
    use std::sync::OnceLock;
    use std::time::Instant;

    pub(super) fn ticks() -> u64 {
        static EPOCH: OnceLock<Instant> = OnceLock::new();
        u64::try_from(EPOCH.get_or_init(Instant::now).elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    pub(super) fn timebase() -> Option<(u32, u32)> {
        Some((1, 1))
    }

    pub(super) fn footprint() -> Option<(u64, u64)> {
        None
    }

    pub(super) fn cpu() -> Option<(u64, u64)> {
        None
    }

    pub(super) fn sysctl_str(_name: &str) -> Option<String> {
        None
    }

    pub(super) fn thermal() -> Option<u64> {
        None
    }

    pub(super) fn qos() -> Option<(u32, i32)> {
        None
    }

    pub(super) fn set_qos(_class: u32, _relative: i32) -> bool {
        false
    }

    pub(super) fn settable(_class: u32) -> Option<()> {
        None
    }
}

/// Ticks to nanoseconds on one timebase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Clock {
    numer: u32,
    denom: u32,
}

impl Clock {
    fn ns(self, ticks: u64) -> u64 {
        let ns = u128::from(ticks) * u128::from(self.numer) / u128::from(self.denom);
        u64::try_from(ns).unwrap_or(u64::MAX)
    }

    fn since(self, from: u64) -> u64 {
        self.ns(os::ticks().saturating_sub(from))
    }
}

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "-".to_owned(), |v| v.to_string())
}

/// The value of rank `p` percent by nearest rank of sorted `v`.
fn nearest_rank(v: &[u64], p: usize) -> Option<u64> {
    let n = v.len();
    if n == 0 {
        return None;
    }
    let rank = (p * n).div_ceil(100).max(1);
    v.get(rank - 1).copied()
}

/// `p50 p95 p99` of `v`, or `-`.
fn three(mut v: Vec<u64>) -> String {
    v.sort_unstable();
    match (
        nearest_rank(&v, 50),
        nearest_rank(&v, 95),
        nearest_rank(&v, 99),
    ) {
        (Some(a), Some(b), Some(c)) => format!("{a} {b} {c}"),
        _ => "-".to_owned(),
    }
}

/// One repetition: its stamps in ns on the run's clock, and its compare time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Rep {
    /// Texts handed in.
    t0: u64,
    /// The packed batch made.
    packed: Option<u64>,
    /// Result rows readable.
    read: u64,
    /// G's chain: t0, loaded, before encode, after commit, scheduled, GPU start, GPU end, handler, woke, read.
    chain: Option<[Option<u64>; 10]>,
    /// The memcmp after it.
    compare: u64,
}

/// Whether part `k` of `chain` is monotone, before-encode included in the encode part.
fn part_valid(chain: &[Option<u64>; 10], k: usize) -> bool {
    let (_, a, b) = PARTS[k];
    let ordered = |x: usize, y: usize| matches!((chain[x], chain[y]), (Some(x), Some(y)) if x <= y);
    ordered(a, b) && (k != 1 || (ordered(1, 2) && ordered(2, 3)))
}

/// One series' repetitions and what moved around it.
struct Measured {
    reps: Vec<Rep>,
    cpu: Option<(u64, u64)>,
}

/// Write series `s`'s figures.
fn put_series(o: &mut Out, s: usize, m: &Measured) {
    let (name, rows, _) = SERIES[s];
    let key = |k: &str| format!("{name}.{k}");
    o.put(key("reps"), m.reps.len());
    o.put(key("rows"), rows);
    let e2e: Vec<u64> = m.reps.iter().map(|r| r.read.saturating_sub(r.t0)).collect();
    o.put(key("e2e_sum_ns"), e2e.iter().sum::<u64>());
    o.put(key("e2e_ns"), three(e2e));
    let packed: Option<Vec<u64>> = m
        .reps
        .iter()
        .map(|r| r.packed.map(|p| r.read.saturating_sub(p)))
        .collect();
    match packed {
        Some(p) => {
            o.put(key("packed_sum_ns"), p.iter().sum::<u64>());
            o.put(key("packed_ns"), three(p));
        }
        None => {
            o.put(key("packed_sum_ns"), "-");
            o.put(key("packed_ns"), "-");
        }
    }
    o.put(
        key("cpu_ns"),
        m.cpu
            .map_or_else(|| "-".to_owned(), |(u, s)| format!("{u} {s}")),
    );
    let compares = m.reps.iter().map(|r| r.compare);
    o.put(
        key("compare_ns"),
        format!(
            "{} {}",
            compares.clone().sum::<u64>(),
            compares.max().unwrap_or(0)
        ),
    );
    let wall = match (m.reps.first(), m.reps.last()) {
        (Some(a), Some(b)) => b.read.saturating_sub(a.t0),
        _ => 0,
    };
    o.put(key("wall_ns"), wall);
    let chains: Option<Vec<[Option<u64>; 10]>> = m.reps.iter().map(|r| r.chain).collect();
    let Some(chains) = chains else {
        for k in ["split_valid", "part_invalid", "gpu_busy_ns"] {
            o.put(key(k), "-");
        }
        for (p, _, _) in PARTS {
            o.put(key(&format!("part.{p}_ns")), "-");
        }
        return;
    };
    let valid: Vec<&[Option<u64>; 10]> = chains
        .iter()
        .filter(|c| (0..PARTS.len()).all(|k| part_valid(c, k)))
        .collect();
    o.put(key("split_valid"), valid.len());
    let invalid: Vec<String> = (0..PARTS.len())
        .map(|k| {
            chains
                .iter()
                .filter(|c| !part_valid(c, k))
                .count()
                .to_string()
        })
        .collect();
    o.put(key("part_invalid"), invalid.join(" "));
    for (p, a, b) in PARTS {
        let v: Vec<u64> = valid
            .iter()
            .filter_map(|c| Some(c[b]?.saturating_sub(c[a]?)))
            .collect();
        o.put(key(&format!("part.{p}_ns")), three(v));
    }
    let busy: u64 = chains
        .iter()
        .filter(|c| part_valid(c, 4))
        .filter_map(|c| Some(c[6]?.saturating_sub(c[5]?)))
        .sum();
    o.put(key("gpu_busy_ns"), busy);
}

/// The sidecar's text: one tab-separated line per repetition, stamps as ns after its t0.
fn raw_text(a: &Args, all: &[(usize, Measured)]) -> String {
    let mut s = format!(
        "# aos-gpu-raw-v1 arm={} run_id={}\nseries\trep\tt0_ns\te2e_ns\tpacked_ns\tcompare_ns\tloaded\tbefore_encode\tafter_commit\tscheduled\tgpu_start\tgpu_end\thandler\twoke\n",
        a.arm.name(),
        a.run_id
    );
    for (k, m) in all {
        for (i, r) in m.reps.iter().enumerate() {
            let _ = write!(
                s,
                "{}\t{i}\t{}\t{}\t{}\t{}",
                SERIES[*k].0,
                r.t0,
                r.read.saturating_sub(r.t0),
                opt(r.packed.map(|p| r.read.saturating_sub(p))),
                r.compare
            );
            for j in 1..9 {
                let off = r
                    .chain
                    .and_then(|c| c[j])
                    .map(|v| i128::from(v) - i128::from(r.t0));
                let _ = write!(s, "\t{}", opt(off));
            }
            s.push('\n');
        }
    }
    s
}

/// One memory point's readings.
#[derive(Clone, Copy, Debug)]
struct Reading {
    footprint: Option<u64>,
    metal: Option<u64>,
    declared: u64,
    live: u64,
    peak: u64,
}

impl Reading {
    /// `phys_footprint` and the heap now, beside the given Metal and declared bytes.
    fn now(metal: Option<u64>, declared: u64) -> Reading {
        Reading {
            footprint: os::footprint().map(|f| f.0),
            metal,
            declared,
            live: LIVE.load(Relaxed),
            peak: PEAK.load(Relaxed),
        }
    }
}

/// Main's entry: its stamp and P0, read before the arguments are parsed or the workload built.
#[derive(Clone, Copy, Debug)]
struct Entry {
    ticks: u64,
    p0: Reading,
}

impl Entry {
    fn now() -> Entry {
        let ticks = os::ticks();
        Entry {
            ticks,
            p0: Reading::now(None, 0),
        }
    }
}

/// What every arm shares: the clock, the workload, the summary and the memory points.
struct Run<'a> {
    a: &'a Args,
    clock: Clock,
    o: Out,
    declared_max: u64,
    texts: [Vec<&'static str>; 3],
    measured: Vec<(usize, Measured)>,
}

impl Run<'_> {
    /// Record point `p`, read now.
    fn point(&mut self, p: usize, metal: Option<u64>, declared: u64) {
        self.put_point(p, Reading::now(metal, declared));
    }

    /// Record point `p` as `at` read it.
    fn put_point(&mut self, p: usize, at: Reading) {
        self.declared_max = self.declared_max.max(at.declared);
        self.o.put(
            format!("mem.{}", POINTS[p]),
            format!(
                "{} {} {} {} {}",
                opt(at.footprint),
                opt(at.metal),
                at.declared,
                at.live,
                at.peak
            ),
        );
    }

    /// Run series `s` of `n` repetitions, `one` doing each; the isolated series sleeps its gap before every repetition, its first included, so none follows the series before it back to back.
    fn series(&mut self, s: usize, mut one: impl FnMut(&[&'static str]) -> R<Rep>) -> R<()> {
        let n = self.a.reps[s];
        let gap = (s == ISO).then(|| Duration::from_millis(self.a.iso_ms));
        let texts = self.texts[ref_of(s)].clone();
        let mut reps = Vec::with_capacity(n);
        let before = os::cpu();
        for _ in 0..n {
            if let Some(g) = gap {
                std::thread::sleep(g);
            }
            reps.push(one(&texts)?);
        }
        let after = os::cpu();
        let cpu = before
            .zip(after)
            .map(|(b, a)| (a.0.saturating_sub(b.0), a.1.saturating_sub(b.1)));
        let m = Measured { reps, cpu };
        put_series(&mut self.o, s, &m);
        self.measured.push((s, m));
        Ok(())
    }
}

/// Which reference a series compares with: B1, B371 or B16384.
const fn ref_of(s: usize) -> usize {
    match s {
        0 | 1 => 0,
        2 => 1,
        _ => 2,
    }
}

/// The three references of one row reference `per_text` (stride words per text): B1 (row 0), B371, and B16384 (row i = row i mod 371).
fn references<T: Copy>(per_text: &[T], stride: usize) -> [Vec<T>; 3] {
    let rows = per_text.len() / stride.max(1);
    let big = SERIES[3].1;
    let b16384 = (0..big)
        .flat_map(|i| {
            per_text[(i % rows) * stride..(i % rows + 1) * stride]
                .iter()
                .copied()
        })
        .collect();
    [per_text[..stride].to_vec(), per_text.to_vec(), b16384]
}

/// The first output position where `got` and `want` differ, or a length mismatch.
fn first_difference<T: PartialEq>(got: &[T], want: &[T]) -> Option<usize> {
    if got.len() != want.len() {
        return Some(got.len().min(want.len()));
    }
    got.iter().zip(want).position(|(g, w)| g != w)
}

/// Compare `got` with `want` for repetition `rep` of series `s`, its duration in ns.
fn compare<T: PartialEq + std::fmt::Debug>(
    clock: Clock,
    s: usize,
    rep: usize,
    stride: usize,
    got: &[T],
    want: &[T],
) -> R<u64> {
    let t = os::ticks();
    let same = got == want;
    let ns = clock.since(t);
    if same {
        return Ok(ns);
    }
    let at = first_difference(got, want).unwrap_or(0);
    Err(defect(format!(
        "C0 mismatch: series {} repetition {rep} row {} word {}: got {:?}, want {:?}",
        SERIES[s].0,
        at / stride.max(1),
        at % stride.max(1),
        got.get(at),
        want.get(at)
    )))
}

fn check_first<T: PartialEq + std::fmt::Debug>(what: &str, got: &[T], want: &[T]) -> R<()> {
    match first_difference(got, want) {
        None => Ok(()),
        Some(at) => Err(defect(format!(
            "C0 mismatch: {what} word {at}: got {:?}, want {:?}",
            got.get(at),
            want.get(at)
        ))),
    }
}

/// The shipped classifier's verdicts as class codes.
fn classify_into(model: &tone::ToneModel, s: &mut ToneScratch, texts: &[&str], out: &mut [u8]) {
    for (slot, text) in out.iter_mut().zip(texts) {
        *slot = tone_int::class_code(model.classify_opt(text, s));
    }
}

fn arm_f(r: &mut Run<'_>) -> R<()> {
    let t = os::ticks();
    let model = tone::builtin().ok_or_else(|| defect("the shipped tone asset did not load"))?;
    r.o.cold("model_load", r.clock.since(t));
    r.point(1, None, 0);
    let mut scratch = ToneScratch::default();
    let t = os::ticks();
    let mut first = [0u8; 1];
    classify_into(model, &mut scratch, &[tone_int::B1_TEXT], &mut first);
    r.o.cold("first_b1", r.clock.since(t));
    let mut per_text = vec![0u8; r.texts[1].len()];
    let t = os::ticks();
    classify_into(model, &mut scratch, &r.texts[1], &mut per_text);
    r.o.cold("reference", r.clock.since(t));
    let digits: String = per_text.iter().map(|c| char::from(b'0' + c)).collect();
    r.o.put("f_argmax", digits);
    let refs = references(&per_text, 1);
    check_first("F's first B1", &first, &refs[0])?;
    let t = os::ticks();
    let mut out = vec![0u8; SERIES[3].1];
    r.o.cold("presize", r.clock.since(t));
    r.point(5, None, out.len() as u64);
    let clock = r.clock;
    for s in 0..SERIES.len() {
        let want = &refs[ref_of(s)];
        let mut rep = 0;
        r.series(s, |texts| {
            let t0 = os::ticks();
            let n = texts.len();
            classify_into(model, &mut scratch, texts, &mut out[..n]);
            let read = os::ticks();
            let c = compare(clock, s, rep, 1, &out[..n], want)?;
            rep += 1;
            Ok(Rep {
                t0: clock.ns(t0),
                packed: None,
                read: clock.ns(read),
                chain: None,
                compare: c,
            })
        })?;
    }
    r.point(6, None, out.len() as u64);
    drop(out);
    r.point(7, None, 0);
    Ok(())
}

/// The bytes of a batch's two vectors and an output buffer, as allocated.
fn host_bytes(parts: Option<&(Vec<u32>, Vec<u16>)>, out: usize) -> u64 {
    parts.map_or(0, |(o, i)| (o.capacity() * 4 + i.capacity() * 2) as u64) + out as u64 * 4
}

fn params(r: &mut Run<'_>) -> R<(&'static [u8], ParamsView<'static>)> {
    let t = os::ticks();
    let bytes = tone_int::v1().map_err(|e| defect(format!("tone-int v1: {e:?}")))?;
    let view = int_bag_mlp::check(bytes).map_err(|e| defect(format!("tone-int v1: {e:?}")))?;
    r.o.cold("model_load", r.clock.since(t));
    r.o.put("tone_int_id", tone_int::id(&view).hex());
    Ok((bytes, view))
}

fn pack(view: &ParamsView<'_>, texts: &[&str], parts: Option<(Vec<u32>, Vec<u16>)>) -> R<Batch> {
    let (offsets, ids) = parts.unwrap_or_default();
    tone_int::pack_into(view, texts, offsets, ids).map_err(|e| defect(format!("pack: {e:?}")))
}

/// What I and T fill their output with before each forward, as G's `load` fills its job's, so a word the forward failed to write cannot pass C0: it is no class, and as an i32 no logit within `127²·H_MAX + B2_LIMIT`.
const OUT_POISON: u32 = 0xa5a5_a5a5;

/// I's and T's warm buffers, made once at B16384's size and reused by every repetition.
struct Warm<'v> {
    view: ParamsView<'v>,
    clock: Clock,
    out: Vec<u32>,
    parts: Option<(Vec<u32>, Vec<u16>)>,
}

impl Warm<'_> {
    /// The bytes its buffers hold, as allocated.
    fn bytes(&self) -> u64 {
        host_bytes(self.parts.as_ref(), self.out.len())
    }

    /// Repetition `rep` of series `s`: pack `texts`, fill the output with [`OUT_POISON`], `forward` into it and compare it with `want`.
    fn rep(
        &mut self,
        s: usize,
        rep: usize,
        texts: &[&str],
        want: &[u32],
        forward: impl FnOnce(&Batch, &mut [u32]) -> R<()>,
    ) -> R<Rep> {
        let t0 = os::ticks();
        let batch = pack(&self.view, texts, self.parts.take())?;
        let packed = os::ticks();
        let n = batch.out_words();
        let out = &mut self.out[..n];
        out.fill(OUT_POISON);
        forward(&batch, out)?;
        let read = os::ticks();
        self.parts = Some(batch.into_parts());
        let stride = n / texts.len().max(1);
        let c = compare(self.clock, s, rep, stride, out, want)?;
        Ok(Rep {
            t0: self.clock.ns(t0),
            packed: Some(self.clock.ns(packed)),
            read: self.clock.ns(read),
            chain: None,
            compare: c,
        })
    }
}

/// The run's reference, checked `forward` of B371, timed, with its digest written.
fn reference(r: &mut Run<'_>, view: &ParamsView<'_>) -> R<[Vec<u32>; 3]> {
    let batch = pack(view, &r.texts[1], None)?;
    let mut words = vec![0u32; batch.out_words()];
    let t = os::ticks();
    int_bag_mlp::forward(view, &batch, &mut words)
        .map_err(|e| defect(format!("forward: {e:?}")))?;
    r.o.cold("reference", r.clock.since(t));
    r.o.put("b371_out_digest", int_bag_mlp::out_digest(&words).hex());
    Ok(references(&words, batch.out_stride() as usize))
}

fn arm_cpu(r: &mut Run<'_>, threads: bool) -> R<()> {
    let (_, view) = params(r)?;
    r.point(1, None, 0);
    let pool = if threads {
        let t = os::ticks();
        let pool = pool::Pool::new(view, aterm_parallelism()).map_err(defect)?;
        r.o.cold("pool", r.clock.since(t));
        r.o.put("pool_qos_set", pool.qos_set());
        r.o.put("pool_qos_wanted", pool.qos_wanted());
        Some(pool)
    } else {
        r.o.absent(&["cold.pool_ns", "pool_qos_set", "pool_qos_wanted"]);
        None
    };
    let forward = |batch: &Batch, out: &mut [u32]| -> R<()> {
        match &pool {
            Some(p) => p.run(batch, out).map_err(defect),
            None => int_bag_mlp::forward_fast(&view, batch, out)
                .map_err(|e| defect(format!("forward_fast: {e:?}"))),
        }
    };
    let t = os::ticks();
    let b1 = pack(&view, &[tone_int::B1_TEXT], None)?;
    let mut first = vec![0u32; b1.out_words()];
    forward(&b1, &mut first)?;
    r.o.cold("first_b1", r.clock.since(t));
    let refs = reference(r, &view)?;
    check_first("the first B1 job", &first, &refs[0])?;
    let t = os::ticks();
    let big = pack(&view, &r.texts[2], None)?;
    let mut warm = Warm {
        view,
        clock: r.clock,
        out: vec![0u32; big.out_words()],
        parts: Some(big.into_parts()),
    };
    r.o.cold("presize", r.clock.since(t));
    r.point(5, None, warm.bytes());
    for s in 0..SERIES.len() {
        let want = &refs[ref_of(s)];
        let mut rep = 0;
        r.series(s, |texts| {
            let one = warm.rep(s, rep, texts, want, forward)?;
            rep += 1;
            Ok(one)
        })?;
    }
    r.point(6, None, warm.bytes());
    drop(pool);
    drop(warm);
    r.point(7, None, 0);
    Ok(())
}

fn aterm_parallelism() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

/// T's pool: threads made once, at the main thread's QoS, each claiming row chunks of a job through one atomic cursor.
mod pool {
    use std::panic::AssertUnwindSafe;
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
    use std::thread::JoinHandle;

    use aterm_effects::int_bag_mlp::{self, Batch, ParamsView};

    /// One job's inputs as the pool threads see them.
    #[derive(Clone, Copy)]
    struct Work {
        batch: *const Batch,
        out: *mut u32,
        rows: u32,
        stride: usize,
        chunk: u32,
    }

    // SAFETY: a pool thread dereferences a `Work` only between `Pool::run` publishing it and that thread's decrement of `busy`, and `run` holds the `&Batch` and the `&mut [u32]` the pointers came from until every thread has decremented, so both pointees are live, the batch is not written, and `out` is written only by the pool threads, each in the disjoint rows the cursor gave it.
    unsafe impl Send for Work {}

    #[derive(Default)]
    struct State {
        generation: u64,
        work: Option<Work>,
        busy: usize,
        ready: usize,
        stop: bool,
        failed: Option<String>,
    }

    struct Shared {
        view: ParamsView<'static>,
        state: Mutex<State>,
        go: Condvar,
        done: Condvar,
        cursor: AtomicU32,
        qos_set: AtomicUsize,
    }

    impl Shared {
        fn lock(&self) -> MutexGuard<'_, State> {
            self.state.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }

    /// The pool.
    pub(super) struct Pool {
        shared: Arc<Shared>,
        threads: Vec<JoinHandle<()>>,
        wanted: usize,
    }

    impl Pool {
        /// `n` threads at the calling thread's QoS, returned once every one waits for work.
        pub(super) fn new(view: ParamsView<'static>, n: usize) -> Result<Pool, String> {
            let shared = Arc::new(Shared {
                view,
                state: Mutex::new(State::default()),
                go: Condvar::new(),
                done: Condvar::new(),
                cursor: AtomicU32::new(0),
                qos_set: AtomicUsize::new(0),
            });
            let qos = super::os::qos();
            let mut threads = Vec::with_capacity(n);
            for i in 0..n.max(1) {
                let s = Arc::clone(&shared);
                let t = std::thread::Builder::new()
                    .name(format!("aos-gpu-pool-{i}"))
                    .spawn(move || worker(&s, qos))
                    .map_err(|e| format!("pool thread {i}: {e}"))?;
                threads.push(t);
            }
            let wanted = if qos.is_some_and(|(class, _)| super::os::settable(class).is_some()) {
                threads.len()
            } else {
                0
            };
            let pool = Pool {
                shared,
                threads,
                wanted,
            };
            let mut g = pool.shared.lock();
            while g.ready < pool.threads.len() {
                g = pool
                    .shared
                    .done
                    .wait(g)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            drop(g);
            Ok(pool)
        }

        /// How many threads took the main thread's QoS.
        pub(super) fn qos_set(&self) -> usize {
            self.shared.qos_set.load(Ordering::Relaxed)
        }

        /// How many threads were asked to take it: all of them when `set_qos` can give its class, else none.
        pub(super) fn qos_wanted(&self) -> usize {
            self.wanted
        }

        /// `forward_fast_rows` of every row of `batch` into `out`, in chunks of at least a quarter of each thread's share.
        pub(super) fn run(&self, batch: &Batch, out: &mut [u32]) -> Result<(), String> {
            let rows = batch.rows();
            let stride = batch.out_stride() as usize;
            if out.len() != rows as usize * stride {
                return Err(format!(
                    "the pool's output holds {} words, not {rows} rows of {stride}",
                    out.len()
                ));
            }
            let chunk = super::chunk(rows, self.threads.len());
            self.shared.cursor.store(0, Ordering::Relaxed);
            {
                let mut g = self.shared.lock();
                g.work = Some(Work {
                    batch: std::ptr::from_ref(batch),
                    out: out.as_mut_ptr(),
                    rows,
                    stride,
                    chunk,
                });
                g.generation += 1;
                g.busy = self.threads.len();
                g.failed = None;
            }
            self.shared.go.notify_all();
            let mut g = self.shared.lock();
            while g.busy > 0 {
                g = self
                    .shared
                    .done
                    .wait(g)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            g.work = None;
            g.failed.take().map_or(Ok(()), Err)
        }
    }

    impl Drop for Pool {
        fn drop(&mut self) {
            self.shared.lock().stop = true;
            self.shared.go.notify_all();
            for t in self.threads.drain(..) {
                let _ = t.join();
            }
        }
    }

    fn worker(s: &Shared, qos: Option<(u32, i32)>) {
        if qos.is_some_and(|(class, relative)| super::os::set_qos(class, relative)) {
            s.qos_set.fetch_add(1, Ordering::Relaxed);
        }
        let mut seen = 0;
        {
            let mut g = s.lock();
            g.ready += 1;
            s.done.notify_all();
            drop(g);
        }
        loop {
            let work = {
                let mut g = s.lock();
                while g.generation == seen && !g.stop {
                    g = s.go.wait(g).unwrap_or_else(PoisonError::into_inner);
                }
                if g.stop {
                    return;
                }
                seen = g.generation;
                g.work
            };
            let failed = work.and_then(|w| {
                std::panic::catch_unwind(AssertUnwindSafe(|| rows(s, w)))
                    .unwrap_or_else(|_| Some("a pool thread panicked".to_owned()))
            });
            let mut g = s.lock();
            if let Some(f) = failed {
                g.failed.get_or_insert(f);
            }
            g.busy -= 1;
            if g.busy == 0 {
                s.done.notify_all();
            }
        }
    }

    /// Claim chunks of `w` until none is left.
    fn rows(s: &Shared, w: Work) -> Option<String> {
        loop {
            let start = s.cursor.fetch_add(w.chunk, Ordering::Relaxed);
            if start >= w.rows {
                return None;
            }
            let end = start.saturating_add(w.chunk).min(w.rows);
            // SAFETY: as `Work`'s `Send`: the pointees are live and unshared for this job; `start..end` lies within the batch's rows, so its words lie within `out`'s `rows × stride`, and the cursor gave these rows to this thread alone.
            let (batch, out) = unsafe {
                (
                    &*w.batch,
                    std::slice::from_raw_parts_mut(
                        w.out.add(start as usize * w.stride),
                        (end - start) as usize * w.stride,
                    ),
                )
            };
            if let Err(e) = int_bag_mlp::forward_fast_rows(&s.view, batch, start..end, out) {
                return Some(format!("forward_fast_rows: {e:?}"));
            }
        }
    }
}

/// T's chunk: at least four chunks per thread, at least one row each.
fn chunk(rows: u32, threads: usize) -> u32 {
    let per = u32::try_from(threads.max(1).saturating_mul(4)).unwrap_or(u32::MAX);
    (rows / per).max(1)
}

/// The salts a run derives from its id: the library's comment salt, and a RUN_SALT at or above `min`.
#[cfg(any(test, target_os = "macos"))]
fn salts(run_id: &str, min: u32) -> (u64, u32) {
    let d = int_bag_mlp::Digest::of(b"aterm-bench-aos-gpu-salt-v1\0", &[run_id.as_bytes()]).0;
    let lib = u64::from_le_bytes([d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]);
    let x = u32::from_le_bytes([d[8], d[9], d[10], d[11]]);
    (lib, min + x % (u32::MAX - min))
}

#[cfg(target_os = "macos")]
mod gpu {
    use aterm_effects::int_bag_mlp::ParamsView;
    use aterm_effects::tone_int;
    use aterm_gpu::compute::{
        Completed, ComputeDevice, Job, JobFailure, Library, Params, Pipeline, PipelineError,
        Prepared, RUN_SALT_MIN, Shape, SubmitRefusal, current_allocated_size_fresh,
    };

    use super::{COLD_PIPELINE_MIN_NS, R, Rep, Run, SERIES, Stop, defect, os, ref_of};

    fn failed(e: JobFailure) -> Stop {
        Stop::Gpu(format!("a job ended {e:?}"))
    }

    fn refused(r: SubmitRefusal) -> Stop {
        defect(format!("submit refused: {r:?}"))
    }

    fn unqualified(which: &str, e: PipelineError) -> Stop {
        Stop::Gpu(format!("the {which} pipeline's qualification: {e:?}"))
    }

    /// A job between repetitions.
    enum Held {
        Prepared(Job<Prepared>),
        Done(Job<Completed>),
    }

    /// One B1 job on `pipeline`: its output words and the completed job.
    fn first_b1(
        dev: &ComputeDevice,
        params: &std::sync::Arc<Params>,
        pipeline: &Pipeline,
        view: &ParamsView<'_>,
        jobs: &mut (u64, u64),
    ) -> R<(Vec<u32>, Job<Completed>)> {
        let batch = super::pack(view, &[tone_int::B1_TEXT], None)?;
        let mut job = Job::new(dev, params);
        job.load(&batch)
            .map_err(|e| defect(format!("load: {e:?}")))?;
        let flight = job.submit(pipeline).map_err(|(_, r)| refused(r))?;
        jobs.0 += 1;
        let done = flight.wait().map_err(failed)?;
        jobs.1 += 1;
        Ok((done.out().to_vec(), done))
    }

    fn secs_ns(s: f64) -> Option<u64> {
        (s.is_finite() && s >= 0.0).then(|| (s * 1e9).round() as u64)
    }

    pub(super) fn arm_g(r: &mut Run<'_>) -> R<()> {
        let (bytes, view) = super::params(r)?;
        r.point(1, None, 0);
        let (library_salt, run_salt) = super::salts(&r.a.run_id, RUN_SALT_MIN);
        r.o.put("library_salt", library_salt);
        r.o.put("run_salt", run_salt);
        r.o.put("params_bytes", bytes.len());
        let d = view.dims();
        let shape = Shape {
            embed: d.embed,
            hidden: d.hidden,
            classes: d.classes,
            min_ids: view.min_ids(),
        };
        let declared = aterm_objc::autoreleasepool(|_| -> R<u64> {
            let clock = r.clock;
            let mut jobs = (0u64, 0u64);
            let t = os::ticks();
            let dev =
                ComputeDevice::open().map_err(|e| defect(format!("no compute device: {e:?}")))?;
            let open = clock.since(t);
            r.o.cold("device_open", open);
            r.o.put("device_name", dev.name());
            r.o.put("device_registry_id", dev.registry_id());
            let l = dev.limits();
            r.o.put(
                "limits",
                format!(
                    "{} {} {} {} {}",
                    l.params, l.job, l.domain, l.max_buffer_length, l.working_set
                ),
            );
            r.point(2, Some(dev.current_allocated_size()), dev.live_bytes());
            let t = os::ticks();
            let lib_salted = Library::compile_salted(&dev, library_salt)
                .map_err(|e| defect(format!("the salted library: {e:?}")))?;
            let lib_salted_ns = clock.since(t);
            let t = os::ticks();
            let salted = Pipeline::new_salted(&lib_salted, shape, run_salt)
                .map_err(|e| defect(format!("the salted pipeline: {e:?}")))?;
            let salted_ns = clock.since(t);
            r.o.cold("library_salted", lib_salted_ns);
            r.o.cold("pipeline_salted", salted_ns);
            r.o.put("cold.cache_hit", u8::from(salted_ns < COLD_PIPELINE_MIN_NS));
            let t = os::ticks();
            let params = Params::new(&dev, bytes).map_err(|e| defect(format!("params: {e:?}")))?;
            let params_ns = clock.since(t);
            r.o.cold("params", params_ns);
            let t = os::ticks();
            if let Err(e) = salted.qualify() {
                r.o.put("qualified", "0 -");
                return Err(unqualified("salted", e));
            }
            let qualify_salted_ns = clock.since(t);
            r.o.cold("qualify_salted", qualify_salted_ns);
            let t = os::ticks();
            let (first_salted, done) = first_b1(&dev, &params, &salted, &view, &mut jobs)?;
            drop(done);
            let first_salted_ns = clock.since(t);
            r.o.cold("first_b1_salted", first_salted_ns);
            r.o.cold(
                "start_cold",
                open + lib_salted_ns + salted_ns + params_ns + qualify_salted_ns + first_salted_ns,
            );
            let t = os::ticks();
            let lib = Library::compile(&dev).map_err(|e| defect(format!("the library: {e:?}")))?;
            let lib_ns = clock.since(t);
            let t = os::ticks();
            let pipeline =
                Pipeline::new(&lib, shape).map_err(|e| defect(format!("the pipeline: {e:?}")))?;
            let pipeline_ns = clock.since(t);
            r.o.cold("library", lib_ns);
            r.o.cold("pipeline", pipeline_ns);
            r.point(3, Some(dev.current_allocated_size()), dev.live_bytes());
            let t = os::ticks();
            if let Err(e) = pipeline.qualify() {
                r.o.put("qualified", "1 0");
                return Err(unqualified("canonical", e));
            }
            let qualify_ns = clock.since(t);
            r.o.cold("qualify", qualify_ns);
            r.o.put("qualified", "1 1");
            r.point(4, Some(dev.current_allocated_size()), dev.live_bytes());
            let t = os::ticks();
            let (first, done) = first_b1(&dev, &params, &pipeline, &view, &mut jobs)?;
            let first_ns = clock.since(t);
            r.o.cold("first_b1", first_ns);
            r.o.cold("start_warm", lib_ns + pipeline_ns + qualify_ns + first_ns);
            r.o.put("pipeline_identity", pipeline.identity().hex());
            r.o.put("threadgroup_width", pipeline.threadgroup_width());
            drop(salted);
            drop(lib_salted);
            let refs = super::reference(r, &view)?;
            super::check_first("the first salted B1 job", &first_salted, &refs[0])?;
            super::check_first("the first B1 job", &first, &refs[0])?;
            let t = os::ticks();
            let big = super::pack(&view, &r.texts[2], None)?;
            let mut prepared = done.recycle();
            prepared
                .load(&big)
                .map_err(|e| defect(format!("load: {e:?}")))?;
            r.o.put("job_bytes", big.bytes().total());
            let mut parts = Some(big.into_parts());
            r.o.cold("presize", clock.since(t));
            r.point(5, Some(dev.current_allocated_size()), dev.live_bytes());
            let mut held = Some(Held::Prepared(prepared));
            let timebase = r.clock;
            for s in 0..SERIES.len() {
                let want = &refs[ref_of(s)];
                let mut rep = 0;
                r.series(s, |texts| {
                    let t0 = os::ticks();
                    let mut job = match held.take() {
                        Some(Held::Prepared(j)) => j,
                        Some(Held::Done(d)) => d.recycle(),
                        None => return Err(defect("the job was lost between repetitions")),
                    };
                    let batch = super::pack(&view, texts, parts.take())?;
                    let packed = os::ticks();
                    job.load(&batch)
                        .map_err(|e| defect(format!("load: {e:?}")))?;
                    let loaded = os::ticks();
                    let flight = job.submit(&pipeline).map_err(|(_, r)| refused(r))?;
                    jobs.0 += 1;
                    let done = flight.wait().map_err(failed)?;
                    jobs.1 += 1;
                    let got = done.out();
                    let read = os::ticks();
                    let stride = batch.out_stride() as usize;
                    let c = super::compare(timebase, s, rep, stride, got, want)?;
                    let t = done
                        .times()
                        .ok_or_else(|| defect("a completed job has no stamps"))?;
                    if let Some(tb) = t.timebase
                        && (tb.numer, tb.denom) != (timebase.numer, timebase.denom)
                    {
                        return Err(defect(format!(
                            "the job's timebase {}/{} is not the run's",
                            tb.numer, tb.denom
                        )));
                    }
                    let ns = |ticks: u64| Some(timebase.ns(ticks));
                    let chain = [
                        ns(t0),
                        ns(loaded),
                        ns(t.before_encode),
                        ns(t.after_commit),
                        t.scheduled.map(|x| timebase.ns(x)),
                        secs_ns(t.gpu_start),
                        secs_ns(t.gpu_end),
                        ns(t.handler),
                        ns(t.woke),
                        ns(read),
                    ];
                    parts = Some(batch.into_parts());
                    held = Some(Held::Done(done));
                    rep += 1;
                    Ok(Rep {
                        t0: timebase.ns(t0),
                        packed: Some(timebase.ns(packed)),
                        read: timebase.ns(read),
                        chain: Some(chain),
                        compare: c,
                    })
                })?;
            }
            r.point(6, Some(dev.current_allocated_size()), dev.live_bytes());
            r.o.put("jobs", format!("{} {}", jobs.0, jobs.1));
            drop(held);
            drop(parts);
            drop(pipeline);
            drop(lib);
            drop(params);
            let declared = dev.live_bytes();
            drop(dev);
            Ok(declared)
        })?;
        r.point(7, current_allocated_size_fresh(), declared);
        Ok(())
    }
}

/// The G arm off macOS: a defect, since no Metal device exists.
#[cfg(not(target_os = "macos"))]
mod gpu {
    use super::{R, Run, defect};

    pub(super) fn arm_g(_r: &mut Run<'_>) -> R<()> {
        Err(defect("the G arm runs on macOS only"))
    }
}

/// Every key one arm has no figure for.
const fn not_in(arm: Arm) -> &'static [&'static str] {
    const G_ONLY: &[&str] = &[
        "cold.device_open_ns",
        "cold.library_salted_ns",
        "cold.pipeline_salted_ns",
        "cold.params_ns",
        "cold.qualify_salted_ns",
        "cold.first_b1_salted_ns",
        "cold.start_cold_ns",
        "cold.library_ns",
        "cold.pipeline_ns",
        "cold.qualify_ns",
        "cold.start_warm_ns",
        "cold.cache_hit",
        "device_name",
        "device_registry_id",
        "library_salt",
        "run_salt",
        "pipeline_identity",
        "threadgroup_width",
        "limits",
        "params_bytes",
        "job_bytes",
        "qualified",
        "jobs",
        "mem.P2",
        "mem.P3",
        "mem.P4",
    ];
    match arm {
        Arm::G => &[
            "cold.pool_ns",
            "pool_qos_set",
            "pool_qos_wanted",
            "f_argmax",
            "f_argmax_pin",
        ],
        Arm::F | Arm::I | Arm::T => G_ONLY,
    }
}

fn run(r: &mut Run<'_>) -> R<()> {
    let a = r.a;
    let keys = not_in(a.arm);
    r.o.absent(keys);
    match a.arm {
        Arm::F => {
            r.o.absent(&[
                "cold.pool_ns",
                "pool_qos_set",
                "pool_qos_wanted",
                "tone_int_id",
                "tone_int_id_pin",
                "b371_out_digest",
                "b371_out_digest_pin",
            ]);
            r.o.put(
                "f_argmax_pin",
                tone_int::F_ARGMAX_371
                    .map(|c| char::from(b'0' + c))
                    .iter()
                    .collect::<String>(),
            );
            arm_f(r)
        }
        Arm::I | Arm::T => {
            r.o.absent(&["f_argmax", "f_argmax_pin"]);
            pins(&mut r.o);
            arm_cpu(r, a.arm == Arm::T)
        }
        Arm::G => {
            pins(&mut r.o);
            gpu::arm_g(r)
        }
    }
}

fn pins(o: &mut Out) {
    o.put("tone_int_id_pin", tone_int::V1_ID);
    o.put("b371_out_digest_pin", tone_int::B371_OUT_DIGEST);
}

/// The run before its arm: the summary's identity, the workload built, and P0 as `entry` read it.
fn start(a: &Args, clock: Clock, entry: Entry) -> Run<'_> {
    let mut o = Out::new();
    o.put("entry_ticks", entry.ticks);
    o.put("format", FORMAT_NAME);
    o.put("arm", a.arm.name());
    o.put("run_id", &a.run_id);
    o.put("build_commit", env!("ATERM_BENCH_GIT_COMMIT"));
    o.put("build_profile", env!("ATERM_BENCH_PROFILE"));
    o.put("build_toolchain", env!("ATERM_BENCH_TOOLCHAIN"));
    o.put("debug_assertions", cfg!(debug_assertions));
    o.put("hw_model", opt(os::sysctl_str("hw.model")));
    o.put("kern_osversion", opt(os::sysctl_str("kern.osversion")));
    o.put("timebase", format!("{}/{}", clock.numer, clock.denom));
    o.put("parallelism", aterm_parallelism());
    o.put(
        "qos",
        os::qos().map_or_else(|| "-".to_owned(), |(c, r)| format!("{c:#x} {r}")),
    );
    o.put("reps", a.reps.map(|n| n.to_string()).join(" "));
    o.put("iso_ms", a.iso_ms);
    let texts371 = tone_int::texts();
    let b16384 = (0..SERIES[3].1)
        .map(|i| texts371[i % texts371.len().max(1)])
        .collect();
    let mut r = Run {
        a,
        clock,
        o,
        declared_max: 0,
        texts: [vec![tone_int::B1_TEXT], texts371, b16384],
        measured: Vec::with_capacity(SERIES.len()),
    };
    r.put_point(0, entry.p0);
    r
}

fn main() -> ExitCode {
    let entry = Entry::now();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a = match parse_args(&args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("aos_gpu: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let Some((numer, denom)) = os::timebase() else {
        eprintln!("aos_gpu: mach_timebase_info failed");
        return ExitCode::from(1);
    };
    let mut r = start(&a, Clock { numer, denom }, entry);
    r.o.put("thermal_start", opt(os::thermal()));
    if r.texts[1].first() != Some(&tone_int::B1_TEXT) {
        eprintln!(
            "aos_gpu: B1's text is not the workload's first line, so B371's row 0 is not B1's reference"
        );
        return ExitCode::from(1);
    }
    let code = match run(&mut r) {
        Ok(()) => {
            r.o.put("gpu_failure", "none");
            0
        }
        Err(Stop::Gpu(why)) => {
            eprintln!("aos_gpu: {why}");
            r.o.put("gpu_failure", &why);
            EXIT_GPU
        }
        Err(Stop::Defect(why)) => {
            eprintln!("aos_gpu: {why}");
            1
        }
    };
    r.o.put("declared_max", r.declared_max);
    r.o.put("workload_digest", tone_int::workload_digest().hex());
    r.o.put("workload_digest_pin", tone_int::WORKLOAD_DIGEST);
    r.o.put("lifetime_max_footprint", opt(os::footprint().map(|f| f.1)));
    r.o.put("thermal_end", opt(os::thermal()));
    let raw = format!("{}.raw", a.out);
    if let Err(e) = std::fs::write(&raw, raw_text(&a, &r.measured)) {
        eprintln!("aos_gpu: cannot write {raw}: {e}");
        return ExitCode::from(1);
    }
    r.o.put("last_ticks", os::ticks());
    if let Err(e) = std::fs::write(&a.out, r.o.text()) {
        eprintln!("aos_gpu: cannot write {}: {e}", a.out);
        return ExitCode::from(1);
    }
    ExitCode::from(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn the_flags_parse_and_default_to_the_registered_repetitions() {
        let a = parse_args(&words("--arm G --run-id s1.r01.4 --out /x/y")).expect("args");
        assert_eq!(a.arm, Arm::G);
        assert_eq!(a.reps, [1000, 100, 500, 200]);
        assert_eq!(a.iso_ms, 100);
        let a = parse_args(&words(
            "--arm T --run-id x --out y --reps 3,2,1,1 --iso-ms 0",
        ))
        .expect("overrides");
        assert_eq!((a.reps, a.iso_ms), ([3, 2, 1, 1], 0));
        for bad in [
            "--arm X --run-id x --out y",
            "--arm F --out y",
            "--arm F --run-id a/b --out y",
            "--arm F --run-id x --out y --reps 1,2,3",
            "--arm F --run-id x --out y --reps 1,0,1,1",
            "--arm F --run-id x --out",
        ] {
            assert!(parse_args(&words(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn nearest_rank_takes_the_ceiling_rank() {
        let v: Vec<u64> = (1..=100).collect();
        assert_eq!(nearest_rank(&v, 50), Some(50));
        assert_eq!(nearest_rank(&v, 95), Some(95));
        assert_eq!(nearest_rank(&v, 99), Some(99));
        assert_eq!(nearest_rank(&[7], 99), Some(7));
        assert_eq!(nearest_rank(&[1, 2, 3], 50), Some(2));
        assert_eq!(nearest_rank(&[], 50), None);
        assert_eq!(three(vec![3, 1, 2]), "2 3 3");
    }

    #[test]
    fn every_format_key_expands_once_and_every_placeholder_is_filled() {
        let keys = format_keys();
        let mut sorted = keys.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "a key repeats");
        assert!(keys.iter().all(|k| !k.contains('<')), "{keys:?}");
        assert_eq!(keys.first().map(String::as_str), Some("entry_ticks"));
        assert_eq!(keys.last().map(String::as_str), Some("last_ticks"));
        assert!(keys.contains(&"b16384.part.readback_ns".to_owned()));
        assert!(keys.contains(&"mem.P7".to_owned()));
        let b1 = keys.iter().position(|k| k == "b1.gpu_busy_ns");
        let b1iso = keys.iter().position(|k| k == "b1iso.reps");
        assert!(b1 < b1iso, "a series' keys stay together");
    }

    #[test]
    fn a_split_is_valid_only_when_its_stamps_are_monotone() {
        let ok = [0, 5, 6, 7, 8, 9, 10, 11, 12, 13].map(Some);
        assert!((0..PARTS.len()).all(|k| part_valid(&ok, k)));
        let mut late_encode = ok;
        late_encode[2] = Some(4);
        assert!(!part_valid(&late_encode, 1));
        assert!(
            (0..PARTS.len())
                .filter(|&k| k != 1)
                .all(|k| part_valid(&late_encode, k))
        );
        let mut unscheduled = ok;
        unscheduled[4] = None;
        assert!(!part_valid(&unscheduled, 2) && !part_valid(&unscheduled, 3));
        let mut early_gpu = ok;
        early_gpu[5] = Some(7);
        assert!(!part_valid(&early_gpu, 3) && part_valid(&early_gpu, 4));
    }

    #[test]
    fn a_split_series_counts_invalid_parts_and_sums_the_gpu() {
        let rep = |chain: [u64; 10]| Rep {
            t0: chain[0],
            packed: Some(chain[0] + 1),
            read: chain[9],
            chain: Some(chain.map(Some)),
            compare: 1,
        };
        let m = Measured {
            reps: vec![
                rep([0, 5, 6, 7, 8, 9, 10, 11, 12, 13]),
                rep([0, 5, 6, 7, 8, 7, 12, 13, 14, 15]),
            ],
            cpu: Some((10, 20)),
        };
        let mut o = Out::new();
        put_series(&mut o, 3, &m);
        assert_eq!(o.values["b16384.split_valid"], "1");
        assert_eq!(o.values["b16384.part_invalid"], "0 0 0 1 0 0 0 0");
        assert_eq!(o.values["b16384.gpu_busy_ns"], "6");
        assert_eq!(o.values["b16384.part.gpu_ns"], "1 1 1");
        assert_eq!(o.values["b16384.e2e_ns"], "13 15 15");
        assert_eq!(o.values["b16384.packed_ns"], "12 14 14");
        assert_eq!(o.values["b16384.cpu_ns"], "10 20");
    }

    #[test]
    fn references_repeat_the_371_rows_cyclically() {
        let per: Vec<u32> = (0..371 * 2).collect();
        let [b1, b371, big] = references(&per, 2);
        assert_eq!(b1, vec![0, 1]);
        assert_eq!(b371, per);
        assert_eq!(big.len(), 16_384 * 2);
        assert_eq!(&big[371 * 2..371 * 2 + 2], &[0, 1]);
        assert_eq!(
            &big[(16_383) * 2..],
            &per[(16_383 % 371) * 2..(16_383 % 371) * 2 + 2]
        );
    }

    #[test]
    fn t_chunks_give_each_thread_at_least_four() {
        for (rows, threads) in [(16_384u32, 10usize), (371, 10), (371, 4), (1, 10), (40, 10)] {
            let c = chunk(rows, threads);
            assert!(c >= 1);
            if rows as usize >= 4 * threads {
                assert!(
                    rows.div_ceil(c) as usize >= 4 * threads,
                    "{rows} rows {threads}"
                );
            }
        }
    }

    #[test]
    fn salts_are_per_run_and_the_run_salt_is_above_its_floor() {
        let min = 1 << 21;
        let (a, x) = salts("s1.r01.4", min);
        let (b, y) = salts("s1.r02.1", min);
        assert_ne!(a, b);
        assert_ne!(x, y);
        assert_eq!(salts("s1.r01.4", min), (a, x));
        for id in ["a", "b", "c", "d", "e"] {
            assert!(salts(id, min).1 >= min);
        }
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let r = std::panic::catch_unwind(|| Out::new().put("no.such.key", 1));
        assert!(r.is_err());
    }

    #[test]
    fn a_row_a_later_repetition_leaves_unwritten_fails_c0() {
        let view = int_bag_mlp::check(tone_int::v1().expect("tone-int v1")).expect("its check");
        let texts = tone_int::texts();
        let batch = pack(&view, &texts, None).expect("B371");
        let (rows, stride) = (batch.rows(), batch.out_stride() as usize);
        let mut want = vec![0u32; batch.out_words()];
        int_bag_mlp::forward(&view, &batch, &mut want).expect("forward");
        let mut warm = Warm {
            view,
            clock: Clock { numer: 1, denom: 1 },
            out: vec![0; want.len()],
            parts: Some(batch.into_parts()),
        };
        let fail = |e: int_bag_mlp::ForwardError| defect(format!("{e:?}"));
        let whole =
            |b: &Batch, out: &mut [u32]| int_bag_mlp::forward_fast(&view, b, out).map_err(fail);
        let skip = 7;
        let all_but_one = |b: &Batch, out: &mut [u32]| -> R<()> {
            let (head, rest) = out.split_at_mut(skip as usize * stride);
            int_bag_mlp::forward_fast_rows(&view, b, 0..skip, head).map_err(fail)?;
            int_bag_mlp::forward_fast_rows(&view, b, skip + 1..rows, &mut rest[stride..])
                .map_err(fail)
        };
        for rep in 0..2 {
            warm.rep(2, rep, &texts, &want, whole)
                .expect("a whole forward passes C0");
        }
        match warm.rep(2, 2, &texts, &want, all_but_one) {
            Err(Stop::Defect(why)) => assert!(
                why.starts_with(&format!(
                    "C0 mismatch: series b371 repetition 2 row {skip} word 0: got Some({OUT_POISON})"
                )),
                "{why}"
            ),
            other => panic!("a row the forward left unwritten passed C0: {other:?}"),
        }
    }

    #[test]
    fn every_isolated_repetition_follows_its_gap_the_first_included() {
        let a = parse_args(&words(
            "--arm I --run-id x --out y --reps 1,3,1,1 --iso-ms 40",
        ))
        .expect("args");
        let mut r = start(&a, Clock { numer: 1, denom: 1 }, Entry::now());
        let mut last = std::time::Instant::now();
        let mut waited = Vec::new();
        r.series(ISO, |_| {
            let now = std::time::Instant::now();
            waited.push(now - last);
            last = now;
            Ok(Rep::default())
        })
        .expect("the series");
        assert_eq!(waited.len(), 3);
        assert!(
            waited.iter().all(|w| *w >= Duration::from_millis(40)),
            "{waited:?}"
        );
    }

    #[test]
    fn p0_is_the_entry_reading_not_one_after_the_workload_is_built() {
        let a = parse_args(&words("--arm I --run-id x --out y")).expect("args");
        let p0 = Reading {
            footprint: Some(1),
            metal: None,
            declared: 0,
            live: 2,
            peak: 3,
        };
        let r = start(&a, Clock { numer: 1, denom: 1 }, Entry { ticks: 7, p0 });
        assert_eq!(r.o.values["entry_ticks"], "7");
        assert_eq!(r.o.values["mem.P0"], "1 - 0 2 3");
    }
}
