// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `deck-motion`: the stats deck in motion, measured (`docs/DESIGN-stats-deck-gpu-motion-2026-10-26.md`
//! §6). Each named take runs one fresh private headless aterm through [`crate::instance`]'s
//! machinery with the deck drawn headless at 2× and a 400 px head, records `ctl video` of it,
//! reads `metrics` and `metrics percentiles` and the process's CPU around it, split by thread
//! (the main thread, the recording's own `aterm-video*` threads, the app's others), sorts each recorded frame by what it changed (the dial, the rest of the deck, outside it,
//! nothing), and recovers the needle's angle in each frame from its pixels.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::band::Band;
use crate::instance::{self, BOOT, Instance, Launch, World, count, take_flag, take_value};
use crate::paths::{from_caller, paths};
use crate::stop;
use crate::{Fail, Res};

/// The verb's own help (`deck-motion --help`).
pub const HELP: &str = "\
usage: targo --unverified lab deck-motion [--aterm PATH] [--secs N] [--settle N] [--repeat N] [--panel-hz N]
         [--deck-rows Y0,Y1] [--dial-x1 X] [--dial CX,CY,R0,R1] [--tip-r R] [--json-out FILE] [--keep]
         <out_dir> [take ...]
       targo --unverified lab deck-motion --analyze <index.json> [geometry flags]

