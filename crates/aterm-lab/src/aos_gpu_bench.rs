// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aos-gpu-bench`: design §12.9's pre-registered M5a measurement, four arms of the release `aos_gpu` harness in a Williams order, judged by gates G0–G6 and criteria C1–C6 into `DIR/record.md`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use aterm_sysprobe::{GpuDriver, GpuReading};
use aterm_uds::spawnfd::Stdio;

use crate::aos_workers_bench::{load_average, median, required_valid, span_ratio, williams};
use crate::instance::take_value;
use crate::paths::{from_caller, paths};
use crate::stop::{self, Stopped};
use crate::sys::{self, digest_file};
use crate::{Fail, Res};

/// The verb's own help (`aos-gpu-bench --help`).
pub const HELP: &str = "\
usage: targo --unverified lab aos-gpu-bench --harness PATH --out DIR [--rounds N]

Design §12.9's pre-registered measurement of complete submission-to-result time and resident memory for Aos M5a's
shared-buffer inference (MEASURED, never scored; it gates no build or push). Four arms, each one run of the release
harness `aos_gpu --arm X` in its own process: F (the shipped classify_opt per text), I (forward_fast on one
thread), T (forward_fast over available_parallelism() threads) and G (the Metal compute job). 12 rounds in a
Williams order (FIGT, ITFG, TGIF, GFTI, three times over). Before each run the runner waits 20 s idle and records
the load average, vm.swapusage, `pmset -g` (gated: lowpowermode 0), `pmset -g ps` (gated: AC power), the GPU's
utilization and recovery counter (before and after) and the AGX driver's bundle and version; it stamps the spawn on
mach_absolute_time, blocks for the exit (no polling) and kills the run at a 120 s deadline (void). Then the gates
G0-G6, the criteria C1-C6 and the medians over valid rounds of every figure are written with every run's
conditions to DIR/record.md, which is printed.

  --harness PATH  the release `aos_gpu` (targo --unverified build --release -p aterm-bench --example aos_gpu
                  --features aos-gpu), built on this runner's clean commit and copied out of the target directory
  --out DIR       absent or empty, OUTSIDE the repository: record.md and runs/ (each run's summary, its .raw
                  sidecar, the runner's .runner sidecar of every reading it made, stdout and stderr) go here
  --rounds N      1 to 12 (default 12). Any N below 12 is a SMOKE run: printed as such, never the registered
                  session, and no claim is made from it

exit: 0 a valid session; 1 a void or insufficient session; 2 a defect stopped it (or usage); 128+N interrupted by
signal N (the record so far is written)
";

/// The registered session's rounds.
pub const REGISTERED_ROUNDS: usize = 12;

/// The idle wait before each run.
pub const IDLE: Duration = Duration::from_secs(20);

/// Each run's deadline (G5).
pub const DEADLINE: Duration = Duration::from_secs(120);

/// G5's bound on a run's lifetime maximum `phys_footprint`.
pub const FOOTPRINT_MAX: u64 = 2 << 30;

/// G4: F's warm B371 median spans at most this across valid rounds.
pub const F_SPAN_MAX: f64 = 1.3;

/// G3: a salted pipeline built faster than this hit Metal's function cache.
pub const COLD_PIPELINE_MIN_NS: u64 = 2_000_000;

/// The harness's digest domain (G0).
pub const HARNESS_DOMAIN: &[u8] = b"aterm-aos-m5a-harness-v1\0";

/// The summary format the harness writes.
pub const FORMAT: &str = "aos-gpu-v1";

/// The registered repetitions of the four series.
pub const REPS: [u64; 4] = [1000, 100, 500, 200];

/// The registered gap between isolated B1 repetitions, ms.
pub const ISO_MS: u64 = 100;

/// The registered build stamp's profile (G0).
pub const RELEASE_PROFILE: &str = "release opt-level=3";

/// The harness's exit after a G3 failure, its summary written.
pub const EXIT_GPU: i32 = 3;

/// The warm series in order, and their rows.
pub const SERIES: [(&str, u64); 4] = [("b1", 1), ("b1iso", 1), ("b371", 371), ("b16384", 16_384)];

/// G's split, in order.
pub const PARTS: [&str; 8] = [
    "pack", "encode", "schedule", "queue", "gpu", "notify", "wake", "readback",
];

/// The memory points.
pub const POINTS: [&str; 8] = ["P0", "P1", "P2", "P3", "P4", "P5", "P6", "P7"];

/// The five figures of a memory point, in the summary's order.
pub const POINT_FIGURES: [&str; 5] = [
    "phys_footprint",
    "currentAllocatedSize",
    "declared buffers",
    "heap live",
    "heap peak",
];

const MIB: f64 = 1_048_576.0;

/// Summary keys that are identities, not figures: no median is taken of them.
const NOT_FIGURES: [&str; 26] = [
    "entry_ticks",
    "format",
    "arm",
    "run_id",
    "build_commit",
    "build_profile",
    "build_toolchain",
    "debug_assertions",
    "hw_model",
    "kern_osversion",
    "timebase",
    "qos",
    "reps",
    "iso_ms",
    "device_name",
    "device_registry_id",
    "library_salt",
    "run_salt",
    "pipeline_identity",
    "workload_digest",
    "workload_digest_pin",
    "tone_int_id",
    "tone_int_id_pin",
    "b371_out_digest",
    "b371_out_digest_pin",
    "last_ticks",
];

/// One arm of design §12.9.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Arm {
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
    /// The arms in the Williams square's numbering.
    pub const ALL: [Arm; 4] = [Arm::F, Arm::I, Arm::T, Arm::G];

    /// The arm's letter.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Arm::F => "F",
            Arm::I => "I",
            Arm::T => "T",
            Arm::G => "G",
        }
    }

    /// The arm a letter names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Arm> {
        Arm::ALL.into_iter().find(|a| a.name() == s)
    }
}

/// The session's order: round r (from 0) runs the Williams square's order r mod 4, so 12 rounds are FIGT, ITFG, TGIF, GFTI three times over.
#[must_use]
pub fn schedule(rounds: usize) -> Vec<[Arm; 4]> {
    let square = williams(Arm::ALL.len()).unwrap_or_default();
    (0..rounds)
        .map(|r| {
            let mut order = Arm::ALL;
            if let Some(row) = square.get(r % Arm::ALL.len()) {
                for (slot, i) in order.iter_mut().zip(row) {
                    if let Some(arm) = Arm::ALL.get(*i) {
                        *slot = *arm;
                    }
                }
            }
            order
        })
        .collect()
}

/// What the verb was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    /// The harness, as given.
    pub harness: String,
    /// The output directory, as given.
    pub out: String,
    /// `--rounds`.
    pub rounds: usize,
}

/// The verb's arguments.
/// # Errors
/// A usage error: an unknown word, a flag without its value, no `--harness` or `--out`, or `--rounds` outside 1 to 12.
pub fn parse_args(args: &[String]) -> Result<Args, Fail> {
    let mut a = args.to_vec();
    let harness = take_value(&mut a, "--harness")?;
    let out = take_value(&mut a, "--out")?;
    let rounds = take_value(&mut a, "--rounds")?;
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "aos-gpu-bench: unexpected '{extra}' (aos-gpu-bench --help)"
        )));
    }
    let harness =
        harness.ok_or_else(|| Fail::usage("aos-gpu-bench: --harness PATH is required"))?;
    let out = out.ok_or_else(|| Fail::usage("aos-gpu-bench: --out DIR is required"))?;
    let rounds = match rounds {
        None => REGISTERED_ROUNDS,
        Some(v) => v
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=REGISTERED_ROUNDS).contains(n))
            .ok_or_else(|| {
                Fail::usage(format!(
                    "aos-gpu-bench: --rounds takes 1 to {REGISTERED_ROUNDS}, not '{v}'"
                ))
            })?,
    };
    Ok(Args {
        harness,
        out,
        rounds,
    })
}

/// A harness summary: its `key=value` lines in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pairs: Vec<(String, String)>,
}

impl Summary {
    /// The value as written, `-` included.
    #[must_use]
    pub fn raw(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// The value, `None` for `-`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.raw(key).filter(|v| *v != "-")
    }

    /// The value as one count.
    #[must_use]
    pub fn int(&self, key: &str) -> Option<u64> {
        self.get(key)?.parse().ok()
    }

    /// Word `i` of the value as a count.
    #[must_use]
    pub fn nth(&self, key: &str, i: usize) -> Option<u64> {
        self.get(key)?.split_whitespace().nth(i)?.parse().ok()
    }

    /// The value's words as counts.
    #[must_use]
    pub fn ints(&self, key: &str) -> Option<Vec<u64>> {
        self.get(key)?
            .split_whitespace()
            .map(|w| w.parse().ok())
            .collect()
    }

    /// The keys in order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.pairs.iter().map(|(k, _)| k.as_str())
    }
}

/// The keys every summary must have, whatever the run's exit.
const REQUIRED: [&str; 8] = [
    "entry_ticks",
    "format",
    "arm",
    "run_id",
    "build_commit",
    "build_profile",
    "debug_assertions",
    "last_ticks",
];

/// A harness summary.
/// # Errors
/// A line with no `=` or an empty key, a key twice, a required key missing, or another format.
pub fn parse_summary(text: &str) -> Result<Summary, String> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once('=').filter(|(k, _)| !k.is_empty()) else {
            return Err(format!("line {} is not key=value: `{line}`", n + 1));
        };
        if pairs.iter().any(|(seen, _)| seen == k) {
            return Err(format!("line {}: {k} twice", n + 1));
        }
        pairs.push((k.to_owned(), v.to_owned()));
    }
    let s = Summary { pairs };
    for k in REQUIRED {
        if s.raw(k).is_none() {
            return Err(format!("no {k}="));
        }
    }
    match s.raw("format") {
        Some(FORMAT) => {}
        other => return Err(format!("format={} is not {FORMAT}", other.unwrap_or(""))),
    }
    if Arm::parse(s.raw("arm").unwrap_or("")).is_none() {
        return Err(format!("arm={} names no arm", s.raw("arm").unwrap_or("")));
    }
    for k in ["entry_ticks", "last_ticks"] {
        if s.int(k).is_none() {
            return Err(format!("{k}= is not a tick count"));
        }
    }
    Ok(s)
}

/// `pmset -g`'s `lowpowermode` setting: true when on.
#[must_use]
pub fn parse_lowpower(text: &str) -> Option<bool> {
    text.lines().find_map(|l| {
        let mut w = l.split_whitespace();
        (w.next() == Some("lowpowermode"))
            .then(|| w.next())
            .flatten()
            .and_then(|v| match v {
                "0" => Some(false),
                "1" => Some(true),
                _ => None,
            })
    })
}

/// `pmset -g ps`'s first line: true on AC power, false on battery.
#[must_use]
pub fn parse_ac(text: &str) -> Option<bool> {
    let first = text.lines().next()?;
    if first.contains("'AC Power'") {
        Some(true)
    } else if first.contains("'Battery Power'") || first.contains("'UPS Power'") {
        Some(false)
    } else {
        None
    }
}

/// `vm.swapusage`'s `used`, in MiB.
#[must_use]
pub fn parse_swap_used(text: &str) -> Option<f64> {
    let rest = text.split_once("used = ")?.1;
    let word = rest.split_whitespace().next()?;
    let (num, unit) = word.split_at(word.len().checked_sub(1)?);
    let v: f64 = num.parse().ok()?;
    match unit {
        "M" => Some(v),
        "G" => Some(v * 1024.0),
        "K" => Some(v / 1024.0),
        _ => None,
    }
}

