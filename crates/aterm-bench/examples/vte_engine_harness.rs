// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! vtebench's default suite and the arena's d1 `cat` corpora replayed into the engine
//! (`Terminal::process`) at one grid, with no PTY, GUI or window: the `vte-engine` lane of
//! `verify perf` (`crates/aterm-verify/src/perf/vte_engine.rs`), which captures the payloads
//! from the pinned vtebench's own scripts and hands their folder to this harness.
//!
//! ```text
//! vte_engine_harness <payload-dir> [--corpus NAME=PATH]... [--rows N] [--cols N]
//!                    [--samples N] [--corpus-samples N] [--only NAME,...] [--secs S]
//!                    [--shape live|bare] [--quiet-drain]
//! ```
//!
//! A payload folder holds `<name>.bench` and, where the benchmark has a setup script,
//! `<name>.setup`: each script's stdout as vtebench's loader read it. Each sample replays what
//! vtebench writes for one: a reset (`ESC c`), the setup, then the benchmark's output repeated to
//! at least 1 MiB (vtebench's `--min-bytes`), timing only the benchmark. One untimed warmup
//! sample comes first (vtebench's `--warmup`). A corpus sample is one whole file into a fresh
//! engine, as the `cat` lane runs one `cat` per fresh terminal. Bytes reach `process` in 16 KiB
//! slices, the width the app's parse thread takes a burst in while no keystroke waits
//! (`ingest_chunk_width`, aterm-gui's spawn.rs).
//!
//! The lane runs at whatever load it finds (it takes no quiet claim), and on a shared desk load
//! moves every clock a sample can be timed on. Its wall-clock time is mostly the time it waited
//! for a core. Its CPU time still follows the performance cores' clock, which falls as more of
//! them are busy, and grows on an efficiency core, where the scheduler can put even a
//! user-interactive thread. So on macOS the harness also reads the kernel's counters across each
//! sample (`proc_pid_rusage`'s `rusage_info_v6`: cycles and instructions retired, in all and on
//! performance cores), and a sample's judged figure is the cycles it would have taken on
//! performance cores: its cycles per instruction there, times all its instructions, from a
//! sample that retired at least half of them there ([`Cost::pcore_cycles`]). Cycles do not
//! follow the clock, and instructions do not follow the core. What load still moves is how
//! often a cycle stalls on memory or on a cache it shares. The thread is user-interactive, which
//! macOS schedules on performance cores first, and the scenarios run interleaved, one sample of
//! each in turn, so a burst of load spreads over all of them.
//!
//! The report gives, per scenario, those performance-core cycles per MiB (minimum and median),
//! the best sample's MiB per 10^9 of them (the lane's gated key), ms per MiB on the thread's CPU
//! clock and on the wall clock (minimum and median), instructions per MiB and the performance
//! cores' clock in GHz, and the 1-minute load before and after. Only the cycles are judged;
//! every time moves with load. Without the counters (another OS) the judged key is absent.
//! `--secs` keeps sampling until that many seconds have passed (a profiler's window); `--only`
//! runs the named scenarios alone. Every vtebench sample begins with RIS, which erases the
//! history, so a live sample starts over an empty store; `--quiet-drain` gives a live engine,
//! after each sample's setup and before it is timed, the drain the app's compression worker runs
//! once output goes quiet ([`worker_quiet_drain`]), so the sample starts over a store that holds
//! what the setup scrolled off, as a session's burst does after earlier output. The last stdout
//! line is the JSON report the lane judges.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_core::config::TerminalConfig;
use aterm_core::scrollback::Scrollback;
use aterm_core::terminal::Terminal;

/// vtebench's `--min-bytes` default: a benchmark's output repeats until it reaches this.
const MIN_BYTES: usize = 1 << 20;
/// vtebench's `--warmup` default, and the corpora's.
const WARMUP: usize = 1;
/// Timed samples per vtebench scenario.
const SAMPLES: usize = 15;
/// Timed samples per corpus (each a whole 150 MB file).
const CORPUS_SAMPLES: usize = 7;
/// The slice the app's parse thread hands `process` while no keystroke waits.
const SLICE: usize = 16 * 1024;
/// What vtebench writes before each sample's setup: RIS.
const RESET: &[u8] = b"\x1bc";
const MIB: f64 = (1u64 << 20) as f64;
/// The share of a sample's instructions that must have retired on performance cores for their
/// cycles per instruction to stand for the whole sample.
const ON_PCORES: f64 = 0.5;