Measure the stats deck in motion: one fresh private headless aterm per take (110x32 cells at --scale 2, the deck drawn
headless, a 400 px head, the cursor trail and blink off; a scratch HOME and XDG dirs, a private control socket, ended
by its process group, never by name), `ctl video <secs> full budget=2048` of it, `metrics` and `metrics percentiles`
before and after (reset between), and the process's CPU over the recording (libproc), split by thread: the main
thread, the recording's own capture threads (`aterm-video*`, which a take without `ctl video` does not run), the app's
other threads, and the time of threads no sample saw (threads are sampled every 50 ms; one that ran and ended between
two samples, or ran on after its last, books its time there).

  take ...      idle (default: idle load swing nodeck sill)
                  idle    the Folded deck with nothing asked of it
                  load    busy threads on every core in pulses, 3 s on and 3 s off, through the recording
                  swing   the dev seam ATERM_DEV_STRAIN_SCRIPT=0.1:2,0.9:2,0.2:2,1.6:2,0.0:2 (a calm sweep, a hot
                          sweep into the redline, a full-range return; the aterm must be a dev-seams build)
                  nodeck  the same instance without the headless deck: the baseline the deck's cost is read against
                  sill    `[sys] stats_bars = \"sill\"`
  <out_dir>     where the takes go, OUTSIDE the repository; made if missing. Per take <take>-<k>/: the recording's
                frames and index.json, frames.tsv (per frame: ms, gap, what changed, the needle's angle) and
                summary.txt; deck-motion.txt holds every summary
  --aterm PATH  the aterm to run (default: the lab's pick); quote the build/profile used with each take
                (`targo --unverified build -p aterm --release --features dev-seams`)
  --secs N      the recording's length (default 10)
  --settle N    seconds from the instance's first answer to the recording (default 15: the deck has readings)
  --repeat N    takes of each name (default 1), run in turn: idle-1 load-1 ... idle-2 ...
  --panel-hz N  ATERM_DEV_PANEL_HZ for the headless frame clock (default: unset, the build's own 60), and the grid
                the summary reads the needle's cadence against (the gaps between frames that move it)
  --deck-rows   the deck's rows in the recorded frame (default 216,340: the Folded deck at 2x under a 400 px head)
  --dial-x1     the dial's right edge: a change left of it is the dial's, right of it the rest of the deck's (default 150)
  --dial        the dial's centre and the needle annulus the angle is read in (default 77.5,277.5,10,48)
  --tip-r       the needle tip's radius, for its travel in px (default 45)
  --json-out    every take's summary as one JSON object (the `deck` perf lane reads it)
  --keep        retain every instance's scratch world
  --analyze     read a recorded take's index.json again (with the geometry flags), write its frames.tsv beside it
                and print its frames' summary; runs nothing

exit: 0 every take recorded and read; 1 a take failed; 2 usage; 3 not run (no aterm)
";

/// The swing take's strain script: a calm sweep, a hot sweep into the redline, a full return.
pub const SWING_SCRIPT: &str = "0.1:2,0.9:2,0.2:2,1.6:2,0.0:2";

/// What one take asks of its instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Take {
    Idle,
    Load,
    Swing,
    NoDeck,
    Sill,
}

impl Take {
    /// Every take, in the order a bare run takes them.
    pub const ALL: [Take; 5] = [
        Take::Idle,
        Take::Load,
        Take::Swing,
        Take::NoDeck,
        Take::Sill,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Take::Idle => "idle",
            Take::Load => "load",
            Take::Swing => "swing",
            Take::NoDeck => "nodeck",
            Take::Sill => "sill",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Take> {
        Take::ALL.into_iter().find(|t| t.name() == s)
    }

    /// The seams its instance runs with, beside the world's own environment.
    fn env(self) -> Vec<(String, String)> {
        let mut env = vec![("ATERM_DEV_HEADROOM_PX".to_string(), "400".to_string())];
        if self != Take::NoDeck {
            env.push(("ATERM_DEV_STATS_DECK_HEADLESS".to_string(), "1".to_string()));
        }
        if self == Take::Swing {
            env.push((
                "ATERM_DEV_STRAIN_SCRIPT".to_string(),
                SWING_SCRIPT.to_string(),
            ));
        }
        env
    }

    /// Its `aterm.toml`.
    fn config(self) -> String {
        let mut config = instance::config_with(&[
            "cursor_trail_style = \"off\"".to_string(),
            "cursor_pet = \"off\"".to_string(),
            "cursor_blink = false".to_string(),
        ]);
        if self == Take::Sill {
            config.push_str("[sys]\nstats_bars = \"sill\"\n");
        }
        config
    }
}

/// Where the deck and the dial sit in a recorded frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geom {
    pub deck_y0: usize,
    pub deck_y1: usize,
    pub dial_x1: usize,
    pub cx: f64,
    pub cy: f64,
    pub r0: f64,
    pub r1: f64,
    pub tip_r: f64,
}

impl Geom {
    /// The Folded deck at 2× under a 400 px head, 110×32 cells (map 3's instance). The card
    /// carries the System State rows (60 px) under the deck since map 3, so the deck sits 60 px
    /// higher than map 3 measured it; the dial's centre is the point its needle's lines meet
    /// across a recorded sweep.
    pub const FOLDED_2X: Geom = Geom {
        deck_y0: 216,
        deck_y1: 340,
        dial_x1: 150,
        cx: 77.5,
        cy: 277.5,
        r0: 10.0,
        r1: 48.0,
        tip_r: 45.0,
    };
}

/// What `deck-motion` was asked.
#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    pub out: Option<String>,
    pub takes: Vec<Take>,
    pub secs: u32,
    pub settle: u32,
    pub repeat: u32,
    pub panel_hz: Option<u32>,
    pub geom: Geom,
    pub json_out: Option<String>,
    pub keep: bool,
    pub aterm: Option<String>,
    pub analyze: Option<String>,
}

fn usage(msg: impl AsRef<str>) -> Fail {
    Fail::usage(format!(
        "deck-motion: {} (`deck-motion --help`)",
        msg.as_ref()
    ))
}

/// Numbers split on commas, exactly `n` of them.
fn numbers(flag: &str, v: &str, n: usize) -> Result<Vec<f64>, Fail> {
    let out: Vec<f64> = v
        .split(',')
        .map(|s| s.trim().parse::<f64>().ok().filter(|x| x.is_finite()))
        .collect::<Option<_>>()
        .ok_or_else(|| usage(format!("{flag} takes {n} numbers, not '{v}'")))?;
    if out.len() == n {
        Ok(out)
    } else {
        Err(usage(format!("{flag} takes {n} numbers, not '{v}'")))
    }
}

/// Parse `deck-motion`'s arguments.
/// # Errors
/// A malformed flag, an unknown take, or no `<out_dir>` without `--analyze`.
pub fn parse_args(args: &[String]) -> Result<Args, Fail> {
    let mut a = args.to_vec();
    let aterm = take_value(&mut a, "--aterm")?;
    let num = |v: Option<String>, flag: &str, default: u32| -> Result<u32, Fail> {
        v.map_or(Ok(default), |v| count(flag, &v))
    };
    let secs = num(take_value(&mut a, "--secs")?, "--secs", 10)?;
    let settle = take_value(&mut a, "--settle")?
        .map(|v| {
            v.parse::<u32>()
                .map_err(|_| usage(format!("--settle takes seconds, not '{v}'")))
        })
        .transpose()?
        .unwrap_or(15);
    let repeat = num(take_value(&mut a, "--repeat")?, "--repeat", 1)?;
    let panel_hz = take_value(&mut a, "--panel-hz")?
        .map(|v| count("--panel-hz", &v))
        .transpose()?;
    let mut geom = Geom::FOLDED_2X;
    if let Some(v) = take_value(&mut a, "--deck-rows")? {
        let n = numbers("--deck-rows", &v, 2)?;
        (geom.deck_y0, geom.deck_y1) = (n[0] as usize, n[1] as usize);
    }
    if let Some(v) = take_value(&mut a, "--dial-x1")? {
        geom.dial_x1 = numbers("--dial-x1", &v, 1)?[0] as usize;
    }
    if let Some(v) = take_value(&mut a, "--dial")? {
        let n = numbers("--dial", &v, 4)?;
        (geom.cx, geom.cy, geom.r0, geom.r1) = (n[0], n[1], n[2], n[3]);
    }
    if let Some(v) = take_value(&mut a, "--tip-r")? {
        geom.tip_r = numbers("--tip-r", &v, 1)?[0];
    }
    if geom.deck_y1 <= geom.deck_y0 || geom.r1 <= geom.r0 {
        return Err(usage(
            "the deck's rows and the dial's annulus must not be empty",
        ));
    }
    let json_out = take_value(&mut a, "--json-out")?.map(|p| from_caller(&p));
    let keep = take_flag(&mut a, "--keep");
    let analyze = take_value(&mut a, "--analyze")?.map(|p| from_caller(&p));
    if let Some(flag) = a.iter().find(|x| x.starts_with("--")) {
        return Err(usage(format!("unknown flag '{flag}'")));
    }
    if analyze.is_some() {
        if !a.is_empty() {
            return Err(usage("--analyze reads one take and runs nothing"));
        }
        return Ok(Args {
            out: None,
            takes: Vec::new(),
            secs,
            settle,
            repeat,
            panel_hz,
            geom,
            json_out,
            keep,
            aterm,
            analyze,
        });
    }
    let mut rest = a.into_iter();
    let out = rest
        .next()
        .ok_or_else(|| usage("names no <out_dir>"))
        .map(|p| from_caller(&p))?;
    let mut takes = Vec::new();
    for name in rest {
        let take = Take::parse(&name).ok_or_else(|| usage(format!("no take '{name}'")))?;
        if !takes.contains(&take) {
            takes.push(take);
        }
    }
    if takes.is_empty() {
        takes = Take::ALL.to_vec();
    }
    Ok(Args {
        out: Some(out),
        takes,
        secs,
        settle,
        repeat,
        panel_hz,
        geom,
        json_out,
        keep,
        aterm,
        analyze,
    })
}

/// `deck-motion`.
/// # Errors
/// Usage, no aterm, or an instance that could not start or answer.
pub fn cmd_deck_motion(args: &[String]) -> Res {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }
    stop::install();
    let args = parse_args(args)?;
    if let Some(index) = &args.analyze {
        let index = Path::new(index);
        let frames = read_take(index, &args.geom).map_err(Fail::die)?;
        let tsv = index.with_file_name("frames.tsv");
        crate::util::write_fresh(&tsv, frames.tsv())
            .map_err(|e| Fail::die(format!("cannot write {}: {e}", tsv.display())))?;
        print!(
            "{}",
            frames.summary(&args.geom, args.panel_hz.unwrap_or(HEADLESS_HZ))
        );
        return Ok(0);
    }
    let aterm = instance::resolve_aterm(args.aterm.clone()).map_err(|e| Fail::exit(3, e))?;
    let out = crate::trail_gallery::resolve_out(
        args.out.as_deref().unwrap_or_default(),
        &crate::trail_gallery::checkouts(&paths().repo),
    )
    .map_err(|e| usage(format!("<out_dir> {e}")))?;
    std::fs::create_dir_all(&out)
        .map_err(|e| Fail::die(format!("deck-motion: cannot make {}: {e}", out.display())))?;
    let mut book = String::new();
    let mut json = BTreeMap::new();
    let mut failed = false;
    for k in 1..=args.repeat {
        for &take in &args.takes {
            let label = format!("{}-{k}", take.name());
            eprintln!("deck-motion: {label} ...");
            match run_take(&args, &aterm, take, &out.join(&label)) {
                Ok(report) => {
                    let _ = writeln!(book, "== {label}\n{}", report.text);
                    eprint!("{}", report.text);
                    json.insert(label, report.json);
                }
                Err(f) => {
                    let why = f.msg.unwrap_or_default();
                    let _ = writeln!(book, "== {label}\nFAILED: {why}\n");
                    eprintln!("deck-motion: {label} FAILED: {why}");
                    failed = true;
                    if f.stopped.is_some() {
                        break;
                    }
                }
            }
        }
    }
    let _ = crate::util::write_fresh(&out.join("deck-motion.txt"), &book);
    if let Some(path) = &args.json_out {
        let mut obj = aterm_json::Map::new();
        for (label, map) in json {
            obj.insert(label, aterm_json::Value::Object(map));
        }
        let text = aterm_json::to_string(&aterm_json::Value::Object(obj))
            .map_err(|e| Fail::die(format!("deck-motion: --json-out: {e}")))?;
        crate::util::write_fresh(Path::new(path), text + "\n")
            .map_err(|e| Fail::die(format!("deck-motion: cannot write {path}: {e}")))?;
    }
    println!("deck-motion: {}", out.join("deck-motion.txt").display());
    Ok(i32::from(failed))
}