/// What the runner read of the machine around one run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Conditions {
    /// The 1-minute load average before the run.
    pub load1: Option<f64>,
    /// `sysctl -n vm.swapusage`'s output, verbatim.
    pub swap: Option<String>,
    /// `pmset -g`'s output, verbatim.
    pub pmset: Option<String>,
    /// `pmset -g ps`'s output, verbatim.
    pub pmset_ps: Option<String>,
    /// The GPU just before the spawn.
    pub gpu_before: Option<GpuReading>,
    /// The GPU just after the exit.
    pub gpu_after: Option<GpuReading>,
    /// The AGX driver's identity.
    pub driver: Option<GpuDriver>,
    /// What could not be read, and why.
    pub notes: Vec<String>,
}

impl Conditions {
    /// `pmset -g`'s `lowpowermode` is on.
    #[must_use]
    pub fn lowpower(&self) -> Option<bool> {
        parse_lowpower(self.pmset.as_deref()?)
    }

    /// `pmset -g ps` says AC power.
    #[must_use]
    pub fn ac(&self) -> Option<bool> {
        parse_ac(self.pmset_ps.as_deref()?)
    }

    /// `pmset -g ps`'s first line.
    #[must_use]
    pub fn power(&self) -> Option<&str> {
        Some(self.pmset_ps.as_deref()?.lines().next()?.trim())
    }
}

/// One run as the runner saw it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Measured {
    /// `mach_absolute_time` just before the spawn.
    pub spawn_ticks: u64,
    /// `mach_absolute_time` when the blocking wait saw the exit.
    pub exit_ticks: Option<u64>,
    /// The exit status.
    pub exit: Option<i32>,
    /// The signal that ended it.
    pub signal: Option<i32>,
    /// Killed at [`DEADLINE`].
    pub deadline: bool,
    /// `wait4`'s user plus system CPU, ns.
    pub cpu_ns: Option<u64>,
    /// `wait4`'s `ru_maxrss`, bytes.
    pub maxrss: Option<u64>,
    /// The spawn or the wait failed.
    pub error: Option<String>,
    /// What it printed.
    pub stdout: String,
    /// What it printed to stderr.
    pub stderr: String,
}

/// A gate a run or the session can fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// The binary and the commit.
    G0,
    /// The base reproduces.
    G1,
    /// The GPU behaves.
    G3,
    /// The base is stable.
    G4,
    /// Budgets.
    G5,
    /// Enough data.
    G6,
}

impl Gate {
    /// Its name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Gate::G0 => "G0",
            Gate::G1 => "G1",
            Gate::G3 => "G3",
            Gate::G4 => "G4",
            Gate::G5 => "G5",
            Gate::G6 => "G6",
        }
    }
}

/// What one run's own evidence says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Every per-run gate held.
    Valid,
    /// These failed.
    Invalid(Vec<(Gate, String)>),
    /// A defect, which stops the session.
    Defect(String),
}

/// One run.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    /// Its `--run-id`.
    pub id: String,
    /// Its round, from 1.
    pub round: usize,
    /// Its position in the round, from 1.
    pub pos: usize,
    /// Its arm.
    pub arm: Arm,
    /// The machine around it.
    pub cond: Conditions,
    /// What the runner measured.
    pub m: Measured,
    /// Its summary, or why there is none.
    pub summary: Result<Summary, String>,
    /// Its verdict.
    pub verdict: Verdict,
}

/// Ticks to ns on `tb`.
#[must_use]
pub fn ticks_ns(tb: (u32, u32), ticks: u64) -> u64 {
    let ns = u128::from(ticks) * u128::from(tb.0) / u128::from(tb.1.max(1));
    u64::try_from(ns).unwrap_or(u64::MAX)
}

impl Run {
    /// The summary, if it parsed.
    #[must_use]
    pub fn s(&self) -> Option<&Summary> {
        self.summary.as_ref().ok()
    }

    /// Process creation: the child's entry stamp less the spawn stamp, ns.
    #[must_use]
    pub fn creation_ns(&self, tb: (u32, u32)) -> Option<u64> {
        let entry = self.s()?.int("entry_ticks")?;
        Some(ticks_ns(tb, entry.checked_sub(self.m.spawn_ticks)?))
    }

    /// Exit: the wait's return less the child's last stamp, ns.
    #[must_use]
    pub fn exit_ns(&self, tb: (u32, u32)) -> Option<u64> {
        let last = self.s()?.int("last_ticks")?;
        Some(ticks_ns(tb, self.m.exit_ticks?.checked_sub(last)?))
    }

    /// Spawn to the exit seen, ns.
    #[must_use]
    pub fn wall_ns(&self, tb: (u32, u32)) -> Option<u64> {
        Some(ticks_ns(
            tb,
            self.m.exit_ticks?.checked_sub(self.m.spawn_ticks)?,
        ))
    }

    /// The run's p50 of `figure` in `series`, ns.
    #[must_use]
    pub fn p50(&self, series: &str, figure: Figure) -> Option<f64> {
        let key = format!("{series}.{}", figure.key());
        self.s()?.nth(&key, 0).map(|v| v as f64)
    }
}

fn exit_text(m: &Measured) -> String {
    match (m.exit, m.signal) {
        (Some(c), _) => format!("status {c}"),
        (None, Some(s)) => format!("by signal {s}"),
        (None, None) => "unknown".to_owned(),
    }
}

fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(3);
    lines[start..].join(" | ")
}

/// The figures a valid run's summary must hold.
fn required_figures(arm: Arm) -> Vec<String> {
    let mut keys = vec!["lifetime_max_footprint".to_owned()];
    for (s, _) in SERIES {
        for k in ["reps", "rows", "e2e_ns", "e2e_sum_ns", "cpu_ns"] {
            keys.push(format!("{s}.{k}"));
        }
        if arm != Arm::F {
            keys.push(format!("{s}.packed_ns"));
        }
        if arm == Arm::G {
            keys.push(format!("{s}.split_valid"));
            keys.push(format!("{s}.part_invalid"));
        }
    }
    if arm == Arm::G {
        keys.extend(
            [
                "cold.start_cold_ns",
                "cold.pipeline_salted_ns",
                "cold.pipeline_ns",
                "limits",
            ]
            .map(str::to_owned),
        );
    }
    keys
}

/// The defect a parsed summary shows against what was spawned, if any.
fn summary_defect(r: &Run, s: &Summary) -> Option<String> {
    let arm = s.raw("arm").unwrap_or("");
    if arm != r.arm.name() {
        return Some(format!(
            "arm mismatch: spawned {}, the summary says {arm}",
            r.arm.name()
        ));
    }
    if s.raw("run_id") != Some(r.id.as_str()) {
        return Some(format!(
            "the summary's run_id={} is not {}",
            s.raw("run_id").unwrap_or(""),
            r.id
        ));
    }
    let reps = REPS.map(|n| n.to_string()).join(" ");
    if s.raw("reps") != Some(reps.as_str()) || s.int("iso_ms") != Some(ISO_MS) {
        return Some(format!(
            "not the registered repetitions: reps={} iso_ms={}",
            s.raw("reps").unwrap_or(""),
            s.raw("iso_ms").unwrap_or("")
        ));
    }
    let failure = s.raw("gpu_failure").unwrap_or("");
    match r.m.exit {
        Some(EXIT_GPU) if r.arm != Arm::G || failure == "none" || failure.is_empty() => {
            return Some(format!(
                "exit {EXIT_GPU} without a G run's gpu_failure (gpu_failure={failure})"
            ));
        }
        Some(EXIT_GPU) => return None,
        _ if failure != "none" => {
            return Some(format!("exit 0 with gpu_failure={failure}"));
        }
        _ => {}
    }
    for (i, (name, rows)) in SERIES.iter().enumerate() {
        if s.int(&format!("{name}.reps")) != Some(REPS[i])
            || s.int(&format!("{name}.rows")) != Some(*rows)
        {
            return Some(format!(
                "series {name} ran {} repetitions of {} rows, not {} of {rows}",
                s.raw(&format!("{name}.reps")).unwrap_or("none"),
                s.raw(&format!("{name}.rows")).unwrap_or("none"),
                REPS[i]
            ));
        }
    }
    if let Some(k) = required_figures(r.arm)
        .into_iter()
        .find(|k| s.ints(k).is_none_or(|v| v.is_empty()))
    {
        return Some(format!(
            "unparsable summary: {k}={}",
            s.raw(&k).unwrap_or("(absent)")
        ));
    }
    if r.arm != Arm::F {
        let (got, pin) = (s.raw("b371_out_digest"), s.raw("b371_out_digest_pin"));
        if got.is_none() || got != pin {
            return Some(format!(
                "C0 (G2): the run's reference forward of B371 digests to {}, not the pin {}",
                got.unwrap_or("(absent)"),
                pin.unwrap_or("(absent)")
            ));
        }
    }
    None
}

fn pin(out: &mut Vec<(Gate, String)>, s: &Summary, key: &str) {
    let (got, want) = (s.raw(key), s.raw(&format!("{key}_pin")));
    if got.is_none() || got == Some("-") || got != want {
        out.push((
            Gate::G1,
            format!(
                "{key}={} is not the pin {}",
                got.unwrap_or("(absent)"),
                want.unwrap_or("(absent)")
            ),
        ));
    }
}

/// G1's checks of one run.
fn gate1(r: &Run, s: &Summary, out: &mut Vec<(Gate, String)>) {
    pin(out, s, "workload_digest");
    match r.arm {
        Arm::F => pin(out, s, "f_argmax"),
        _ if r.m.exit == Some(0) => pin(out, s, "tone_int_id"),
        _ => {}
    }
}

/// G3's per-run checks of a G run.
fn gate3(r: &Run, s: &Summary, out: &mut Vec<(Gate, String)>) {
    if r.arm != Arm::G {
        return;
    }
    let mut miss = |why: String| out.push((Gate::G3, why));
    match s.raw("gpu_failure") {
        Some("none") => {}
        other => miss(format!("gpu_failure={}", other.unwrap_or("(absent)"))),
    }
    if s.raw("qualified") != Some("1 1") {
        miss(format!(
            "qualified={} (salted canonical): not both passed",
            s.raw("qualified").unwrap_or("(absent)")
        ));
    }
    if s.raw("cold.cache_hit") != Some("0") {
        miss(format!(
            "cold.cache_hit={}",
            s.raw("cold.cache_hit").unwrap_or("(absent)")
        ));
    }
    match s.int("cold.pipeline_salted_ns") {
        Some(ns) if ns >= COLD_PIPELINE_MIN_NS => {}
        other => miss(format!(
            "the cold pipeline took {} ns, under 2 ms: the function-cache miss did not happen",
            opt(other)
        )),
    }
    match s.int("cold.pipeline_ns") {
        Some(ns) if ns < COLD_PIPELINE_MIN_NS => {}
        Some(ns) => miss(format!(
            "the canonical pipeline took {ns} ns, at least 2 ms: the warm-cache start missed the cache"
        )),
        None => miss("the canonical pipeline's time was not written".to_owned()),
    }
    let recov = |g: &Option<GpuReading>| g.as_ref().and_then(|g| g.recoveries);
    match (recov(&r.cond.gpu_before), recov(&r.cond.gpu_after)) {
        (Some(a), Some(b)) if a == b => {}
        (Some(a), Some(b)) => miss(format!("the GPU recovery counter moved {a} -> {b}")),
        _ => miss("the GPU recovery counter was not read before and after".to_owned()),
    }
}