/// Which engine a sample runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// A live session's engine, as aterm-gui's `new_live_terminal` (spawn.rs) builds it: tiered
    /// scrollback with the live ring, the default config applied, the alternate-scroll host
    /// mode on and the alt-screen archive in the process-wide pool; and scrollback compression
    /// handed off the parse thread, as the session's reader turns it on. No compression worker
    /// runs here, so a sample times the parse thread's own work: staging scrolled-off rows and
    /// shedding a flood's overflow, never the worker's LZ4.
    Live,
    /// `Terminal::new`: a 10 000-line ring and nothing else, the other harnesses' engine.
    Bare,
}

impl Shape {
    const fn name(self) -> &'static str {
        match self {
            Shape::Live => "live",
            Shape::Bare => "bare",
        }
    }
}

/// The app's live ring: about 4 MiB of rows, 512 to 10 000 of them (aterm-gui spawn.rs,
/// `live_scrollback_ring_lines`).
fn live_ring_lines(cols: u16) -> usize {
    let per_line = usize::from(cols)
        .saturating_mul(std::mem::size_of::<aterm_core::grid::Cell>())
        .saturating_add(45)
        .max(1);
    ((4 << 20) / per_line).clamp(512, 10_000)
}

fn engine(shape: Shape, rows: u16, cols: u16) -> Terminal {
    match shape {
        Shape::Bare => Terminal::new(rows, cols),
        Shape::Live => {
            let mut t = Terminal::with_scrollback(
                rows,
                cols,
                live_ring_lines(cols),
                Scrollback::with_defaults(),
            );
            t.apply_config(&TerminalConfig::default());
            t.set_host_alternate_scroll(true);
            t.set_alt_archive_origin(1);
            t.set_alt_archive_shared(true);
            t.set_compress_offload_active(true);
            t
        }
    }
}

/// The lines the app's compression worker moves into the store in one hold of the terminal's
/// lock, and the staged lines its quiet drain leaves (aterm-gui's spawn.rs, `COMPRESS_BUDGET`
/// and `COMPRESS_LOW_WATER`).
const WORKER_BUDGET: usize = 256;

/// What the app's compression worker does once output goes quiet (aterm-gui's spawn.rs,
/// `spawn_compress_worker`): move staged lines into the store a budget at a time until at most
/// its low water mark stays staged or a batch moves nothing.
fn worker_quiet_drain(term: &mut Terminal) {
    let mut prev = usize::MAX;
    loop {
        let remaining = term.drain_lazy_bounded(WORKER_BUDGET);
        if remaining <= WORKER_BUDGET || remaining >= prev {
            break;
        }
        prev = remaining;
    }
}

fn feed(term: &mut Terminal, bytes: &[u8]) {
    for slice in bytes.chunks(SLICE) {
        term.process(std::hint::black_box(slice));
    }
}