/// One take's summary, as text and as JSON figures.
struct TakeReport {
    text: String,
    json: aterm_json::Map,
}

/// The headless frame clock's rate where `--panel-hz` names none: the build's own
/// (`stats_clock`'s `HEADLESS_HZ`).
const HEADLESS_HZ: u32 = 60;

/// How often a take samples the app's threads ([`CpuSampler`]).
const THREAD_POLL: Duration = Duration::from_millis(50);

/// The prefix of the recording's own threads (`ctl video`'s convert workers), whose CPU a take
/// reports apart from the app's.
const CAPTURE_THREADS: &str = "aterm-video";

/// The CPU a take's process ran, split by thread, ns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CpuSplit {
    /// The process's, every thread it had ([`crate::sys::process_cpu_ns`]).
    process: u64,
    /// The main thread's.
    main: u64,
    /// The recording's own threads' ([`CAPTURE_THREADS`]).
    capture: u64,
    /// Every other thread a sample saw, by name (`""` unnamed).
    others: BTreeMap<String, u64>,
}

impl CpuSplit {
    /// The time no sample booked: threads that ran and ended between two samples, or ran on
    /// after the last sample that saw them.
    fn unseen(&self) -> u64 {
        self.process
            .saturating_sub(self.main + self.capture + self.others.values().sum::<u64>())
    }