/// G5's per-run checks.
fn gate5(r: &Run, s: &Summary, out: &mut Vec<(Gate, String)>) {
    let mut miss = |why: String| out.push((Gate::G5, why));
    match s.int("lifetime_max_footprint") {
        Some(b) if b <= FOOTPRINT_MAX => {}
        other => miss(format!(
            "lifetime max phys_footprint {} bytes, over 2 GiB or unread",
            opt(other)
        )),
    }
    if r.arm == Arm::G && r.m.exit == Some(0) {
        let limits = s.ints("limits").unwrap_or_default();
        for (key, i, name) in [
            ("params_bytes", 0, "params"),
            ("job_bytes", 1, "job"),
            ("declared_max", 2, "domain"),
        ] {
            match (s.int(key), limits.get(i)) {
                (Some(v), Some(l)) if v <= *l => {}
                (v, l) => miss(format!(
                    "{key}={} is not within the {name} limit {}",
                    opt(v),
                    opt(l)
                )),
            }
        }
    }
    if r.arm == Arm::T {
        match (s.int("pool_qos_set"), s.int("pool_qos_wanted")) {
            (Some(n), Some(w)) if n >= w => {}
            (n, w) => miss(format!(
                "T's pool: {} of the {} threads asked took the main thread's QoS {}",
                opt(n),
                opt(w),
                s.raw("qos").unwrap_or("")
            )),
        }
    }
    match r.cond.lowpower() {
        Some(false) => {}
        Some(true) => miss("Low Power Mode is on".to_owned()),
        None => miss("pmset -g gave no lowpowermode".to_owned()),
    }
    match r.cond.ac() {
        Some(true) => {}
        Some(false) => miss(format!("not on AC power: {}", r.cond.power().unwrap_or(""))),
        None => miss("pmset -g ps named no power source".to_owned()),
    }
}

/// The class word of a summary's `qos=` (`0x21 0`).
#[must_use]
pub fn qos_class(qos: &str) -> Option<u32> {
    let word = qos.split_whitespace().next()?;
    u32::from_str_radix(word.strip_prefix("0x")?, 16).ok()
}

/// A `qos_class_t` value's name, for display only.
fn qos_name(class: Option<u32>) -> &'static str {
    match class {
        None => "unread",
        Some(0) => "unspecified",
        Some(0x21) => "user-interactive",
        Some(0x19) => "user-initiated",
        Some(0x15) => "default",
        Some(0x11) => "utility",
        Some(0x09) => "background",
        Some(_) => "unknown",
    }
}

/// One run's verdict from its own evidence.
#[must_use]
pub fn judge(r: &Run) -> Verdict {
    if let Some(e) = &r.m.error {
        return Verdict::Defect(format!("the run did not complete: {e}"));
    }
    if r.m.deadline {
        return Verdict::Invalid(vec![(
            Gate::G5,
            format!("killed at the {} s deadline", DEADLINE.as_secs()),
        )]);
    }
    if !matches!(r.m.exit, Some(0 | EXIT_GPU)) {
        return Verdict::Defect(format!(
            "the harness exited {}: {}",
            exit_text(&r.m),
            tail(&r.m.stderr)
        ));
    }
    let s = match &r.summary {
        Ok(s) => s,
        Err(e) => return Verdict::Defect(format!("unparsable summary: {e}")),
    };
    if let Some(d) = summary_defect(r, s) {
        return Verdict::Defect(d);
    }
    let mut out = Vec::new();
    gate1(r, s, &mut out);
    gate3(r, s, &mut out);
    gate5(r, s, &mut out);
    if out.is_empty() {
        Verdict::Valid
    } else {
        Verdict::Invalid(out)
    }
}

/// The runner sidecar's format.
pub const RUNNER_FORMAT: &str = "aos-gpu-runner-v1";

/// A run as its `runs/{id}.runner` sidecar holds it: every reading the runner made, its stdout and stderr aside.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub struct Runner {
    /// The run's id.
    pub id: String,
    /// Its arm.
    pub arm: Arm,
    /// Its round, from 1.
    pub round: usize,
    /// Its position in the round, from 1.
    pub pos: usize,
    /// The runner's `mach_timebase_info`.
    pub timebase: (u32, u32),
    /// What the runner measured, stdout and stderr empty.
    pub m: Measured,
    /// The machine around it.
    pub cond: Conditions,
}

/// A text on one line: `\` `\n` `\r` escaped, `-` for none and `\-` for the text `-`.
fn escape(v: Option<&str>) -> String {
    match v {
        None => "-".to_owned(),
        Some("-") => "\\-".to_owned(),
        Some(t) => {
            let mut o = String::with_capacity(t.len());
            for c in t.chars() {
                match c {
                    '\\' => o.push_str("\\\\"),
                    '\n' => o.push_str("\\n"),
                    '\r' => o.push_str("\\r"),
                    c => o.push(c),
                }
            }
            o
        }
    }
}

#[cfg(test)]
fn unescape(v: &str) -> Result<Option<String>, String> {
    if v == "-" {
        return Ok(None);
    }
    let mut o = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            o.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => o.push('\\'),
            Some('n') => o.push('\n'),
            Some('r') => o.push('\r'),
            Some('-') => o.push('-'),
            other => {
                return Err(format!(
                    "`\\{}` is not an escape",
                    other.map(String::from).unwrap_or_default()
                ));
            }
        }
    }
    Ok(Some(o))
}

/// Every reading the runner made of run `r` on timebase `tb`, as `runs/{id}.runner` holds it: `key=value` lines, texts escaped onto their line, `-` for what was not read.
#[must_use]
pub fn runner_text(r: &Run, tb: (u32, u32)) -> String {
    let Measured {
        spawn_ticks,
        exit_ticks,
        exit,
        signal,
        deadline,
        cpu_ns,
        maxrss,
        error,
        stdout: _,
        stderr: _,
    } = &r.m;
    let Conditions {
        load1,
        swap,
        pmset,
        pmset_ps,
        gpu_before,
        gpu_after,
        driver,
        notes,
    } = &r.cond;
    let mut o = String::new();
    let mut put = |k: &str, v: String| {
        let _ = writeln!(o, "{k}={v}");
    };
    put("format", RUNNER_FORMAT.to_owned());
    put("run_id", escape(Some(&r.id)));
    put("arm", r.arm.name().to_owned());
    put("round", r.round.to_string());
    put("pos", r.pos.to_string());
    put("timebase", format!("{}/{}", tb.0, tb.1));
    put("spawn_ticks", spawn_ticks.to_string());
    put("exit_ticks", opt(*exit_ticks));
    put("exit", opt(*exit));
    put("signal", opt(*signal));
    put("deadline", u8::from(*deadline).to_string());
    put("cpu_ns", opt(*cpu_ns));
    put("maxrss", opt(*maxrss));
    put("error", escape(error.as_deref()));
    put("load1", opt(*load1));
    put("swap", escape(swap.as_deref()));
    put("pmset", escape(pmset.as_deref()));
    put("pmset_ps", escape(pmset_ps.as_deref()));
    match driver {
        None => put("driver", "-".to_owned()),
        Some(GpuDriver { bundle, version }) => {
            put("driver", "read".to_owned());
            put("driver.bundle", escape(bundle.as_deref()));
            put("driver.version", escape(version.as_deref()));
        }
    }
    for (which, g) in [("gpu_before", gpu_before), ("gpu_after", gpu_after)] {
        let Some(GpuReading {
            device_pct,
            renderer_pct,
            tiler_pct,
            in_use_bytes,
            alloc_bytes,
            recoveries,
            cores,
            name,
            unread,
        }) = g
        else {
            put(which, "-".to_owned());
            continue;
        };
        put(which, "read".to_owned());
        put(&format!("{which}.device_pct"), opt(*device_pct));
        put(&format!("{which}.renderer_pct"), opt(*renderer_pct));
        put(&format!("{which}.tiler_pct"), opt(*tiler_pct));
        put(&format!("{which}.in_use_bytes"), opt(*in_use_bytes));
        put(&format!("{which}.alloc_bytes"), opt(*alloc_bytes));
        put(&format!("{which}.recoveries"), opt(*recoveries));
        put(&format!("{which}.cores"), opt(*cores));
        put(&format!("{which}.name"), escape(name.as_deref()));
        put(
            &format!("{which}.unread"),
            escape(unread.as_ref().map(ToString::to_string).as_deref()),
        );
    }
    for n in notes {
        put("note", escape(Some(n)));
    }
    o
}

/// A sidecar's `key=value` pairs, each key once.
#[cfg(test)]
struct Fields<'a> {
    map: BTreeMap<&'a str, &'a str>,
}

#[cfg(test)]
impl Fields<'_> {
    fn raw(&self, k: &str) -> Result<&str, String> {
        self.map.get(k).copied().ok_or_else(|| format!("no {k}="))
    }

    fn text(&self, k: &str) -> Result<Option<String>, String> {
        unescape(self.raw(k)?).map_err(|e| format!("{k}: {e}"))
    }

    fn num<T: std::str::FromStr>(&self, k: &str) -> Result<Option<T>, String> {
        match self.raw(k)? {
            "-" => Ok(None),
            v => v
                .parse()
                .map(Some)
                .map_err(|_| format!("{k}={v} is not a number")),
        }
    }

    fn read(&self, k: &str) -> Result<bool, String> {
        match self.raw(k)? {
            "read" => Ok(true),
            "-" => Ok(false),
            v => Err(format!("{k}={v} is neither read nor -")),
        }
    }

    fn gpu(&self, which: &str) -> Result<Option<GpuReading>, String> {
        if !self.read(which)? {
            return Ok(None);
        }
        let k = |f: &str| format!("{which}.{f}");
        let unread = match self.text(&k("unread"))? {
            None => None,
            Some(w) => Some(
                [
                    aterm_sysprobe::Why::Unsupported,
                    aterm_sysprobe::Why::NotRead,
                    aterm_sysprobe::Why::Asleep,
                    aterm_sysprobe::Why::Absent,
                ]
                .into_iter()
                .find(|y| y.to_string() == w)
                .ok_or_else(|| {
                    format!("{which}.unread={w} names no reason a reading here gives")
                })?,
            ),
        };
        Ok(Some(GpuReading {
            device_pct: self.num(&k("device_pct"))?,
            renderer_pct: self.num(&k("renderer_pct"))?,
            tiler_pct: self.num(&k("tiler_pct"))?,
            in_use_bytes: self.num(&k("in_use_bytes"))?,
            alloc_bytes: self.num(&k("alloc_bytes"))?,
            recoveries: self.num(&k("recoveries"))?,
            cores: self.num(&k("cores"))?,
            name: self.text(&k("name"))?,
            unread,
        }))
    }
}