/// The clocks and counters a sample is read on.
mod clock {
    /// This thread's CPU time in nanoseconds: what it ran, not what it waited for a core.
    #[cfg(unix)]
    pub(super) fn thread_cpu_ns() -> u64 {
        let mut stamp = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `stamp` is initialized and writable for this one call.
        if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut stamp) } != 0 {
            super::fail("the thread CPU clock is unreadable");
        }
        u64::try_from(stamp.tv_sec).unwrap_or(0) * 1_000_000_000
            + u64::try_from(stamp.tv_nsec).unwrap_or(0)
    }

    /// No thread CPU clock here: the wall clock stands in, and the report names it.
    #[cfg(not(unix))]
    pub(super) fn thread_cpu_ns() -> u64 {
        static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        u64::try_from(
            ORIGIN
                .get_or_init(std::time::Instant::now)
                .elapsed()
                .as_nanos(),
        )
        .unwrap_or(u64::MAX)
    }

    /// What the report calls [`thread_cpu_ns`].
    pub(super) const NAME: &str = if cfg!(unix) { "thread_cpu" } else { "wall" };

    /// The process's kernel counters, cumulative: CPU time in nanoseconds, cycles and
    /// instructions retired, each in all and on performance cores alone.
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    pub(super) struct Counters {
        pub cpu_ns: f64,
        pub pcpu_ns: f64,
        pub cycles: u64,
        pub pcycles: u64,
        pub instructions: u64,
        pub pinstructions: u64,
    }

    impl Counters {
        pub(super) fn since(self, then: Self) -> Self {
            Self {
                cpu_ns: self.cpu_ns - then.cpu_ns,
                pcpu_ns: self.pcpu_ns - then.pcpu_ns,
                cycles: self.cycles.saturating_sub(then.cycles),
                pcycles: self.pcycles.saturating_sub(then.pcycles),
                instructions: self.instructions.saturating_sub(then.instructions),
                pinstructions: self.pinstructions.saturating_sub(then.pinstructions),
            }
        }
    }

    /// `struct rusage_info_v6` (`<sys/resource.h>`), which libc does not declare; the fields this
    /// harness reads are named, the others kept as runs of their place.
    #[cfg(target_os = "macos")]
    #[repr(C)]
    struct RusageV6 {
        uuid: [u8; 16],
        user_time: u64,
        system_time: u64,
        /// `ri_pkg_idle_wkups` through `ri_lifetime_max_phys_footprint`.
        _before_instructions: [u64; 27],
        instructions: u64,
        cycles: u64,
        /// `ri_billed_energy` through `ri_flags`.
        _before_ptime: [u64; 5],
        user_ptime: u64,
        system_ptime: u64,
        pinstructions: u64,
        pcycles: u64,
        /// `ri_energy_nj` through `ri_page_cache_hits`, then `ri_reserved`.
        _after_pcycles: [u64; 16],
    }

    /// `RUSAGE_INFO_V6`.
    #[cfg(target_os = "macos")]
    const RUSAGE_INFO_V6: libc::c_int = 6;

    /// Nanoseconds per Mach tick, the unit of `rusage_info`'s times.
    #[cfg(target_os = "macos")]
    fn ns_per_tick() -> Option<f64> {
        static TICK: std::sync::OnceLock<Option<f64>> = std::sync::OnceLock::new();
        *TICK.get_or_init(|| {
            let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
            // SAFETY: `tb` is writable for one `mach_timebase_info`.
            let rc = unsafe { libc::mach_timebase_info(&mut tb) };
            (rc == 0 && tb.denom != 0).then(|| f64::from(tb.numer) / f64::from(tb.denom))
        })
    }

    /// The process's counters, or `None` where the kernel does not count them by core type.
    #[cfg(target_os = "macos")]
    pub(super) fn counters() -> Option<Counters> {
        let tick = ns_per_tick()?;
        let mut info = std::mem::MaybeUninit::<RusageV6>::zeroed();
        let pid = libc::c_int::try_from(std::process::id()).ok()?;
        // SAFETY: `info` is writable for one `rusage_info_v6`, the flavor asked for, whose
        // layout `RusageV6` repeats field for field.
        let rc = unsafe {
            libc::proc_pid_rusage(
                pid,
                RUSAGE_INFO_V6,
                info.as_mut_ptr().cast::<libc::rusage_info_t>(),
            )
        };
        if rc != 0 {
            return None;
        }
        // SAFETY: zeroed at creation, so initialized whatever the call wrote.
        let info = unsafe { info.assume_init() };
        Some(Counters {
            cpu_ns: (info.user_time + info.system_time) as f64 * tick,
            pcpu_ns: (info.user_ptime + info.system_ptime) as f64 * tick,
            cycles: info.cycles,
            pcycles: info.pcycles,
            instructions: info.instructions,
            pinstructions: info.pinstructions,
        })
        .filter(|c| c.instructions > 0 && c.cycles > 0)
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn counters() -> Option<Counters> {
        None
    }

    /// Declare this thread user-interactive, the class macOS runs on performance cores first:
    /// whether it took.
    #[cfg(target_os = "macos")]
    pub(super) fn declare_interactive() -> bool {
        // SAFETY: sets the calling thread's own QoS class; takes no pointers.
        unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
                == 0
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) const fn declare_interactive() -> bool {
        false
    }
}

/// One timed sample: its milliseconds on both clocks, and the kernel's counters across it where
/// they are this thread's alone.
#[derive(Clone, Copy)]
struct Cost {
    cpu_ms: f64,
    wall_ms: f64,
    counted: Option<clock::Counters>,
}