    /// The app's own: the main thread and every other thread but the recording's.
    fn app(&self) -> u64 {
        self.main + self.others.values().sum::<u64>()
    }
}

/// A take's samples of one process's threads: each thread's CPU when the take began (0 for one
/// born during it) and at the last sample that saw it, with its name.
#[derive(Debug, Default)]
struct CpuSampler {
    process0: u64,
    process1: u64,
    /// The main thread's id: the least when the take began.
    main: Option<u64>,
    /// By thread id: its name, its CPU at the take's start, at its last sample.
    seen: BTreeMap<u64, (String, u64, u64)>,
}

impl CpuSampler {
    /// The take's start: the process's CPU and every live thread's.
    fn begin(process: u64, threads: &[crate::sys::ThreadCpu]) -> Self {
        let mut s = Self {
            process0: process,
            process1: process,
            main: threads.first().map(|t| t.tid),
            seen: BTreeMap::new(),
        };
        for t in threads {
            s.seen.insert(t.tid, (t.name.clone(), t.ns, t.ns));
        }
        s
    }

    /// A sample: each live thread's CPU (one born since the take began from 0), and the
    /// process's where `process` is known.
    fn sample(&mut self, process: Option<u64>, threads: &[crate::sys::ThreadCpu]) {
        if let Some(p) = process {
            self.process1 = p.max(self.process1);
        }
        for t in threads {
            let e = self
                .seen
                .entry(t.tid)
                .or_insert_with(|| (t.name.clone(), 0, 0));
            if !t.name.is_empty() {
                e.0.clone_from(&t.name);
            }
            e.2 = t.ns.max(e.2);
        }
    }

    /// The take's CPU, split by thread.
    fn split(&self) -> CpuSplit {
        let mut out = CpuSplit {
            process: self.process1.saturating_sub(self.process0),
            ..CpuSplit::default()
        };
        for (&tid, (name, at0, last)) in &self.seen {
            let ran = last.saturating_sub(*at0);
            if Some(tid) == self.main {
                out.main = ran;
            } else if name.starts_with(CAPTURE_THREADS) {
                out.capture += ran;
            } else if ran > 0 {
                *out.others.entry(name.clone()).or_default() += ran;
            }
        }
        out
    }
}

/// `pid`'s CPU now: the process's and each live thread's.
fn cpu_now(pid: i32) -> (Option<u64>, Vec<crate::sys::ThreadCpu>) {
    (
        crate::sys::process_cpu_ns(pid),
        crate::sys::thread_cpu_ns(pid).unwrap_or_default(),
    )
}

/// `split` as one line: the process, then by whom, s.
fn split_line(split: &CpuSplit, secs: f64) -> String {
    let s = |ns: u64| ns as f64 / 1e9;
    let mut named: Vec<(&String, &u64)> = split.others.iter().collect();
    named.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let top: Vec<String> = named
        .iter()
        .take(4)
        .map(|&(name, &ns)| {
            let name = if name.is_empty() { "(unnamed)" } else { name };
            format!("{name} {:.3}", s(ns))
        })
        .collect();
    format!(
        "cpu over the recording ({secs:.2} s): process {:.3} s = app {:.3} s (main thread {:.3} s, \
         its other threads {:.3} s: {}) + the recording's capture threads ({CAPTURE_THREADS}*) {:.3} s \
         + threads no sample saw {:.3} s",
        s(split.process),
        s(split.app()),
        s(split.main),
        s(split.app().saturating_sub(split.main)),
        if top.is_empty() {
            "none".to_owned()
        } else {
            top.join(", ")
        },
        s(split.capture),
        s(split.unseen()),
    )
}