/// A `runs/{id}.runner` sidecar, as [`runner_text`] writes it.
/// # Errors
/// A line with no `=`, a key twice (but `note`), a key missing, a value that does not read back, or another format.
#[cfg(test)]
pub fn parse_runner(text: &str) -> Result<Runner, String> {
    let mut map = BTreeMap::new();
    let mut notes = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once('=').filter(|(k, _)| !k.is_empty()) else {
            return Err(format!("line {} is not key=value: `{line}`", n + 1));
        };
        if k == "note" {
            notes.push(unescape(v)?.unwrap_or_default());
        } else if map.insert(k, v).is_some() {
            return Err(format!("line {}: {k} twice", n + 1));
        }
    }
    let f = Fields { map };
    match f.raw("format")? {
        RUNNER_FORMAT => {}
        other => return Err(format!("format={other} is not {RUNNER_FORMAT}")),
    }
    let arm = f.raw("arm")?;
    let arm = Arm::parse(arm).ok_or_else(|| format!("arm={arm} names no arm"))?;
    let timebase = f
        .raw("timebase")?
        .split_once('/')
        .and_then(|(n, d)| Some((n.parse().ok()?, d.parse().ok()?)))
        .ok_or_else(|| "timebase= is not numer/denom".to_owned())?;
    let driver = if f.read("driver")? {
        Some(GpuDriver {
            bundle: f.text("driver.bundle")?,
            version: f.text("driver.version")?,
        })
    } else {
        None
    };
    Ok(Runner {
        id: f.text("run_id")?.ok_or("no run_id")?,
        arm,
        round: f.num("round")?.ok_or("no round")?,
        pos: f.num("pos")?.ok_or("no pos")?,
        timebase,
        m: Measured {
            spawn_ticks: f.num("spawn_ticks")?.ok_or("no spawn_ticks")?,
            exit_ticks: f.num("exit_ticks")?,
            exit: f.num("exit")?,
            signal: f.num("signal")?,
            deadline: match f.raw("deadline")? {
                "0" => false,
                "1" => true,
                v => return Err(format!("deadline={v} is neither 0 nor 1")),
            },
            cpu_ns: f.num("cpu_ns")?,
            maxrss: f.num("maxrss")?,
            error: f.text("error")?,
            stdout: String::new(),
            stderr: String::new(),
        },
        cond: Conditions {
            load1: f.num("load1")?,
            swap: f.text("swap")?,
            pmset: f.text("pmset")?,
            pmset_ps: f.text("pmset_ps")?,
            gpu_before: f.gpu("gpu_before")?,
            gpu_after: f.gpu("gpu_after")?,
            driver,
            notes,
        },
    })
}

/// What the record says of the session before its runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// Rounds.
    pub rounds: usize,
    /// The session's tag, every run id's prefix.
    pub session: String,
    /// This runner's build commit.
    pub runner_commit: String,
    /// This runner's build profile.
    pub runner_profile: String,
    /// The harness's path.
    pub harness: String,
    /// The harness's digest in [`HARNESS_DOMAIN`], at the start.
    pub harness_digest: String,
    /// The digest at the end equals the start's; `None` until the end.
    pub harness_unchanged: Option<bool>,
    /// The runner's `mach_timebase_info`.
    pub timebase: (u32, u32),
}

impl Header {
    /// Not the registered session.
    #[must_use]
    pub const fn smoke(&self) -> bool {
        self.rounds != REGISTERED_ROUNDS
    }
}

/// A commit stamp of 12 lowercase hex digits and nothing else.
#[must_use]
pub fn clean_commit(stamp: &str) -> bool {
    stamp.len() == 12
        && stamp
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// G0: what breaks it.
#[must_use]
pub fn gate0(h: &Header, runs: &[Run], done: bool) -> Vec<String> {
    let mut out = Vec::new();
    if !clean_commit(&h.runner_commit) {
        out.push(format!(
            "the runner's build {} names no clean commit",
            h.runner_commit
        ));
    }
    let summaries: Vec<(&Run, &Summary)> = runs.iter().filter_map(|r| Some((r, r.s()?))).collect();
    if summaries.is_empty() && done {
        out.push("no run printed a build stamp".to_owned());
    }
    let mut seen = |what: String| {
        if !out.contains(&what) {
            out.push(what);
        }
    };
    for (r, s) in summaries {
        let commit = s.raw("build_commit").unwrap_or("");
        if commit != h.runner_commit {
            seen(format!(
                "the harness's build {commit} ({}) is not the runner's {}",
                r.id, h.runner_commit
            ));
        }
        let profile = s.raw("build_profile").unwrap_or("");
        if profile != RELEASE_PROFILE {
            seen(format!(
                "the harness's profile is `{profile}`, not `{RELEASE_PROFILE}`"
            ));
        }
        if s.raw("debug_assertions") != Some("false") {
            seen("the harness has debug_assertions on".to_owned());
        }
    }
    match h.harness_unchanged {
        Some(true) => {}
        Some(false) => out.push("the harness changed during the session".to_owned()),
        None if done => out.push("the harness was not read again at the end".to_owned()),
        None => {}
    }
    out
}

/// G3's session-wide checks: one value of each identity across the runs.
#[must_use]
pub fn gate3_session(runs: &[Run]) -> Vec<String> {
    type Read = fn(&Run) -> Option<String>;
    let reads: [(&str, Read); 7] = [
        ("hw.model", |r| r.s()?.get("hw_model").map(str::to_owned)),
        ("the child's main-thread QoS", |r| {
            r.s()?.get("qos").map(str::to_owned)
        }),
        ("kern.osversion", |r| {
            r.s()?.get("kern_osversion").map(str::to_owned)
        }),
        ("the timebase", |r| {
            r.s()?.get("timebase").map(str::to_owned)
        }),
        ("the device's registryID", |r| {
            r.s()?.get("device_registry_id").map(str::to_owned)
        }),
        ("the driver's bundle", |r| {
            r.cond.driver.as_ref()?.bundle.clone()
        }),
        ("the driver's version", |r| {
            r.cond.driver.as_ref()?.version.clone()
        }),
    ];
    let mut out = Vec::new();
    for (what, read) in reads {
        let mut values: Vec<(String, &str)> = Vec::new();
        for r in runs {
            if let Some(v) = read(r)
                && !values.iter().any(|(seen, _)| *seen == v)
            {
                values.push((v, &r.id));
            }
        }
        if values.len() > 1 {
            let list: Vec<String> = values.iter().map(|(v, id)| format!("{v} ({id})")).collect();
            out.push(format!(
                "{what} differs across the session: {}",
                list.join(", ")
            ));
        }
    }
    let driver_read = runs
        .iter()
        .any(|r| r.cond.driver.as_ref().is_some_and(|d| d.version.is_some()));
    if !runs.is_empty() && !driver_read {
        out.push("the driver's version was never read".to_owned());
    }
    out
}

/// Where a session ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every gate held.
    Valid,
    /// These gates did not.
    Void(Vec<String>),
    /// A defect stopped it.
    Stopped(String),
}

/// The session's gates over its runs so far.
#[derive(Clone, Debug, PartialEq)]
pub struct Eval {
    /// G0's failures.
    pub gate0: Vec<String>,
    /// G1's failures, `id: why`.
    pub gate1: Vec<String>,
    /// G3's session-wide failures.
    pub gate3: Vec<String>,
    /// Runs voided by G3 or G5: id, gate, why.
    pub out: Vec<(String, Gate, String)>,
    /// The defect that stopped the session.
    pub defect: Option<String>,
    /// Rounds with all four runs.
    pub complete: Vec<usize>,
    /// Rounds with all four runs valid.
    pub valid_rounds: Vec<usize>,
    /// F's B371 end-to-end p50 over the valid rounds, ns.
    pub f_b371: Vec<f64>,
    /// Their span.
    pub f_span: Option<f64>,
    /// G4's failures.
    pub gate4: Vec<String>,
    /// G6's requirement.
    pub required: usize,
    /// G6's failures.
    pub gate6: Vec<String>,
    /// The outcome.
    pub outcome: Outcome,
}

/// The session's gates G0–G6 over its runs so far.
#[must_use]
pub fn evaluate(h: &Header, runs: &[Run], done: bool) -> Eval {
    let gate0 = gate0(h, runs, done);
    let defect = runs.iter().find_map(|r| match &r.verdict {
        Verdict::Defect(d) => Some(format!("{}: {d}", r.id)),
        _ => None,
    });
    let mut gate1 = Vec::new();
    let mut out = Vec::new();
    for r in runs {
        if let Verdict::Invalid(v) = &r.verdict {
            for (g, why) in v {
                if *g == Gate::G1 {
                    gate1.push(format!("{}: {why}", r.id));
                } else {
                    out.push((r.id.clone(), *g, why.clone()));
                }
            }
        }
    }
    let gate3 = gate3_session(runs);
    let rounds: Vec<usize> = (1..=h.rounds).collect();
    let in_round = |n: usize| runs.iter().filter(move |r| r.round == n);
    let complete: Vec<usize> = rounds
        .iter()
        .copied()
        .filter(|n| in_round(*n).count() == Arm::ALL.len())
        .collect();
    let valid_rounds: Vec<usize> = complete
        .iter()
        .copied()
        .filter(|n| in_round(*n).all(|r| r.verdict == Verdict::Valid))
        .collect();
    let f_b371: Vec<f64> = valid_rounds
        .iter()
        .filter_map(|n| {
            in_round(*n)
                .find(|r| r.arm == Arm::F)
                .and_then(|r| r.p50("b371", Figure::E2e))
        })
        .collect();
    let f_span = span_ratio(&f_b371);
    let mut gate4 = Vec::new();
    match f_span {
        None if done => gate4.push("no valid round, so F's B371 span is not measured".to_owned()),
        Some(x) if x > F_SPAN_MAX => gate4.push(format!(
            "F's warm B371 median spans {x:.3}x across valid rounds, over {F_SPAN_MAX}x"
        )),
        _ => {}
    }
    let required = required_valid(h.rounds);
    let mut gate6 = Vec::new();
    if done && valid_rounds.len() < required {
        gate6.push(format!(
            "{} of {} rounds valid, under {required}",
            valid_rounds.len(),
            h.rounds
        ));
    }
    let outcome = if let Some(d) = &defect {
        Outcome::Stopped(d.clone())
    } else {
        let mut why = Vec::new();
        for (g, v) in [
            (Gate::G0, &gate0),
            (Gate::G1, &gate1),
            (Gate::G3, &gate3),
            (Gate::G4, &gate4),
            (Gate::G6, &gate6),
        ] {
            why.extend(v.iter().map(|w| format!("{}: {w}", g.name())));
        }
        if why.is_empty() {
            Outcome::Valid
        } else {
            Outcome::Void(why)
        }
    };
    Eval {
        gate0,
        gate1,
        gate3,
        out,
        defect,
        complete,
        valid_rounds,
        f_b371,
        f_span,
        gate4,
        required,
        gate6,
        outcome,
    }
}

/// Which time of a repetition a criterion reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Figure {
    /// Texts handed in to result rows readable.
    E2e,
    /// The packed batch to result rows readable.
    Packed,
}

impl Figure {
    /// The summary's key suffix.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Figure::E2e => "e2e_ns",
            Figure::Packed => "packed_ns",
        }
    }

    /// Its name in the record.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Figure::E2e => "end to end",
            Figure::Packed => "from the packed batch",
        }
    }
}