/// How far the process's CPU time across a sample may exceed this thread's, as a ratio and in
/// nanoseconds, before its counters are taken to hold another thread's work too: the readings
/// themselves cost a few microseconds.
const OTHER_THREADS: (f64, f64) = (1.02, 50_000.0);

impl Cost {
    /// The cycles this sample would have taken had it all run on performance cores: its cycles
    /// per instruction there, times all its instructions. `None` without counters, or when
    /// fewer than [`ON_PCORES`] of its instructions retired on performance cores, too few to
    /// stand for the rest.
    fn pcore_cycles(&self) -> Option<f64> {
        let c = self.counted?;
        let share = c.pinstructions as f64 / c.instructions as f64;
        (c.instructions > 0 && share >= ON_PCORES)
            .then_some(c.pcycles as f64 / c.pinstructions as f64 * c.instructions as f64)
    }

    /// The performance cores' clock across the sample, in GHz: their cycles per nanosecond.
    fn pcore_ghz(&self) -> Option<f64> {
        let c = self.counted?;
        (c.pcpu_ns > 0.0 && self.pcore_cycles().is_some()).then_some(c.pcycles as f64 / c.pcpu_ns)
    }
}

struct Stopwatch {
    wall: Instant,
    cpu: u64,
    counted: Option<clock::Counters>,
}

impl Stopwatch {
    fn start() -> Self {
        Self {
            counted: clock::counters(),
            cpu: clock::thread_cpu_ns(),
            wall: Instant::now(),
        }
    }

    fn stop(&self) -> Cost {
        let wall_ms = self.wall.elapsed().as_secs_f64() * 1e3;
        let cpu_ns = clock::thread_cpu_ns().saturating_sub(self.cpu) as f64;
        let counted = self
            .counted
            .zip(clock::counters())
            .map(|(then, now)| now.since(then))
            .filter(|c| c.cpu_ns <= cpu_ns * OTHER_THREADS.0 + OTHER_THREADS.1);
        Cost {
            cpu_ms: cpu_ns / 1e6,
            wall_ms,
            counted,
        }
    }
}

/// One scenario's payload.
enum Payload {
    /// A vtebench benchmark: its setup and its output repeated to [`MIN_BYTES`].
    Vte { setup: Vec<u8>, bench: Vec<u8> },
    /// A d1 corpus file, read into memory before the first sample.
    Corpus(Vec<u8>),
}

struct Scenario {
    name: String,
    payload: Payload,
    /// The engine its vtebench samples share, made at its first (warmup) sample.
    live: Option<Terminal>,
    /// How many timed samples it takes.
    n: usize,
    costs: Vec<(Cost, usize)>,
}

/// vtebench's loader: `count = (min_bytes - 1) / len + 1` copies.
fn repeat_to_min(once: &[u8]) -> Vec<u8> {
    once.repeat((MIN_BYTES - 1) / once.len() + 1)
}

/// The scenarios in `dir`, in vtebench's order (by name).
fn vte_scenarios(dir: &Path, n: usize) -> Result<Vec<Scenario>, String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".bench"))
                .map(str::to_string)
        })
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let read = |ext: &str| std::fs::read(dir.join(format!("{name}.{ext}")));
            let once = read("bench").map_err(|e| format!("{name}.bench: {e}"))?;
            if once.is_empty() {
                return Err(format!(
                    "{name}.bench is empty (vtebench drops an empty benchmark)"
                ));
            }
            let setup = match read("setup") {
                Ok(setup) => setup,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(e) => return Err(format!("{name}.setup: {e}")),
            };
            Ok(Scenario {
                name,
                payload: Payload::Vte {
                    setup,
                    bench: repeat_to_min(&once),
                },
                live: None,
                n,
                costs: Vec::new(),
            })
        })
        .collect()
}