/// `key=value` words of an `OK …` reply.
fn fields(reply: &str) -> BTreeMap<String, String> {
    reply
        .split_whitespace()
        .filter_map(|w| w.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Busy threads on every core while `on` holds, until `done`.
fn burners(on: &Arc<AtomicBool>, done: &Arc<AtomicBool>) -> Vec<std::thread::JoinHandle<()>> {
    let n = aterm_cores::available_parallelism().map_or(8, std::num::NonZero::get);
    (0..n)
        .map(|_| {
            let (on, done) = (Arc::clone(on), Arc::clone(done));
            std::thread::spawn(move || {
                let mut x = 0u64;
                while !done.load(Ordering::Relaxed) {
                    if on.load(Ordering::Relaxed) {
                        for i in 0..100_000u64 {
                            x = std::hint::black_box(
                                x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(i),
                            );
                        }
                    } else {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
            })
        })
        .collect()
}

fn run_take(args: &Args, aterm: &Path, take: Take, dir: &Path) -> Result<TakeReport, Fail> {
    let mut world = World::new(&take.config())?;
    if args.keep {
        world.keep();
    }
    let mut env = world.env();
    env.extend(take.env());
    if let Some(hz) = args.panel_hz {
        env.push(("ATERM_DEV_PANEL_HZ".to_string(), hz.to_string()));
    }
    let mut inst = Instance::start_in(
        &world,
        Launch {
            aterm: aterm.to_path_buf(),
            headless: true,
            args: ["--columns", "110", "--lines", "32", "--scale", "2"]
                .map(String::from)
                .to_vec(),
            env,
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
        Band::App,
    )?;
    if let Err(f) = inst.await_socket(BOOT) {
        return crate::instance::after_stop(Err(f), inst.stop());
    }
    let report = record(args, take, &inst, dir);
    let report = crate::instance::after_stop(report, inst.stop());
    let _ = std::fs::create_dir_all(dir);
    let _ = crate::util::copy_regular_file(&inst.log, dir.join("gui.log"));
    report
}

fn record(args: &Args, take: Take, inst: &Instance, dir: &Path) -> Result<TakeReport, Fail> {
    let ready = instance::terminal_request(|a| inst.ctl(a), &["ready"]);
    if ready.code != 0 {
        return Err(Fail::die(format!("not ready: {}", ready.first())));
    }
    stop::sleep(Duration::from_secs(u64::from(args.settle)))?;
    let pid = inst.aterm_pid();
    let metrics_before = inst.ctl(&["metrics"]).stdout;
    let pct_before = inst.ctl(&["metrics", "percentiles"]).stdout;
    let _ = inst.ctl(&["metrics", "reset"]);
    let (on, done) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let burn = if take == Take::Load {
        let threads = burners(&on, &done);
        let (on, done) = (Arc::clone(&on), Arc::clone(&done));
        let pulse = std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                on.store(true, Ordering::Relaxed);
                std::thread::sleep(Duration::from_secs(3));
                on.store(false, Ordering::Relaxed);
                std::thread::sleep(Duration::from_secs(3));
            }
        });
        Some((threads, pulse))
    } else {
        None
    };
    let secs = args.secs;
    let t0 = Instant::now();
    let sampler = std::thread::spawn(move || {
        let Some(pid) = pid else {
            return (CpuSplit::default(), t0.elapsed());
        };
        let (process, threads) = cpu_now(pid);
        let mut take = CpuSampler::begin(process.unwrap_or(0), &threads);
        let end = t0 + Duration::from_secs(u64::from(secs));
        loop {
            let now = Instant::now();
            if now >= end {
                break;
            }
            std::thread::sleep(THREAD_POLL.min(end - now));
            let (process, threads) = cpu_now(pid);
            take.sample(process, &threads);
        }
        (take.split(), t0.elapsed())
    });
    let video = inst.ctl(&["video", &secs.to_string(), "full", "budget=2048"]);
    let (cpu, recorded) = sampler
        .join()
        .unwrap_or((CpuSplit::default(), Duration::ZERO));
    done.store(true, Ordering::Relaxed);
    if let Some((threads, pulse)) = burn {
        for t in threads {
            let _ = t.join();
        }
        let _ = pulse.join();
    }
    let metrics_after = inst.ctl(&["metrics"]).stdout;
    let pct_after = inst.ctl(&["metrics", "percentiles"]).stdout;
    let said = format!("{} {}", video.stdout, video.stderr);
    let index = said
        .split_whitespace()
        .rev()
        .find(|w| w.ends_with("index.json"))
        .map(PathBuf::from)
        .ok_or_else(|| Fail::die(format!("video named no index.json: {}", said.trim())))?;
    copy_take(index.parent().unwrap_or(Path::new("/")), dir)?;
    let frames = read_take(&dir.join("index.json"), &args.geom).map_err(Fail::die)?;
    crate::util::write_fresh(&dir.join("frames.tsv"), frames.tsv())
        .map_err(|e| Fail::die(format!("cannot write frames.tsv: {e}")))?;
    let mut text = frames.summary(&args.geom, args.panel_hz.unwrap_or(HEADLESS_HZ));
    let (before, after) = (fields(&metrics_before), fields(&metrics_after));
    let (pb, pa) = (fields(&pct_before), fields(&pct_after));
    let _ = writeln!(text, "{}", split_line(&cpu, recorded.as_secs_f64()));
    for key in [
        "deadline_fires_by_owner",
        "redraw_attempts",
        "redraw_early_outs",
        "redraw_overlay_presents",
        "redraw_overlay_refusals",
        "event_wakes",
    ] {
        let _ = writeln!(
            text,
            "metrics {key}: before {} after {}",
            before.get(key).map_or("-", String::as_str),
            after.get(key).map_or("-", String::as_str),
        );
    }
    let mut json = aterm_json::Map::new();
    let num = |v: f64| {
        if v.is_finite() {
            aterm_json::Value::Number(aterm_json::Number::from(v))
        } else {
            aterm_json::Value::Null
        }
    };
    json.insert("frames".into(), num(frames.rows.len() as f64));
    json.insert("presents_per_s".into(), num(frames.presents_per_s()));
    for (key, ns) in [
        ("process_cpu_s", cpu.process),
        ("main_cpu_s", cpu.main),
        ("app_cpu_s", cpu.app()),
        ("capture_cpu_s", cpu.capture),
        ("unseen_cpu_s", cpu.unseen()),
    ] {
        json.insert(key.into(), num(ns as f64 / 1e9));
    }
    let mut line = String::from("after (the take's own, reset before it):");
    for key in [
        "n_render",
        "render_p50_ms",
        "render_p95_ms",
        "render_p99_ms",
        "n_pre_present",
        "pre_present_p50_ms",
        "pre_present_p95_ms",
        "n_gpu_time",
        "gpu_time_p50_ms",
        "gpu_time_p95_ms",
        "main_busy_ms_per_s",
        "n_deck_frame",
        "deck_frame_p50_ms",
        "deck_frame_p95_ms",
        "deck_frame_p99_ms",
        "n_present",
        "redraw_overlay_presents",
        "redraw_overlay_refusals",
        "tray_uploads",
        "tray_upload_bytes",
        "tray_compare_bytes",
        "deck_clock_ticks",
        "deck_clock_frames",
        "deck_clock_run_ms",
        "n_deck_glass_late",
        "deck_glass_late_p50_ms",
        "deck_glass_late_p95_ms",
    ] {
        let v = pa.get(key).or_else(|| after.get(key));
        if let Some(v) = v {
            let _ = write!(line, " {key}={v}");
            if let Ok(x) = v.parse::<f64>() {
                json.insert(key.into(), num(x));
            }
        }
    }
    text.push_str(&line);
    text.push('\n');
    if let (Some(up), Some(bytes)) = (
        after
            .get("tray_uploads")
            .and_then(|v| v.parse::<f64>().ok()),
        after
            .get("tray_upload_bytes")
            .and_then(|v| v.parse::<f64>().ok()),
    ) && up > 0.0
    {
        let _ = writeln!(text, "tray upload per upload: {:.0} B", bytes / up);
    }
    let _ = writeln!(
        text,
        "before (since boot): {}",
        ["n_render", "render_p50_ms", "main_busy_ms_per_s"]
            .iter()
            .map(|k| format!("{k}={}", pb.get(*k).map_or("-", String::as_str)))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let _ = writeln!(text, "load {}", load_average());
    crate::util::write_fresh(&dir.join("summary.txt"), &text)
        .map_err(|e| Fail::die(format!("cannot write summary.txt: {e}")))?;
    crate::util::write_fresh(
        &dir.join("metrics.txt"),
        format!("before {metrics_before}before {pct_before}after {metrics_after}after {pct_after}"),
    )
    .map_err(|e| Fail::die(format!("cannot write metrics.txt: {e}")))?;
    Ok(TakeReport { text, json })
}

/// The 1, 5 and 15 minute load averages.
fn load_average() -> String {
    aterm_sysprobe::load_average().map_or_else(
        || "?".to_string(),
        |avg| format!("{:.2} {:.2} {:.2}", avg[0], avg[1], avg[2]),
    )
}

/// Copy a recording's directory (its frames and index) into `dir`.
fn copy_take(from: &Path, dir: &Path) -> Result<(), Fail> {
    std::fs::create_dir_all(dir)
        .map_err(|e| Fail::die(format!("cannot make {}: {e}", dir.display())))?;
    let entries = std::fs::read_dir(from)
        .map_err(|e| Fail::die(format!("cannot read {}: {e}", from.display())))?;
    for entry in entries.flatten() {
        let is_file = entry
            .file_type()
            .map(|ft| ft.is_file() || (ft.is_symlink() && entry.path().is_file()))
            .unwrap_or(false);
        if is_file {
            let path = entry.path();
            let to = dir.join(entry.file_name());
            crate::util::copy_regular_file(&path, &to)
                .map_err(|e| Fail::die(format!("cannot copy {}: {e}", path.display())))?;
        }
    }
    Ok(())
}

/// What a frame changed against the one before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    First,
    Dial,
    DialRest,
    Rest,
    Outside,
    Identical,
}

impl Kind {
    const fn name(self) -> &'static str {
        match self {
            Kind::First => "first",
            Kind::Dial => "dial",
            Kind::DialRest => "dial+rest",
            Kind::Rest => "rest",
            Kind::Outside => "outside-only",
            Kind::Identical => "identical",
        }
    }
}

/// One recorded frame, read.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub n: u64,
    pub t_us: i64,
    pub kind: Kind,
    /// The needle's angle, degrees clockwise from straight up; `None` with no needle ink.
    pub angle: Option<f64>,
    pub weight: f64,
}