/// One criterion, judged in every valid round.
#[derive(Clone, Debug, PartialEq)]
pub struct Criterion {
    /// C1…C6.
    pub name: &'static str,
    /// The inequality, in words.
    pub text: &'static str,
    /// Which time.
    pub figure: Figure,
    /// A sanity check, not a claim.
    pub sanity: bool,
    /// Per valid round: the round, G's figure and the other's, ns.
    pub rounds: Vec<(usize, Option<f64>, Option<f64>)>,
    /// G's figure must be below the other's (else above).
    pub below: bool,
}

impl Criterion {
    /// Whether round `i`'s inequality holds.
    #[must_use]
    pub fn holds_in(&self, i: usize) -> Option<bool> {
        let (_, g, x) = self.rounds.get(i)?;
        let (g, x) = ((*g)?, (*x)?);
        Some(if self.below { g < x } else { g > x })
    }

    /// Shown: it holds in every valid round (and there is one).
    #[must_use]
    pub fn shown(&self) -> bool {
        !self.rounds.is_empty() && (0..self.rounds.len()).all(|i| self.holds_in(i) == Some(true))
    }
}

/// C1–C6 over the valid rounds.
#[must_use]
pub fn criteria(runs: &[Run], valid_rounds: &[usize]) -> Vec<Criterion> {
    let of = |n: usize, arm: Arm| runs.iter().find(|r| r.round == n && r.arm == arm);
    let pairs =
        |series: &str, other: Arm, figure: Figure| -> Vec<(usize, Option<f64>, Option<f64>)> {
            valid_rounds
                .iter()
                .map(|n| {
                    (
                        *n,
                        of(*n, Arm::G).and_then(|r| r.p50(series, figure)),
                        of(*n, other).and_then(|r| r.p50(series, figure)),
                    )
                })
                .collect()
        };
    let mut out = Vec::new();
    for figure in [Figure::E2e, Figure::Packed] {
        for (name, text, series, other) in [
            (
                "C1",
                "at B16384, G's warm median is below I's",
                "b16384",
                Arm::I,
            ),
            ("C2", "at B16384, G's is below T's", "b16384", Arm::T),
            ("C5", "at B371, G's is below T's", "b371", Arm::T),
            ("C6", "at B371, G's is below I's", "b371", Arm::I),
        ] {
            out.push(Criterion {
                name,
                text,
                figure,
                sanity: false,
                rounds: pairs(series, other, figure),
                below: true,
            });
        }
    }
    out.push(Criterion {
        name: "C3",
        text: "at isolated B1, G's median is above I's: the GPU loses a single query",
        figure: Figure::E2e,
        sanity: true,
        rounds: pairs("b1iso", Arm::I, Figure::E2e),
        below: false,
    });
    out.push(Criterion {
        name: "C4",
        text: "G's cold-cache start exceeds F's warm B371 median",
        figure: Figure::E2e,
        sanity: true,
        rounds: valid_rounds
            .iter()
            .map(|n| {
                (
                    *n,
                    of(*n, Arm::G)
                        .and_then(|r| r.s()?.int("cold.start_cold_ns"))
                        .map(|v| v as f64),
                    of(*n, Arm::F).and_then(|r| r.p50("b371", Figure::E2e)),
                )
            })
            .collect(),
        below: false,
    });
    out
}

/// The runs of `arm` in the valid rounds.
fn valid_runs<'a>(runs: &'a [Run], valid: &[usize], arm: Arm) -> Vec<&'a Run> {
    runs.iter()
        .filter(|r| r.arm == arm && valid.contains(&r.round))
        .collect()
}

/// A value's words as numbers, `-` and other words as `None`.
fn words(v: &str) -> Vec<Option<f64>> {
    v.split_whitespace()
        .map(|w| w.parse::<u64>().ok().map(|n| n as f64))
        .collect()
}

/// The median of each word position of `key` over `rs`.
fn medians_of(rs: &[&Run], key: &str) -> Vec<Option<f64>> {
    let rows: Vec<Vec<Option<f64>>> = rs
        .iter()
        .filter_map(|r| r.s()?.raw(key).map(words))
        .collect();
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    (0..width)
        .map(|i| {
            let xs: Vec<f64> = rows
                .iter()
                .filter_map(|w| w.get(i).copied().flatten())
                .collect();
            median(&xs)
        })
        .collect()
}

/// Figures the runner derives from one run: name and value.
#[must_use]
pub fn derived(r: &Run, tb: (u32, u32)) -> Vec<(String, Option<f64>)> {
    let f = |v: Option<u64>| v.map(|v| v as f64);
    let mut out = vec![
        ("process creation, ns".to_owned(), f(r.creation_ns(tb))),
        ("exit, ns".to_owned(), f(r.exit_ns(tb))),
        ("spawn to exit, ns".to_owned(), f(r.wall_ns(tb))),
        ("RSS (ru_maxrss), bytes".to_owned(), f(r.m.maxrss)),
        ("CPU (wait4 user + system), ns".to_owned(), f(r.m.cpu_ns)),
    ];
    for (name, rows) in SERIES {
        let s = r.s();
        let reps = s.and_then(|s| s.int(&format!("{name}.reps")));
        let accepted = reps.map(|n| (n * rows) as f64);
        let e2e = s.and_then(|s| s.int(&format!("{name}.e2e_sum_ns")));
        let cpu = s
            .and_then(|s| s.ints(&format!("{name}.cpu_ns")))
            .map(|v| v.iter().sum::<u64>());
        let per = |rows: Option<f64>, ns: Option<u64>| {
            rows.zip(ns.filter(|n| *n > 0))
                .map(|(r, ns)| r / (ns as f64 / 1e9))
        };
        out.push((format!("{name} rows/s"), per(accepted, e2e)));
        out.push((format!("{name} rows per CPU-second"), per(accepted, cpu)));
    }
    out
}

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "-".to_owned(), |v| v.to_string())
}

fn num(v: Option<f64>) -> String {
    match v {
        None => "-".to_owned(),
        Some(x) if x.fract() == 0.0 && x.abs() < 1e15 => format!("{x:.0}"),
        Some(x) => format!("{x:.1}"),
    }
}

fn us(v: Option<f64>) -> String {
    v.map_or_else(|| "-".to_owned(), |ns| format!("{:.1}", ns / 1e3))
}

fn mib(v: Option<f64>) -> String {
    v.map_or_else(|| "-".to_owned(), |b| format!("{:.1}", b / MIB))
}

fn yn(b: Option<bool>) -> &'static str {
    match b {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unread",
    }
}

/// Where the runner is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Runs remain.
    Running,
    /// Every run is made, or a defect stopped them.
    Done,
    /// A signal cut the session short.
    Interrupted(i32),
}

fn list(o: &mut String, title: &str, v: &[String]) {
    if v.is_empty() {
        let _ = writeln!(o, "- **{title}:** pass");
    } else {
        let _ = writeln!(o, "- **{title}:** FAIL");
        for w in v {
            let _ = writeln!(o, "  - {w}");
        }
    }
}

fn header_section(o: &mut String, h: &Header, runs: &[Run], state: State) {
    let first = runs.iter().find_map(Run::s);
    let fact = |k: &str| first.and_then(|s| s.get(k)).unwrap_or("-").to_owned();
    let device = runs
        .iter()
        .filter_map(Run::s)
        .find_map(|s| s.get("device_name").map(str::to_owned))
        .unwrap_or_else(|| "-".to_owned());
    let driver = runs
        .iter()
        .find_map(|r| r.cond.driver.clone())
        .unwrap_or_default();
    let _ = writeln!(
        o,
        "# Aos M5a: shared-buffer inference, design §12.9's session\n"
    );
    if h.smoke() {
        let _ = writeln!(
            o,
            "**SMOKE (--rounds {}): not the registered session ({REGISTERED_ROUNDS} rounds); no claim is made from it.**\n",
            h.rounds
        );
    }
    let state = match state {
        State::Running => "running".to_owned(),
        State::Done => "done".to_owned(),
        State::Interrupted(n) => format!("interrupted by signal {n}"),
    };
    let _ = writeln!(
        o,
        "- state: {state}; session `{}`; {} rounds",
        h.session, h.rounds
    );
    let _ = writeln!(
        o,
        "- commit: runner `{}` ({}), harness `{}` ({}, debug_assertions={})",
        h.runner_commit,
        h.runner_profile,
        fact("build_commit"),
        fact("build_profile"),
        fact("debug_assertions")
    );
    let _ = writeln!(
        o,
        "- harness: `{}`, digest `{}` (domain aterm-aos-m5a-harness-v1), unchanged at the end: {}",
        h.harness,
        h.harness_digest,
        yn(h.harness_unchanged)
    );
    let _ = writeln!(o, "- toolchain: {}", fact("build_toolchain"));
    let qos = fact("qos");
    let _ = writeln!(
        o,
        "- QoS: the child's main thread {qos} ({}), which G3 requires across the session; each run's is in the runs table",
        qos_name(qos_class(&qos))
    );
    let _ = writeln!(
        o,
        "- machine: {} {}, kern.osversion {}, {} threads (T's pool), timebase {}/{}",
        fact("hw_model"),
        device,
        fact("kern_osversion"),
        fact("parallelism"),
        h.timebase.0,
        h.timebase.1
    );
    let _ = writeln!(
        o,
        "- GPU driver: {} {}",
        driver.bundle.as_deref().unwrap_or("unavailable"),
        driver.version.as_deref().unwrap_or("unavailable")
    );
    let order: Vec<String> = schedule(h.rounds.min(Arm::ALL.len()))
        .iter()
        .map(|o| o.iter().map(|a| a.name()).collect())
        .collect();
    let _ = writeln!(
        o,
        "- order: {} (round r runs order r mod 4)\n",
        order.join(", ")
    );
}

/// What C0 compared in a run that stopped before its last series: the repetitions its summary holds.
fn partial_text(r: &Run) -> String {
    let how = if r.m.deadline {
        format!("killed at the {} s deadline", DEADLINE.as_secs())
    } else {
        format!("exit {}", exit_text(&r.m))
    };
    let Some(s) = r.s() else {
        return format!(
            "{} ({how}) stopped part way: no summary, so the repetitions it compared are unread",
            r.id
        );
    };
    let done: Vec<(&str, u64)> = SERIES
        .iter()
        .filter_map(|(name, _)| Some((*name, s.int(&format!("{name}.reps"))?)))
        .collect();
    let names: Vec<&str> = done.iter().map(|(n, _)| *n).collect();
    format!(
        "{} ({how}) stopped part way: {} repetitions compared in the series it completed ({}){}",
        r.id,
        done.iter().map(|(_, n)| n).sum::<u64>(),
        if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        },
        if done.len() < SERIES.len() {
            ", and any of a series it stopped in, which its summary does not hold"
        } else {
            ""
        }
    )
}