/// One sample of `scenario` and the bytes it timed. A vtebench sample runs on the scenario's
/// one engine, as vtebench's samples share their terminal, a live one drained after its setup
/// where `quiet_drain` says ([`worker_quiet_drain`]); a corpus sample runs on a fresh one.
fn sample(
    scenario: &mut Scenario,
    shape: Shape,
    (rows, cols): (u16, u16),
    quiet_drain: bool,
) -> (Cost, usize) {
    match &scenario.payload {
        Payload::Vte { setup, bench } => {
            let term = scenario
                .live
                .get_or_insert_with(|| engine(shape, rows, cols));
            term.process(RESET);
            feed(term, setup);
            if quiet_drain && shape == Shape::Live {
                worker_quiet_drain(term);
            }
            let watch = Stopwatch::start();
            feed(term, bench);
            let cost = watch.stop();
            std::hint::black_box(term.cursor().col);
            (cost, bench.len())
        }
        Payload::Corpus(corpus) => {
            let mut term = engine(shape, rows, cols);
            let watch = Stopwatch::start();
            feed(&mut term, corpus);
            let cost = watch.stop();
            std::hint::black_box(term.cursor().col);
            (cost, corpus.len())
        }
    }
}

/// Whether a scenario of `n` samples takes one in round `round` of `rounds`: its samples spread
/// evenly over the rounds, so a corpus's few interleave with the vtebench scenarios' many.
const fn due(round: usize, n: usize, rounds: usize) -> bool {
    (round + 1) * n / rounds > round * n / rounds
}

/// The 1-minute load average, `-1` where it cannot be read.
fn load1() -> f64 {
    aterm_sysprobe::load_average().map_or(-1.0, |load| load[0])
}

struct Args {
    dir: PathBuf,
    corpora: Vec<(String, PathBuf)>,
    rows: u16,
    cols: u16,
    samples: usize,
    corpus_samples: usize,
    only: Option<Vec<String>>,
    secs: Option<Duration>,
    shape: Shape,
    quiet_drain: bool,
}

fn usage() -> ! {
    eprintln!(
        "usage: vte_engine_harness <payload-dir> [--corpus NAME=PATH]... [--rows N] [--cols N] \
         [--samples N] [--corpus-samples N] [--only NAME,...] [--secs S] [--shape live|bare] \
         [--quiet-drain]"
    );
    std::process::exit(2)
}

fn parse_args() -> Args {
    let mut it = std::env::args().skip(1);
    let Some(dir) = it.next().filter(|d| !d.starts_with("--")) else {
        usage()
    };
    let mut a = Args {
        dir: PathBuf::from(dir),
        corpora: Vec::new(),
        rows: 40,
        cols: 120,
        samples: SAMPLES,
        corpus_samples: CORPUS_SAMPLES,
        only: None,
        secs: None,
        shape: Shape::Live,
        quiet_drain: false,
    };
    while let Some(flag) = it.next() {
        if flag == "--quiet-drain" {
            a.quiet_drain = true;
            continue;
        }
        let value = it.next().unwrap_or_else(|| usage());
        let positive = || {
            value
                .parse::<usize>()
                .ok()
                .filter(|n| *n > 0)
                .unwrap_or_else(|| usage())
        };
        match flag.as_str() {
            "--corpus" => {
                let (name, path) = value.split_once('=').unwrap_or_else(|| usage());
                a.corpora.push((name.to_string(), PathBuf::from(path)));
            }
            "--rows" => a.rows = u16::try_from(positive()).unwrap_or_else(|_| usage()),
            "--cols" => a.cols = u16::try_from(positive()).unwrap_or_else(|_| usage()),
            "--samples" => a.samples = positive(),
            "--corpus-samples" => a.corpus_samples = positive(),
            "--only" => a.only = Some(value.split(',').map(str::to_string).collect()),
            "--secs" => a.secs = Some(Duration::from_secs(positive() as u64)),
            "--shape" => {
                a.shape = match value.as_str() {
                    "live" => Shape::Live,
                    "bare" => Shape::Bare,
                    _ => usage(),
                };
            }
            _ => usage(),
        }
    }
    a
}

fn fail(why: impl std::fmt::Display) -> ! {
    eprintln!("vte_engine_harness: {why}");
    std::process::exit(1)
}

/// The minimum and median of `values` (sorted in place), `None` when there are none.
fn min_median(values: &mut [f64]) -> Option<(f64, f64)> {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    let median = match n {
        0 => return None,
        _ if n % 2 == 1 => values[n / 2],
        _ => (values[n / 2 - 1] + values[n / 2]) / 2.0,
    };
    Some((values[0], median))
}