/// A take's frames, read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frames {
    pub rows: Vec<Row>,
}

/// Map 3's percentile: the sorted sample at `round(q·(n−1))`.
fn pct(xs: &[f64], q: f64) -> f64 {
    if xs.is_empty() {
        return f64::NAN;
    }
    let mut v = xs.to_vec();
    v.sort_by(f64::total_cmp);
    let k = ((q / 100.0) * (v.len() - 1) as f64).round_ties_even() as usize;
    v[k.min(v.len() - 1)]
}

/// What changed between `prev` and `next` (RGBA rows of `w` px), alpha aside.
#[must_use]
pub fn classify(prev: &[u8], next: &[u8], w: usize, g: &Geom) -> Kind {
    let h = next.len() / (w * 4);
    let differs = |y: usize, x0: usize, x1: usize| {
        let at = |x: usize| (y * w + x) * 4;
        (x0..x1.min(w)).any(|x| prev[at(x)..at(x) + 3] != next[at(x)..at(x) + 3])
    };
    let (y0, y1) = (g.deck_y0.min(h), g.deck_y1.min(h));
    let dial = (y0..y1).any(|y| differs(y, 0, g.dial_x1));
    let rest = (y0..y1).any(|y| differs(y, g.dial_x1, w));
    let outside = (0..y0).chain(y1..h).any(|y| differs(y, 0, w));
    match (dial, rest, outside) {
        (true, true, _) => Kind::DialRest,
        (true, false, _) => Kind::Dial,
        (false, true, _) => Kind::Rest,
        (false, false, true) => Kind::Outside,
        (false, false, false) => Kind::Identical,
    }
}