fn gates_section(o: &mut String, ev: &Eval, runs: &[Run]) {
    let _ = writeln!(o, "## Gates\n");
    let mut g0 = ev.gate0.clone();
    g0.push(
        "not checked here: the doctest stage (`targo --unverified test -p aterm-gpu --features compute --doc`: 9 compile_fail and 1 no_run, each ok), verify's required-features pass and the harness's unit tests (`targo --unverified test -p aterm-bench --features aos-gpu --example aos_gpu`, where none run is a failure) are green on the commit (the operator's check)"
            .to_owned(),
    );
    let _ = writeln!(
        o,
        "- **G0, the binary and the commit:** {}",
        if ev.gate0.is_empty() { "pass" } else { "FAIL" }
    );
    for w in &g0 {
        let _ = writeln!(o, "  - {w}");
    }
    list(o, "G1, the base reproduces", &ev.gate1);
    if let Some(d) = &ev.defect {
        let _ = writeln!(o, "- **G2, C0 parity on every repetition:** DEFECT {d}");
    } else {
        let (whole, partial): (Vec<&Run>, Vec<&Run>) = runs
            .iter()
            .partition(|r| r.m.exit == Some(0) && !r.m.deadline);
        let _ = writeln!(
            o,
            "- **G2, C0 parity on every repetition:** no mismatch in the {} runs that completed every series",
            whole.len()
        );
        for r in partial {
            let _ = writeln!(o, "  - {}", partial_text(r));
        }
    }
    let mut g3 = ev.gate3.clone();
    g3.extend(
        ev.out
            .iter()
            .filter(|(_, g, _)| *g == Gate::G3)
            .map(|(id, _, w)| format!("{id} void: {w}")),
    );
    list(o, "G3, the GPU behaves", &g3);
    let span = ev
        .f_span
        .map_or_else(|| "unmeasured".to_owned(), |x| format!("{x:.3}x"));
    let _ = writeln!(
        o,
        "- **G4, the base is stable:** {} (F's B371 p50 over {} valid rounds spans {span}; bound {F_SPAN_MAX}x)",
        if ev.gate4.is_empty() { "pass" } else { "FAIL" },
        ev.f_b371.len()
    );
    for w in &ev.gate4 {
        let _ = writeln!(o, "  - {w}");
    }
    let g5: Vec<String> = ev
        .out
        .iter()
        .filter(|(_, g, _)| *g == Gate::G5)
        .map(|(id, _, w)| format!("{id} void: {w}"))
        .collect();
    list(o, "G5, budgets", &g5);
    let rounds: Vec<String> = ev.valid_rounds.iter().map(ToString::to_string).collect();
    let _ = writeln!(
        o,
        "- **G6, enough data:** {} ({} of {} complete rounds valid: [{}]; {} required)",
        if ev.gate6.is_empty() { "pass" } else { "FAIL" },
        ev.valid_rounds.len(),
        ev.complete.len(),
        rounds.join(", "),
        ev.required
    );
    for w in &ev.gate6 {
        let _ = writeln!(o, "  - {w}");
    }
    let outcome = match &ev.outcome {
        Outcome::Valid => "VALID".to_owned(),
        Outcome::Void(_) => "VOID".to_owned(),
        Outcome::Stopped(d) => format!("STOPPED by a defect ({d})"),
    };
    let _ = writeln!(o, "\n**Session: {outcome}**\n");
}

/// Why the record claims no criterion, or `None` for a valid, finished registered session.
#[must_use]
pub fn no_claim(smoke: bool, outcome: &Outcome, state: State) -> Option<&'static str> {
    if smoke {
        return Some("smoke");
    }
    match (state, outcome) {
        (State::Running, _) => Some("session running"),
        (State::Interrupted(_), _) => Some("session interrupted"),
        (State::Done, Outcome::Void(_)) => Some("session VOID"),
        (State::Done, Outcome::Stopped(_)) => Some("session STOPPED"),
        (State::Done, Outcome::Valid) => None,
    }
}

fn criteria_section(o: &mut String, cs: &[Criterion], no_claim: Option<&str>) {
    let _ = writeln!(o, "## Criteria\n");
    let _ = writeln!(
        o,
        "Each is claimed only if it holds in every valid round of a valid, finished session (G's run p50 against the other arm's, µs). {}\n",
        match no_claim {
            None => "A criterion that fails is not shown.".to_owned(),
            Some("smoke") => "This is a smoke run: nothing is claimed.".to_owned(),
            Some(why) => format!("No claim ({why}): the rounds' marks are reported only."),
        }
    );
    let _ = writeln!(
        o,
        "| criterion | figure | rounds (G vs other, µs) | result |"
    );
    let _ = writeln!(o, "|---|---|---|---|");
    for c in cs {
        let rounds: Vec<String> = c
            .rounds
            .iter()
            .enumerate()
            .map(|(i, (n, g, x))| {
                let mark = match c.holds_in(i) {
                    Some(true) => "holds",
                    Some(false) => "fails",
                    None => "unread",
                };
                format!("r{n}: {} vs {} {mark}", us(*g), us(*x))
            })
            .collect();
        let result = match (c.shown(), c.sanity, no_claim) {
            (shown, _, Some(why)) => format!(
                "{} ({why}, no claim)",
                if shown { "holds" } else { "fails" }
            ),
            (true, true, None) => "holds (sanity)".to_owned(),
            (false, true, None) => "FAILS (sanity)".to_owned(),
            (true, false, None) => "SHOWN".to_owned(),
            (false, false, None) => "not shown".to_owned(),
        };
        let _ = writeln!(
            o,
            "| {}: {} | {} | {} | {result} |",
            c.name,
            c.text,
            c.figure.name(),
            if rounds.is_empty() {
                "no valid round".to_owned()
            } else {
                rounds.join("; ")
            }
        );
    }
    let _ = writeln!(o);
}

fn registered_section(o: &mut String, runs: &[Run], valid: &[usize]) {
    let _ = writeln!(o, "## The registered figure: resident memory\n");
    let _ = writeln!(
        o,
        "Per arm, the median over valid rounds of the child's `ri_lifetime_max_phys_footprint`, MiB.\n"
    );
    let _ = writeln!(o, "| arm | median | rounds | per round |");
    let _ = writeln!(o, "|---|---|---|---|");
    let mut med = BTreeMap::new();
    for arm in Arm::ALL {
        let rs = valid_runs(runs, valid, arm);
        let xs: Vec<f64> = rs
            .iter()
            .filter_map(|r| r.s()?.int("lifetime_max_footprint"))
            .map(|v| v as f64)
            .collect();
        let m = median(&xs);
        med.insert(arm, m);
        let per: Vec<String> = rs
            .iter()
            .map(|r| {
                format!(
                    "r{} {}",
                    r.round,
                    mib(r
                        .s()
                        .and_then(|s| s.int("lifetime_max_footprint"))
                        .map(|v| v as f64))
                )
            })
            .collect();
        let _ = writeln!(
            o,
            "| {} | {} | {} | {} |",
            arm.name(),
            mib(m),
            xs.len(),
            per.join(", ")
        );
    }
    let gi = med
        .get(&Arm::G)
        .copied()
        .flatten()
        .zip(med.get(&Arm::I).copied().flatten())
        .map(|(g, i)| g - i);
    let _ = writeln!(
        o,
        "\n**G − I (the medians' difference): {} MiB**\n",
        mib(gi)
    );
}