/// One scenario's report: its line on stderr and its keys in `json`. Its judged key,
/// `<name>_best_mib_per_gcycle`, is the best sample's MiB per 10^9 performance-core cycles
/// ([`Cost::pcore_cycles`]); without counters, or with no sample mostly on performance cores,
/// the key is absent and the report says why.
fn report(scenario: &Scenario, json: &mut String) {
    let costs = &scenario.costs;
    let per_mib = |x: f64, len: usize| x * MIB / len as f64;
    let mut cpu: Vec<f64> = costs.iter().map(|(c, l)| per_mib(c.cpu_ms, *l)).collect();
    let mut wall: Vec<f64> = costs.iter().map(|(c, l)| per_mib(c.wall_ms, *l)).collect();
    let mut pcyc: Vec<f64> = costs
        .iter()
        .filter_map(|(c, l)| c.pcore_cycles().map(|cy| per_mib(cy / 1e6, *l)))
        .collect();
    let mut insn: Vec<f64> = costs
        .iter()
        .filter_map(|(c, l)| c.counted.map(|k| per_mib(k.instructions as f64 / 1e6, *l)))
        .collect();
    let mut ghz: Vec<f64> = costs.iter().filter_map(|(c, _)| c.pcore_ghz()).collect();
    let (Some((cpu_min, cpu_med)), Some((wall_min, wall_med))) =
        (min_median(&mut cpu), min_median(&mut wall))
    else {
        return;
    };
    let name = &scenario.name;
    let bytes = costs.last().map_or(0, |(_, l)| *l);
    let _ = write!(
        json,
        "\"{name}_min_ms_per_mib\":{cpu_min:.4},\"{name}_median_ms_per_mib\":{cpu_med:.4},\
         \"{name}_wall_min_ms_per_mib\":{wall_min:.4},\
         \"{name}_wall_median_ms_per_mib\":{wall_med:.4},\"{name}_bytes\":{bytes},\
         \"{name}_n\":{},\"{name}_pcore_n\":{},",
        costs.len(),
        pcyc.len(),
    );
    let cycles = match min_median(&mut pcyc) {
        Some((min, med)) => {
            let best = 1e3 / min;
            let _ = write!(
                json,
                "\"{name}_pcycles_min_m_per_mib\":{min:.3},\
                 \"{name}_pcycles_median_m_per_mib\":{med:.3},\
                 \"{name}_best_mib_per_gcycle\":{best:.4},",
            );
            format!(
                "P-core Mcycles/MiB min {min:>7.2} med {med:>7.2} ({} of {} samples)",
                pcyc.len(),
                costs.len()
            )
        }
        None if insn.is_empty() => "no cycle counters this thread's alone".to_string(),
        None => format!(
            "no sample of {} ran mostly on performance cores",
            costs.len()
        ),
    };
    if let Some((_, med)) = min_median(&mut insn) {
        let _ = write!(json, "\"{name}_minsn_per_mib\":{med:.3},");
    }
    if let Some((_, med)) = min_median(&mut ghz) {
        let _ = write!(json, "\"{name}_pcore_ghz\":{med:.3},");
    }
    eprintln!(
        "vte_engine_harness: {name:<30} {cycles}; ms/MiB cpu min {cpu_min:>7.2} med \
         {cpu_med:>7.2}, wall min {wall_min:>7.2} med {wall_med:>7.2}; {:.2} MiB a sample",
        bytes as f64 / MIB,
    );
}