/// The needle's angle in an RGBA frame `w` px wide: the weighted centroid of its near-white ink
/// (each channel over 200, weighing `(min − 200)/55`) inside the annulus `r0..=r1` about the
/// dial's centre, in degrees clockwise from straight up, with the ink's total weight.
#[must_use]
pub fn needle_angle(rgba: &[u8], w: usize, g: &Geom) -> (Option<f64>, f64) {
    let h = rgba.len() / (w * 4);
    let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
    let span = |c: f64, max: usize| {
        let lo = (c - g.r1).max(0.0) as usize;
        let hi = ((c + g.r1) as usize).min(max.saturating_sub(1));
        lo..=hi
    };
    for y in span(g.cy, h) {
        for x in span(g.cx, w) {
            let (dx, dy) = (x as f64 - g.cx, y as f64 - g.cy);
            let d = dx.hypot(dy);
            if d < g.r0 || d > g.r1 {
                continue;
            }
            let at = (y * w + x) * 4;
            let (r, gg, b) = (rgba[at], rgba[at + 1], rgba[at + 2]);
            if r > 200 && gg > 200 && b > 200 {
                let wt = f64::from(r.min(gg).min(b) - 200) / 55.0;
                sx += dx * wt;
                sy += dy * wt;
                n += wt;
            }
        }
    }
    let angle = (n > 0.0).then(|| sx.atan2(-sy).to_degrees());
    (angle, n)
}

/// Read a recorded take: its index and each frame, in order.
/// # Errors
/// The index or a frame cannot be read or decoded.
pub fn read_take(index: &Path, g: &Geom) -> Result<Frames, String> {
    let base = index.parent().unwrap_or(Path::new("."));
    let text = crate::util::read_regular_text(index)
        .map_err(|e| format!("cannot read {}: {e}", index.display()))?;
    let doc: aterm_json::Value =
        aterm_json::from_str(&text).map_err(|e| format!("{} is not JSON: {e}", index.display()))?;
    let list = doc
        .get("frames")
        .and_then(aterm_json::Value::as_array)
        .ok_or_else(|| format!("{} has no frames", index.display()))?;
    let mut rows = Vec::with_capacity(list.len());
    let mut prev: Option<(Vec<u8>, usize)> = None;
    for f in list {
        let file = f
            .get("file")
            .and_then(aterm_json::Value::as_str)
            .ok_or("a frame names no file")?;
        let n = f.get("n").and_then(aterm_json::Value::as_u64).unwrap_or(0);
        let t_us = f
            .get("t_us")
            .and_then(aterm_json::Value::as_i64)
            .ok_or("a frame has no t_us")?;
        let bytes =
            crate::util::read_regular_file(&base.join(file)).map_err(|e| format!("{file}: {e}"))?;
        let (rgba, w, _) =
            aterm_png::decode_rgba8(&bytes, 1 << 15).ok_or_else(|| format!("{file}: not a PNG"))?;
        let kind = match &prev {
            Some((p, pw)) if *pw == w && p.len() == rgba.len() => classify(p, &rgba, w, g),
            Some(_) => Kind::DialRest,
            None => Kind::First,
        };
        let (angle, weight) = needle_angle(&rgba, w, g);
        rows.push(Row {
            n,
            t_us,
            kind,
            angle,
            weight,
        });
        prev = Some((rgba, w));
    }
    Ok(Frames { rows })
}

impl Frames {
    fn span_s(&self) -> f64 {
        match (self.rows.first(), self.rows.last()) {
            (Some(a), Some(b)) => (b.t_us - a.t_us) as f64 / 1e6,
            _ => 0.0,
        }
    }

    fn presents_per_s(&self) -> f64 {
        let span = self.span_s();
        if span > 0.0 {
            (self.rows.len() - 1) as f64 / span
        } else {
            0.0
        }
    }

    /// Per frame: n, ms since the first, gap, what changed, the needle's angle and its step.
    #[must_use]
    pub fn tsv(&self) -> String {
        let mut out = String::from("n\tt_ms\tgap_ms\tkind\tangle_deg\td_angle_deg\tweight\n");
        let t0 = self.rows.first().map_or(0, |r| r.t_us);
        let mut prev: Option<&Row> = None;
        for r in &self.rows {
            let gap = prev.map_or(0.0, |p| (r.t_us - p.t_us) as f64 / 1000.0);
            let step = match (prev.and_then(|p| p.angle), r.angle) {
                (Some(a), Some(b)) => format!("{:.2}", b - a),
                _ => String::new(),
            };
            let _ = writeln!(
                out,
                "{}\t{:.1}\t{gap:.1}\t{}\t{}\t{step}\t{:.1}",
                r.n,
                (r.t_us - t0) as f64 / 1000.0,
                r.kind.name(),
                r.angle.map_or(String::new(), |a| format!("{a:.2}")),
                r.weight,
            );
            prev = Some(r);
        }
        out
    }