fn series_section(o: &mut String, runs: &[Run], valid: &[usize], tb: (u32, u32)) {
    let _ = writeln!(o, "## Warm series: medians over valid rounds\n");
    let _ = writeln!(
        o,
        "Each cell is the median over valid rounds of the runs' p50/p95/p99, µs; rows/s and rows per CPU-second are medians of each run's figure.\n"
    );
    let _ = writeln!(
        o,
        "| series | arm | end to end p50/p95/p99 | from packed p50/p95/p99 | rows/s | rows per CPU-s |"
    );
    let _ = writeln!(o, "|---|---|---|---|---|---|");
    for (name, _) in SERIES {
        for arm in Arm::ALL {
            let rs = valid_runs(runs, valid, arm);
            let three = |key: &str| {
                let m = medians_of(&rs, key);
                if m.is_empty() {
                    "-".to_owned()
                } else {
                    m.iter().map(|v| us(*v)).collect::<Vec<_>>().join(" / ")
                }
            };
            let derived_median = |label: &str| {
                let xs: Vec<f64> = rs
                    .iter()
                    .filter_map(|r| derived(r, tb).into_iter().find(|(k, _)| k == label)?.1)
                    .collect();
                median(&xs).map_or_else(|| "-".to_owned(), |v| format!("{v:.0}"))
            };
            let _ = writeln!(
                o,
                "| {name} | {} | {} | {} | {} | {} |",
                arm.name(),
                three(&format!("{name}.e2e_ns")),
                three(&format!("{name}.packed_ns")),
                derived_median(&format!("{name} rows/s")),
                derived_median(&format!("{name} rows per CPU-second")),
            );
        }
    }
    let _ = writeln!(
        o,
        "\n### G's split (p50, µs; invalid splits per part summed over valid rounds)\n"
    );
    let mut head = String::from("| series |");
    let mut rule = String::from("|---|");
    for p in PARTS {
        let _ = write!(head, " {p} |");
        rule.push_str("---|");
    }
    let _ = writeln!(o, "{head} invalid |\n{rule}---|");
    let rs = valid_runs(runs, valid, Arm::G);
    for (name, _) in SERIES {
        let mut row = format!("| {name} |");
        for p in PARTS {
            let m = medians_of(&rs, &format!("{name}.part.{p}_ns"));
            let _ = write!(row, " {} |", us(m.first().copied().flatten()));
        }
        let invalid: Vec<u64> = (0..PARTS.len())
            .map(|i| {
                rs.iter()
                    .filter_map(|r| r.s()?.nth(&format!("{name}.part_invalid"), i))
                    .sum()
            })
            .collect();
        let _ = writeln!(
            o,
            "{row} {} |",
            invalid
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    let _ = writeln!(o);
}

fn memory_section(o: &mut String, runs: &[Run], valid: &[usize]) {
    let _ = writeln!(o, "## Memory at P0–P7: medians over valid rounds, MiB\n");
    let _ = writeln!(
        o,
        "Each cell is {} (`-`: not this arm's).\n",
        POINT_FIGURES.join(" / ")
    );
    let _ = writeln!(o, "| point | F | I | T | G |");
    let _ = writeln!(o, "|---|---|---|---|---|");
    for p in POINTS {
        let mut row = format!("| {p} |");
        for arm in Arm::ALL {
            let rs = valid_runs(runs, valid, arm);
            let m = medians_of(&rs, &format!("mem.{p}"));
            let cell = if m.iter().all(Option::is_none) {
                "-".to_owned()
            } else {
                m.iter().map(|v| mib(*v)).collect::<Vec<_>>().join(" / ")
            };
            let _ = write!(row, " {cell} |");
        }
        let _ = writeln!(o, "{row}");
    }
    let _ = writeln!(o);
}

fn every_figure_section(o: &mut String, runs: &[Run], valid: &[usize], tb: (u32, u32)) {
    let _ = writeln!(o, "## Every figure: medians over valid rounds\n");
    let _ = writeln!(
        o,
        "As the summary prints it (ns, bytes, counts), word by word; then the runner's own figures.\n"
    );
    let _ = writeln!(o, "| figure | F | I | T | G |");
    let _ = writeln!(o, "|---|---|---|---|---|");
    let keys: Vec<String> = runs
        .iter()
        .filter(|r| valid.contains(&r.round))
        .find_map(Run::s)
        .map(|s| s.keys().map(str::to_owned).collect())
        .unwrap_or_default();
    let by_arm: Vec<Vec<&Run>> = Arm::ALL
        .iter()
        .map(|a| valid_runs(runs, valid, *a))
        .collect();
    for k in keys.iter().filter(|k| !NOT_FIGURES.contains(&k.as_str())) {
        let cells: Vec<Vec<Option<f64>>> = by_arm.iter().map(|rs| medians_of(rs, k)).collect();
        if cells.iter().all(|c| c.iter().all(Option::is_none)) {
            continue;
        }
        let text: Vec<String> = cells
            .iter()
            .map(|c| {
                if c.iter().all(Option::is_none) {
                    "-".to_owned()
                } else {
                    c.iter().map(|v| num(*v)).collect::<Vec<_>>().join(" ")
                }
            })
            .collect();
        let _ = writeln!(o, "| {k} | {} |", text.join(" | "));
    }
    let labels: Vec<String> = runs
        .first()
        .map(|r| derived(r, tb).into_iter().map(|(k, _)| k).collect())
        .unwrap_or_default();
    for label in labels {
        let cells: Vec<String> = by_arm
            .iter()
            .map(|rs| {
                let xs: Vec<f64> = rs
                    .iter()
                    .filter_map(|r| derived(r, tb).into_iter().find(|(k, _)| *k == label)?.1)
                    .collect();
                num(median(&xs).map(f64::round))
            })
            .collect();
        let _ = writeln!(o, "| {label} | {} |", cells.join(" | "));
    }
    let _ = writeln!(o);
}

fn verdict_text(v: &Verdict) -> String {
    match v {
        Verdict::Valid => "valid".to_owned(),
        Verdict::Invalid(w) => format!(
            "void: {}",
            w.iter()
                .map(|(g, w)| format!("{} {w}", g.name()))
                .collect::<Vec<_>>()
                .join("; ")
        ),
        Verdict::Defect(d) => format!("DEFECT: {d}"),
    }
}

fn runs_section(o: &mut String, runs: &[Run], tb: (u32, u32)) {
    let _ = writeln!(o, "## Runs and their conditions\n");
    let _ = writeln!(
        o,
        "GPU utilization is an instantaneous sample (device %), before / after; swap is vm.swapusage's used; creation and exit are µs; QoS is the child's main thread's class and relative priority (T: and its pool threads that took it); G's pipelines are the salted (cold-cache) and canonical (warm-cache) builds, µs; RSS is ru_maxrss, MiB. Each run's raw readings are in runs/<run>.runner.\n"
    );
    let _ = writeln!(
        o,
        "| run | arm | exit | creation | exit | load1 | swap used | low power | AC | thermal | QoS | GPU % | recoveries | G pipelines | RSS | lifetime max | b371 p50 µs | b16384 p50 µs | verdict |"
    );
    let _ = writeln!(
        o,
        "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"
    );
    for r in runs {
        let s = r.s();
        let gpu = |g: &Option<GpuReading>| opt(g.as_ref().and_then(|g| g.device_pct));
        let rec = |g: &Option<GpuReading>| opt(g.as_ref().and_then(|g| g.recoveries));
        let thermal = format!(
            "{} -> {}",
            s.and_then(|s| s.get("thermal_start")).unwrap_or("-"),
            s.and_then(|s| s.get("thermal_end")).unwrap_or("-")
        );
        let mut qos = s.and_then(|s| s.get("qos")).unwrap_or("-").to_owned();
        if r.arm == Arm::T {
            let _ = write!(
                qos,
                " (pool {} of {})",
                opt(s.and_then(|s| s.int("pool_qos_set"))),
                opt(s.and_then(|s| s.int("pool_qos_wanted")))
            );
        }
        let pipelines = if r.arm == Arm::G {
            let ns = |k: &str| us(s.and_then(|s| s.int(k)).map(|v| v as f64));
            format!(
                "{} / {}",
                ns("cold.pipeline_salted_ns"),
                ns("cold.pipeline_ns")
            )
        } else {
            "-".to_owned()
        };
        let _ = writeln!(
            o,
            "| {} | {} | {}{} | {} | {} | {} | {} | {} | {} | {thermal} | {qos} | {} / {} | {} / {} | {pipelines} | {} | {} | {} | {} | {} |",
            r.id,
            r.arm.name(),
            exit_text(&r.m),
            if r.m.deadline { " (deadline)" } else { "" },
            us(r.creation_ns(tb).map(|v| v as f64)),
            us(r.exit_ns(tb).map(|v| v as f64)),
            r.cond
                .load1
                .map_or_else(|| "-".to_owned(), |v| format!("{v:.2}")),
            r.cond
                .swap
                .as_deref()
                .and_then(parse_swap_used)
                .map_or_else(|| "-".to_owned(), |v| format!("{v:.1}M")),
            yn(r.cond.lowpower()),
            yn(r.cond.ac()),
            gpu(&r.cond.gpu_before),
            gpu(&r.cond.gpu_after),
            rec(&r.cond.gpu_before),
            rec(&r.cond.gpu_after),
            mib(r.m.maxrss.map(|v| v as f64)),
            mib(s
                .and_then(|s| s.int("lifetime_max_footprint"))
                .map(|v| v as f64)),
            us(r.p50("b371", Figure::E2e)),
            us(r.p50("b16384", Figure::E2e)),
            verdict_text(&r.verdict)
        );
    }
    let notes: Vec<String> = runs
        .iter()
        .flat_map(|r| r.cond.notes.iter().map(move |n| format!("{}: {n}", r.id)))
        .collect();
    if !notes.is_empty() {
        let _ = writeln!(o, "\nUnread conditions:\n");
        for n in notes {
            let _ = writeln!(o, "- {n}");
        }
    }
    let _ = writeln!(o);
}

/// The choices §12.9 left to the runner, and what the record cannot show.
pub const DISCLOSURES: &[&str] = &[
    "Exit is measured at the return of a blocking `waitid(WEXITED|WNOWAIT)`, before `wait4` reaps the child for its rusage, so the deadline thread never signals a reaped pid; §12.9 names `wait4`'s return. Darwin's `waitid` also returns a stop or continue report there: the runner consumes it (`WSTOPPED|WCONTINUED|WNOHANG`, which cannot reap) and waits on, so a stopped run is still killed at the deadline.",
    "A nonzero harness exit is a defect, except a kill at the 120 s deadline (G5, the run void) and exit 3, which the harness gives only for a G3 failure (a job not Completed or a qualification refused) with its summary written (G3, the run void).",
    "G1 is split from G2 thus: the run's own reference (F's classify_opt of the 371 texts, or checked forward of B371) is compared with each repetition (a mismatch is a C0 defect inside the harness, exit 1); F's reference against F_ARGMAX_371, the texts' digest and tone-int v1's id against their pins are G1 (the session void); the B371 reference's digest against its pin is a C0 defect, as G2 says the reference equals the pin.",
    "G3's equality across the session also covers hw.model, the timebase, the driver's bundle id and the child's main-thread QoS (which steers the cores every CPU arm and T's pool run on); equality across sessions reported together is the operator's check. A G run's recovery counter unread before or after voids it.",
    "G3 also voids a G run whose canonical pipeline took at least 2 ms: its warm-cache start missed the per-user Metal cache, so it would be a cold start under the warm-cache figure's name.",
    "G5 also voids a T run whose pool threads did not all take the main thread's QoS, when that QoS is a class the pool can apply (user-interactive, user-initiated, default, utility or background); under an unspecified class the pool runs at the main thread's, which is the registered arm.",
    "Every reading the runner makes of a run is in runs/<run>.runner (aos-gpu-runner-v1): the spawn and exit stamps on the runner's timebase, the exit status, signal and deadline kill, wait4's CPU and ru_maxrss, any error, the load average, vm.swapusage, `pmset -g` and `pmset -g ps` verbatim, the driver's identity and both GPU readings, so every runner figure and condition gate here can be recomputed from it.",
    "G0's doctest stage (`targo --unverified test -p aterm-gpu --features compute --doc`: 9 compile_fail and 1 no_run, each ok), verify's required-features pass and the harness's unit tests (`targo --unverified test -p aterm-bench --features aos-gpu --example aos_gpu`, where none run is a failure) are the operator's check; the runner checks the commit (its own build stamp against the harness's, neither -dirty), the profile `release opt-level=3`, debug_assertions=false and the harness digest at start and end.",
    "G5's declared bytes are checked for G runs against the device limits the harness printed (params ≤ params limit, B16384's job ≤ job limit, the most live declared bytes ≤ domain limit); the CPU arms declare host buffers, which §12.5's limits do not cover.",
    "Medians over valid rounds are of each run's own p50, p95 and p99 (nearest rank, in the harness); rows/s divides the accepted rows by the series' summed end-to-end times (gaps and compares excluded), rows per CPU-second by the series' RUSAGE_SELF user + system delta.",
    "G − I is the difference of the two arms' medians.",
    "Run ids are `<session>.rNN.P`, the session tag from the start time and the runner's pid; each G run's library comment salt and RUN_SALT derive from its id, so no two runs of any session share a cache key.",
    "The runner reads the conditions after the 20 s idle and before the spawn, the GPU again after the exit; GPU utilization is an instantaneous sample.",
    "The harness's G figure from the packed batch includes the copy into the Shared buffers (`load`), which §12.9's split counts in pack. Every I, T and G repetition fills its output with the poison word 0xa5a5a5a5 between its packed stamp and its forward (G's inside `load`), so a row a forward leaves unwritten fails C0; the fill is in those arms' end-to-end and from-packed figures. F's classifier writes every slot and is not filled.",
    "§11's figures were debug builds; this harness is a release build (opt-level 3), which matters beside any comparison with them.",
    "RSS (ru_maxrss) undercounts GPU memory; MTLCompilerService's CPU and memory are outside the process and in no figure.",
    "No windowed run during the session is the operator's discipline; the owner's windows share the GPU.",
    "The held-out set is read as workload by every arm; no selection reads it.",
    "What it cannot show: one Mac (Apple M5, a 10-core GPU, unified memory); one small model and one batch distribution, with repetition; the workload never reaches step 5's clamp or step 6's saturation, so C0 covers the unclamped path only; no contention with a renderer in the same process; debug-only faults and no real GPU hang; no persistent kernel, bytesNoCopy, shared event or second backend; the GPU's power state is not read.",
    "The biggest risk (§12.10) is that a job's fixed costs exceed the kernel at every batch the workload has; what C1–C6 show is the criteria table above. Whatever they show, its stated value is the tested substrate and its exposed costs, not a speedup.",
];

/// The record, and the verb's exit for it.
#[must_use]
pub fn render(h: &Header, runs: &[Run], state: State) -> (String, i32) {
    let done = state == State::Done;
    let ev = evaluate(h, runs, done);
    let mut o = String::new();
    header_section(&mut o, h, runs, state);
    gates_section(&mut o, &ev, runs);
    criteria_section(
        &mut o,
        &criteria(runs, &ev.valid_rounds),
        no_claim(h.smoke(), &ev.outcome, state),
    );
    registered_section(&mut o, runs, &ev.valid_rounds);
    series_section(&mut o, runs, &ev.valid_rounds, h.timebase);
    memory_section(&mut o, runs, &ev.valid_rounds);
    every_figure_section(&mut o, runs, &ev.valid_rounds, h.timebase);
    runs_section(&mut o, runs, h.timebase);
    let _ = writeln!(o, "## Disclosures\n");
    for d in DISCLOSURES {
        let _ = writeln!(o, "- {d}");
    }
    let (word, code) = match (&ev.outcome, state) {
        (_, State::Running) => ("running", 1),
        (_, State::Interrupted(n)) => ("interrupted", 128 + n),
        (Outcome::Valid, State::Done) => ("valid", 0),
        (Outcome::Void(_), State::Done) => ("void", 1),
        (Outcome::Stopped(_), State::Done) => ("defect", 2),
    };
    let _ = writeln!(
        o,
        "\naos-gpu-bench result={word} kind={} exit={code}",
        if h.smoke() { "smoke" } else { "registered" }
    );
    (o, code)
}

fn drain<R: std::io::Read + Send + 'static>(r: Option<R>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    if let Some(mut r) = r {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = r.read_to_end(&mut bytes);
            let _ = tx.send(String::from_utf8_lossy(&bytes).into_owned());
        });
    }
    rx
}