fn main() {
    let args = parse_args();
    let interactive = clock::declare_interactive();
    let mut scenarios = vte_scenarios(&args.dir, args.samples).unwrap_or_else(|e| fail(e));
    for (name, path) in &args.corpora {
        if args.only.as_ref().is_some_and(|only| !only.contains(name)) {
            continue;
        }
        let bytes =
            std::fs::read(path).unwrap_or_else(|e| fail(format!("{}: {e}", path.display())));
        scenarios.push(Scenario {
            name: name.clone(),
            payload: Payload::Corpus(bytes),
            live: None,
            n: args.corpus_samples,
            costs: Vec::new(),
        });
    }
    if let Some(only) = &args.only {
        let named = |n: &String| {
            scenarios.iter().any(|s| &s.name == n) || args.corpora.iter().any(|(c, _)| c == n)
        };
        if let Some(missing) = only.iter().find(|n| !named(n)) {
            eprintln!("vte_engine_harness: no scenario `{missing}`");
            std::process::exit(2);
        }
        scenarios.retain(|s| only.contains(&s.name));
    }
    let counted = clock::counters().is_some();
    let load_before = load1();
    eprintln!(
        "vte_engine_harness: {} scenarios at {}x{} (cols x rows), {} engine, {} KiB slices, \
         interleaved, on the {} clock{}{}; load {load_before:.1}",
        scenarios.len(),
        args.cols,
        args.rows,
        args.shape.name(),
        SLICE >> 10,
        clock::NAME,
        if counted {
            " and the kernel's per-core-type cycle counters"
        } else {
            ", with no cycle counters"
        },
        if interactive {
            ", user-interactive"
        } else {
            ""
        },
    );
    let grid = (args.rows, args.cols);
    for scenario in &mut scenarios {
        scenario.costs.reserve(scenario.n);
        for _ in 0..WARMUP {
            let _ = sample(scenario, args.shape, grid, args.quiet_drain);
        }
    }
    let rounds = scenarios.iter().map(|s| s.n).max().unwrap_or(0);
    let started = Instant::now();
    let mut round = 0;
    while round < rounds || args.secs.is_some_and(|s| started.elapsed() < s) {
        for scenario in &mut scenarios {
            if round >= rounds || due(round, scenario.n, rounds) {
                let cost = sample(scenario, args.shape, grid, args.quiet_drain);
                scenario.costs.push(cost);
            }
        }
        round += 1;
    }
    let mut json = format!(
        "{{\"rows\":{},\"cols\":{},\"slice\":{SLICE},\"min_bytes\":{MIN_BYTES},\"warmup\":{WARMUP},\
         \"samples\":{},\"corpus_samples\":{},\"shape\":\"{}\",\"quiet_drain\":{},\"clock\":\"{}\",\
         \"counters\":{counted},\"on_pcores\":{ON_PCORES},\"user_interactive\":{interactive},\
         \"load1_before\":{load_before:.2},",
        args.rows,
        args.cols,
        args.samples,
        args.corpus_samples,
        args.shape.name(),
        args.quiet_drain,
        clock::NAME,
    );
    for scenario in &scenarios {
        report(scenario, &mut json);
    }
    let load_after = load1();
    eprintln!("vte_engine_harness: load {load_before:.1} -> {load_after:.1}");
    let _ = write!(json, "\"load1_after\":{load_after:.2}}}");
    println!("{json}");
}

#[cfg(test)]
mod tests {
    use super::clock::Counters;
    use super::{Cost, due};

    fn cost(instructions: u64, pinstructions: u64, cycles: u64, pcycles: u64) -> Cost {
        Cost {
            cpu_ms: 1.0,
            wall_ms: 1.0,
            counted: Some(Counters {
                cpu_ns: 1e6,
                pcpu_ns: 5e5,
                cycles,
                pcycles,
                instructions,
                pinstructions,
            }),
        }
    }

    #[test]
    fn a_sample_counts_the_cycles_it_would_take_on_performance_cores() {
        assert_eq!(cost(1000, 1000, 400, 400).pcore_cycles(), Some(400.0));
        // Half of it on an efficiency core, at 1.6x the cycles per instruction there: the
        // performance cores' rate stands for the whole.
        assert_eq!(cost(1000, 500, 520, 200).pcore_cycles(), Some(400.0));
        assert_eq!(
            cost(1000, 499, 520, 200).pcore_cycles(),
            None,
            "too little there"
        );
        assert_eq!(cost(0, 0, 0, 0).pcore_cycles(), None);
        let uncounted = Cost {
            counted: None,
            ..cost(1000, 1000, 400, 400)
        };
        assert_eq!(uncounted.pcore_cycles(), None);
        assert_eq!(cost(1000, 500, 520, 200).pcore_ghz(), Some(200.0 / 5e5));
    }

    #[test]
    fn every_scenario_takes_its_count_spread_over_the_rounds() {
        for rounds in 1..=20 {
            for n in 1..=rounds {
                let taken: Vec<usize> = (0..rounds).filter(|&r| due(r, n, rounds)).collect();
                assert_eq!(taken.len(), n, "{n} of {rounds}");
                if n > 1 {
                    let gaps: Vec<usize> = taken.windows(2).map(|w| w[1] - w[0]).collect();
                    let (lo, hi) = (gaps.iter().min(), gaps.iter().max());
                    assert!(hi.zip(lo).is_some_and(|(h, l)| h - l <= 1), "{taken:?}");
                }
            }
        }
    }
}