    /// The take's summary: its presents, what they changed, and the needle's steps.
    #[must_use]
    /// The frames' summary, the needle's cadence read against a `panel_hz` vsync grid.
    pub fn summary(&self, g: &Geom, panel_hz: u32) -> String {
        let mut out = String::new();
        let span = self.span_s();
        let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
        for r in self.rows.iter().skip(1) {
            *kinds.entry(r.kind.name()).or_default() += 1;
        }
        let _ = writeln!(
            out,
            "frames={} span_s={span:.2} presents_per_s={:.2} kinds={}",
            self.rows.len(),
            self.presents_per_s(),
            kinds
                .iter()
                .map(|(k, n)| format!("{k}:{n}"))
                .collect::<Vec<_>>()
                .join(",")
        );
        for (tag, of) in [
            ("dial", [Kind::Dial, Kind::DialRest].as_slice()),
            ("rest", [Kind::Rest, Kind::DialRest].as_slice()),
        ] {
            let at: Vec<i64> = self
                .rows
                .iter()
                .filter(|r| of.contains(&r.kind))
                .map(|r| r.t_us)
                .collect();
            let gaps: Vec<f64> = at
                .windows(2)
                .map(|w| (w[1] - w[0]) as f64 / 1000.0)
                .collect();
            if gaps.is_empty() {
                let _ = writeln!(out, "{tag}-changing frames={}", at.len());
                continue;
            }
            let short: Vec<f64> = gaps.iter().copied().filter(|g| *g < 200.0).collect();
            let per_s = if span > 0.0 {
                at.len() as f64 / span
            } else {
                0.0
            };
            let _ = writeln!(
                out,
                "{tag}-changing frames={} per_s={per_s:.2} gap_ms p50={:.1} p95={:.1} min={:.1} | gaps<200ms n={} p50={:.1} max={:.1}",
                at.len(),
                pct(&gaps, 50.0),
                pct(&gaps, 95.0),
                gaps.iter().copied().fold(f64::INFINITY, f64::min),
                short.len(),
                pct(&short, 50.0),
                short.iter().copied().fold(f64::NAN, f64::max),
            );
        }
        let steps: Vec<f64> = self
            .rows
            .windows(2)
            .filter(|w| matches!(w[1].kind, Kind::Dial | Kind::DialRest))
            .filter_map(|w| Some((w[1].angle? - w[0].angle?).abs()))
            .filter(|d| *d > 0.05)
            .collect();
        let first = self.rows.iter().find_map(|r| r.angle);
        let last = self.rows.iter().rev().find_map(|r| r.angle);
        let tip = |deg: f64| deg.to_radians() * g.tip_r;
        let _ = writeln!(
            out,
            "needle: {} -> {} deg; {} moving frames, step p50={:.2} deg ({:.1} px of tip) p95={:.2} deg max={:.2} deg ({:.1} px)",
            first.map_or("-".to_string(), |a| format!("{a:.2}")),
            last.map_or("-".to_string(), |a| format!("{a:.2}")),
            steps.len(),
            pct(&steps, 50.0),
            tip(pct(&steps, 50.0)),
            pct(&steps, 95.0),
            steps.iter().copied().fold(f64::NAN, f64::max),
            tip(steps.iter().copied().fold(f64::NAN, f64::max)),
        );
        let _ = writeln!(out, "{}", self.needle_cadence(panel_hz));
        out
    }

    /// The needle's cadence: the gaps between frames that move it (under 200 ms, so a sweep's
    /// own) against the `panel_hz` grid, each `|gap - k·P|` for its nearest whole `k`, and the
    /// dial frames that move no needle (a reading's or the steam's frames, which the live grid
    /// draws, not the deck clock).
    fn needle_cadence(&self, panel_hz: u32) -> String {
        let period = 1000.0 / f64::from(panel_hz.max(1));
        let dial = |r: &Row| matches!(r.kind, Kind::Dial | Kind::DialRest);
        let moving: Vec<i64> = self
            .rows
            .windows(2)
            .filter(|w| dial(&w[1]))
            .filter(|w| {
                w[0].angle
                    .zip(w[1].angle)
                    .is_some_and(|(a, b)| (b - a).abs() > 0.05)
            })
            .map(|w| w[1].t_us)
            .collect();
        let still = self
            .rows
            .windows(2)
            .filter(|w| dial(&w[1]))
            .filter(|w| {
                w[0].angle
                    .zip(w[1].angle)
                    .is_none_or(|(a, b)| (b - a).abs() <= 0.05)
            })
            .count();
        let off: Vec<f64> = moving
            .windows(2)
            .map(|w| (w[1] - w[0]) as f64 / 1000.0)
            .filter(|gap| *gap < 200.0)
            .map(|gap| (gap - (gap / period).round().max(1.0) * period).abs())
            .collect();
        format!(
            "needle cadence on the {panel_hz} Hz grid: {} moving frames, {} gaps <200ms off the grid p50={:.2} p95={:.2} max={:.2} ms, {} over 2 ms; {still} dial frames move no needle",
            moving.len(),
            off.len(),
            pct(&off, 50.0),
            pct(&off, 95.0),
            off.iter().copied().fold(f64::NAN, f64::max),
            off.iter().filter(|o| **o > 2.0).count(),
        )
    }
}

#[cfg(test)]
#[path = "deck_motion_tests.rs"]
mod tests;