/// What ended the deadline thread's wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Watch {
    /// The run exited on its own.
    Exited,
    /// The deadline passed, and the run was killed.
    Deadline,
    /// An interrupt arrived, and the run was killed.
    Stopped(Stopped),
}

/// Waits until `done` is set or `deadline` passes, and kills `pid` with `SIGKILL` at the deadline or
/// on an interrupt (the M6 runner's watcher too, at its own deadline).
pub(crate) fn watch(pid: u32, deadline: Duration, done: &AtomicBool) -> Watch {
    let ended = stop::sleep_unless(deadline, done);
    let kill = |w: Watch| {
        if let Ok(pid) = i32::try_from(pid) {
            let _ = sys::kill(pid, libc::SIGKILL);
        }
        w
    };
    match ended {
        Ok(true) => Watch::Exited,
        Ok(false) => kill(Watch::Deadline),
        Err(s) => kill(Watch::Stopped(s)),
    }
}

/// One run of the harness: spawn, block until it exits (killed at the deadline), then reap it.
/// # Errors
/// An interrupt arrived (the run is killed and reaped first).
fn measure(harness: &Path, arm: Arm, id: &str, summary: &Path) -> Result<Measured, Stopped> {
    let mut cmd = aterm_uds::spawnfd::command(harness);
    cmd.args(["--arm", arm.name(), "--run-id", id, "--out"])
        .arg(summary)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let spawn_ticks = sys::mach_ticks();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Ok(Measured {
                spawn_ticks,
                error: Some(format!("cannot spawn {}: {e}", harness.display())),
                ..Measured::default()
            });
        }
    };
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let pid = child.id();
    let done = Arc::new(AtomicBool::new(false));
    let watcher = {
        let done = Arc::clone(&done);
        std::thread::spawn(move || watch(pid, DEADLINE, &done))
    };
    let exited = sys::await_exit(pid);
    let exit_ticks = sys::mach_ticks();
    done.store(true, Ordering::SeqCst);
    stop::ring();
    let watched = watcher.join().unwrap_or(Watch::Exited);
    if exited.is_err() {
        let _ = child.kill();
    }
    let waited = child.wait_with_rusage();
    let take =
        |rx: mpsc::Receiver<String>| rx.recv_timeout(Duration::from_secs(10)).unwrap_or_default();
    let (stdout, stderr) = (take(stdout), take(stderr));
    if let Watch::Stopped(s) = watched {
        return Err(s);
    }
    let mut m = Measured {
        spawn_ticks,
        exit_ticks: exited.is_ok().then_some(exit_ticks),
        deadline: watched == Watch::Deadline,
        stdout,
        stderr,
        ..Measured::default()
    };
    if let Err(e) = exited {
        m.error = Some(format!("waitid: {e}"));
    }
    match waited {
        Ok(u) => {
            use std::os::unix::process::ExitStatusExt as _;
            m.exit = u.status.code();
            m.signal = u.status.signal();
            m.cpu_ns = Some(u64::try_from(u.cpu.as_nanos()).unwrap_or(u64::MAX));
            m.maxrss = Some(u.maxrss);
        }
        Err(e) => {
            m.error.get_or_insert(format!("wait4: {e}"));
        }
    }
    Ok(m)
}

/// A tool's stdout, spawned through `spawnfd` with a 10 s deadline.
fn tool(program: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = aterm_uds::spawnfd::command(program);
    cmd.args(args);
    let out = aterm_uds::deadline::output_within(&mut cmd, Duration::from_secs(10))
        .map_err(|e| format!("{program}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{program} exited {}", out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The machine's conditions before a run.
fn conditions() -> Conditions {
    let mut c = Conditions::default();
    match load_average() {
        Ok(v) => c.load1 = Some(v),
        Err(e) => c.notes.push(format!("load average: {e}")),
    }
    match tool("/usr/sbin/sysctl", &["-n", "vm.swapusage"]) {
        Ok(t) => c.swap = Some(t),
        Err(e) => c.notes.push(format!("vm.swapusage: {e}")),
    }
    match tool("pmset", &["-g"]) {
        Ok(t) => c.pmset = Some(t),
        Err(e) => c.notes.push(format!("pmset -g: {e}")),
    }
    match tool("pmset", &["-g", "ps"]) {
        Ok(t) => c.pmset_ps = Some(t),
        Err(e) => c.notes.push(format!("pmset -g ps: {e}")),
    }
    c.driver = aterm_sysprobe::gpu_driver();
    if c.driver.is_none() {
        c.notes
            .push("the GPU driver's identity is unavailable".to_owned());
    }
    c.gpu_before = aterm_sysprobe::gpu();
    c
}

/// The regular file `given` names, made absolute and physical.
fn regular_file(given: &str, flag: &str) -> Result<PathBuf, Fail> {
    let path = std::fs::canonicalize(given)
        .map_err(|e| Fail::usage(format!("aos-gpu-bench: {flag} {given}: {e}")))?;
    if !path.is_file() {
        return Err(Fail::usage(format!(
            "aos-gpu-bench: {flag} {given}: not a regular file"
        )));
    }
    Ok(path)
}

struct Bench {
    harness: PathBuf,
    out: PathBuf,
    header: Header,
    runs: Vec<Run>,
}

impl Bench {
    fn write(&self, state: State) -> (String, i32) {
        let (text, code) = render(&self.header, &self.runs, state);
        let path = self.out.join("record.md");
        if let Err(e) = std::fs::write(&path, &text) {
            eprintln!("aos-gpu-bench: cannot write {}: {e}", path.display());
        }
        (text, code)
    }

    fn one(&mut self, round: usize, pos: usize, arm: Arm) -> Result<(), Stopped> {
        stop::sleep(IDLE)?;
        let mut cond = conditions();
        let id = format!("{}.r{round:02}.{pos}", self.header.session);
        let runs_dir = self.out.join("runs");
        let summary_path = runs_dir.join(format!("{id}.summary"));
        let m = measure(&self.harness, arm, &id, &summary_path)?;
        cond.gpu_after = aterm_sysprobe::gpu();
        let _ = std::fs::write(runs_dir.join(format!("{id}.stdout")), &m.stdout);
        let _ = std::fs::write(runs_dir.join(format!("{id}.stderr")), &m.stderr);
        let summary = std::fs::read_to_string(&summary_path)
            .map_err(|e| format!("cannot read {}: {e}", summary_path.display()))
            .and_then(|t| parse_summary(&t));
        let mut run = Run {
            id,
            round,
            pos,
            arm,
            cond,
            m,
            summary,
            verdict: Verdict::Valid,
        };
        run.verdict = judge(&run);
        let sidecar = runs_dir.join(format!("{}.runner", run.id));
        if let Err(e) = std::fs::write(&sidecar, runner_text(&run, self.header.timebase)) {
            eprintln!("aos-gpu-bench: cannot write {}: {e}", sidecar.display());
        }
        eprintln!(
            "aos-gpu-bench: {} {} b371 p50 {} µs, b16384 p50 {} µs, lifetime max {} MiB: {}",
            run.id,
            arm.name(),
            us(run.p50("b371", Figure::E2e)),
            us(run.p50("b16384", Figure::E2e)),
            mib(run
                .s()
                .and_then(|s| s.int("lifetime_max_footprint"))
                .map(|v| v as f64)),
            verdict_text(&run.verdict)
        );
        self.runs.push(run);
        let _ = self.write(State::Running);
        Ok(())
    }

    fn session(&mut self) -> Result<(), Stopped> {
        for (n, order) in schedule(self.header.rounds).iter().enumerate() {
            for (p, arm) in order.iter().enumerate() {
                self.one(n + 1, p + 1, *arm)?;
                if self
                    .runs
                    .last()
                    .is_some_and(|r| matches!(r.verdict, Verdict::Defect(_)))
                {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

/// `aos-gpu-bench`: the §12.9 session into `--out`.
pub fn cmd_aos_gpu_bench(args: &[String]) -> Res {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }
    let args = parse_args(args)?;
    let out = crate::trail_gallery::resolve_out(
        &from_caller(&args.out),
        &crate::trail_gallery::checkouts(&paths().repo),
    )
    .map_err(|e| Fail::usage(format!("aos-gpu-bench: --out {e}")))?;
    if out.exists() {
        let empty = std::fs::read_dir(&out)
            .map_err(|e| Fail::usage(format!("aos-gpu-bench: --out {}: {e}", out.display())))?
            .next()
            .is_none();
        if !empty {
            return Err(Fail::usage(format!(
                "aos-gpu-bench: --out {} is not empty: a record never mixes sessions",
                out.display()
            )));
        }
    }
    let harness = regular_file(&from_caller(&args.harness), "--harness")?;
    let digest = digest_file(&harness, HARNESS_DOMAIN)
        .map_err(|e| Fail::usage(format!("aos-gpu-bench: --harness {e}")))?;
    let timebase = sys::mach_timebase()
        .ok_or_else(|| Fail::die("aos-gpu-bench: mach_timebase_info failed"))?;
    std::fs::create_dir_all(out.join("runs")).map_err(|e| {
        Fail::die(format!(
            "aos-gpu-bench: cannot make {}: {e}",
            out.join("runs").display()
        ))
    })?;
    stop::install();
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let header = Header {
        rounds: args.rounds,
        session: format!("m5a-{started}-{}", std::process::id()),
        runner_commit: env!("ATERM_LAB_GIT_COMMIT").to_owned(),
        runner_profile: env!("ATERM_LAB_PROFILE").to_owned(),
        harness: harness.display().to_string(),
        harness_digest: digest.short(),
        harness_unchanged: None,
        timebase,
    };
    if header.smoke() {
        eprintln!(
            "aos-gpu-bench: SMOKE (--rounds {}): NOT the registered session ({REGISTERED_ROUNDS} rounds)",
            args.rounds
        );
    }
    let mut bench = Bench {
        harness: harness.clone(),
        out: out.clone(),
        header,
        runs: Vec::new(),
    };
    let ran = bench.session();
    bench.header.harness_unchanged =
        Some(digest_file(&harness, HARNESS_DOMAIN).is_ok_and(|d| d == digest));
    match ran {
        Err(stopped) => {
            let (text, _) = bench.write(State::Interrupted(stopped.0));
            print!("{text}");
            Err(Fail::from(stopped))
        }
        Ok(()) => {
            let (text, code) = bench.write(State::Done);
            print!("{text}");
            println!("aos-gpu-bench: wrote {}", out.join("record.md").display());
            Ok(code)
        }
    }
}

#[cfg(test)]
#[path = "aos_gpu_bench_tests.rs"]
mod tests;
