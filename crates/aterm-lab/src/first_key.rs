// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `first-key`: the first key a window takes after a quiet, arm against arm (ATERM-KI-1027-4,
//! `docs/measured/idle-memory-rest-2026-10-29.md`).
//!
//! One windowed private instance (110x32) of the aterm given runs every press. Each arm names the
//! window's `idle_rest_secs` and any `aterm.toml` lines of its own; the arms take turns press by
//! press, each round of them reversed after the last, so a drift in the Mac's load falls on every
//! arm alike.
//! `--fixed-order` instead repeats the given order (A B A B for two equal arms).
//! `--key-shape press|built` selects the existing `hwkey` event constructor explicitly; the
//! default is `press`. Before warm-up, the lane raises its private terminal and observes native
//! focus. These gates do not establish that a posted event arrived: booking still must follow.
//! A press waits out its quiet (the longer of `--min-quiet` and its arm's rest plus 1 s, so an arm
//! that rests longer than the state wait's minute is still measured rather than dropped; then
//! until `trail status` says the pet sleeps deeply and the window rests exactly when its arm rests
//! it; then a random `--settle` more, so the key meets the resting window's 5 s reading at any
//! phase), reads the load, the memory pressure and aterm's footprint, waits 2 s, resets the
//! metrics, presses `a` through the OS event path (`hwkey`), and reads `metrics` 600 ms later. A
//! burst arm presses four more keys once the first is booked, each a random 100 to 250 ms after
//! the one before, and books each of those four from its own booking as a row of its own (`burst`
//! 2 to 5), compared with the first burst arm's.
//! `key ctrl+u` then clears the shell's line, which starts the next quiet. A press whose window is
//! not in its arm's state a minute past its quiet (a pointer crossed it, the focus came back to
//! it, its tab printed) is dropped and counted, with its status in `<out>/dropped.tsv`.
//!
//! Every first-key press is a row of `<out>/presses.tsv` (a burst press four), written as it
//! ends, with its `trail status`, `metrics` and `metrics percentiles` replies whole in
//! `<out>/raw/<press>-<arm>.txt`. A row carries aterm's own counts of the press's slices booked in
//! a present 2 s or more after the process's previous present (`n_input_cold`) and in a window's
//! first present after a person woke it from a focused rest (`n_rest_wake_key`); a burst key's
//! row carries its own key's. The summary is `<out>/summary.txt`, and `first-key summarize
//! <presses.tsv>...` reads it again from rows: per arm, nearest-rank percentiles, the cold share
//! (rows whose `present_idle_ms` is 2000 or more) beside the summed `n_input_cold` and the rows
//! where the two disagree, the submit and the key's queue, the input and present percentiles of
//! its cold keys (rows whose `n_input_cold` is above 0), and its presses by memory pressure; then
//! each arm against its base, the arm of its kind (first keys or burst keys) whose row comes first
//! (in a run, the first of its kind to press, which without `--fixed-order` is not always the
//! first `--arm` given: an arm with fewer presses than the most skips the first round; pooled
//! takes read in the order the files are named): bootstrap intervals of the input, present and
//! worst-glass quantiles, and one-sided exact tests of the keys past the base's input and present
//! quantiles.
//! Exit 0 once every press ran or was dropped, 3 when the run could not: the
//! instance did not come up, a control call was refused, or a key was not booked in a minute.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::band::Band;
use crate::instance::{
    BOOT, Instance, Launch, World, resolve_aterm, take_flag, take_value, take_values,
};
use crate::{Fail, Res, stop};

const USAGE: &str = "usage: first-key [--aterm PATH] [--arm NAME=REST:PRESSES[:burst]]... \
     [--arm-toml NAME=LINE]... [--min-quiet SECS] [--settle LO..HI] [--no-pet] [--toml LINE]... \
     [--seed N] [--fixed-order] [--key-shape press|built] --out DIR\n       \
     first-key summarize PRESSES.TSV...\n\
     A press's quiet is the longer of --min-quiet (default 30) and its arm's REST plus 1 s. Each \
     arm is compared with its base: the arm of its kind (first key or burst) whose row comes \
     first, in press order and then in the order the files are named.";

/// The columns of `presses.tsv`, in order.
const COLUMNS: &[&str] = &[
    "press",
    "arm",
    "burst",
    "unix_ms",
    "quiet_s",
    "load_1m",
    "pressure",
    "footprint_mib",
    "resting",
    "pet_deep",
    "focused",
    "n_input",
    "n_input_cold",
    "n_rest_wake_key",
    "input_ms",
    "max_input_ms",
    "n_present",
    "present_ms",
    "max_present_ms",
    "key_write_ms",
    "key_queue_ms",
    "echo_ms",
    "n_glass",
    "max_glass_ms",
    "max_gpu_ms",
    "frames",
    "max_render_ms",
    "reading_asked_ms",
    "reading_began_ms",
    "reading_handed_ms",
    "aterm_runnable_ms",
    "aterm_cpu_ms",
    "aterm_ipc",
    "aterm_pageins",
    "shell_runnable_ms",
    "shell_cpu_ms",
    "out_req_ms",
    "present_gap_ms",
    "present_idle_ms",
    "present_pre_ms",
    "present_acquire_ms",
    "present_render_ms",
    "present_submit_ms",
    "present_submit_cpu_ms",
    "present_submit_faults",
    "present_encode_ms",
];

/// The index of `name` in [`COLUMNS`].
fn col(name: &str) -> usize {
    COLUMNS
        .iter()
        .position(|c| *c == name)
        .unwrap_or_else(|| panic!("no column {name}"))
}

/// One arm: the window's rest, its own config lines, and how many presses.
#[derive(Clone, Debug, PartialEq)]
pub struct Arm {
    pub name: String,
    /// `idle_rest_secs` (0: the window never rests).
    pub rest_secs: u32,
    pub presses: u32,
    /// Four more keys once the first is booked, each booked as a row of its own.
    pub burst: bool,
    /// Top-level `aterm.toml` lines of this arm alone (`--arm-toml NAME=LINE`), dotted keys for a
    /// table's (`sys.stats_bars = "off"`).
    pub toml: Vec<String>,
}

impl Arm {
    /// `NAME=REST:PRESSES[:burst]`.
    /// # Errors
    /// The spec is malformed.
    pub fn parse(spec: &str) -> Result<Arm, String> {
        let bad = || format!("--arm {spec}: want NAME=REST:PRESSES[:burst]");
        let (name, rest) = spec.split_once('=').ok_or_else(bad)?;
        let parts: Vec<&str> = rest.split(':').collect();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(bad());
        }
        let (burst, nums) = match parts.as_slice() {
            [a, b] => (false, [*a, *b]),
            [a, b, "burst"] => (true, [*a, *b]),
            _ => return Err(bad()),
        };
        Ok(Arm {
            name: name.to_string(),
            rest_secs: nums[0].parse().map_err(|_| bad())?,
            presses: nums[1].parse().map_err(|_| bad())?,
            burst,
            toml: Vec::new(),
        })
    }
}

/// The order the arms take their presses: rounds that each hold every arm whose share of the
/// presses so far is due, forward and backward by turns (A B C, C B A, …).
#[must_use]
pub fn schedule(arms: &[Arm]) -> Vec<usize> {
    let most = arms.iter().map(|a| a.presses).max().unwrap_or(0);
    let mut order = Vec::new();
    for round in 0..most {
        let mut this: Vec<usize> = (0..arms.len())
            .filter(|&i| {
                let n = u64::from(arms[i].presses);
                let (r, m) = (u64::from(round), u64::from(most));
                (r + 1) * n / m > r * n / m
            })
            .collect();
        if round % 2 == 1 {
            this.reverse();
        }
        order.extend(this);
    }
    order
}

/// How long a press of `arm` waits after the last key before it reads the window's state: the
/// longer of `min_quiet` and the arm's rest plus 1 s, so the state wait's minute (a hang detector,
/// not part of the quiet) starts once the window should already rest, however long its arm rests.
#[must_use]
pub fn quiet_for(min_quiet: Duration, arm: &Arm) -> Duration {
    min_quiet.max(Duration::from_secs(u64::from(arm.rest_secs) + 1))
}

/// A small deterministic generator (xorshift64*), for the settle and the burst gaps.
pub struct Rng(u64);

impl Rng {
    #[must_use]
    pub const fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    pub const fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `[lo, hi)`.
    pub fn between(&mut self, lo: f64, hi: f64) -> f64 {
        let unit = (self.next_u64() >> 11) as f64 / (1_u64 << 53) as f64;
        lo + (hi - lo) * unit
    }

    /// Uniform in `0..n`.
    pub fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next_u64() % (n.max(1) as u64)).unwrap_or(0)
    }
}

/// The `key=value` fields of a control reply, by key.
#[must_use]
pub fn fields(text: &str) -> std::collections::BTreeMap<String, String> {
    text.split_whitespace()
        .filter_map(|w| w.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn num(f: &std::collections::BTreeMap<String, String>, key: &str) -> Option<f64> {
    f.get(key).and_then(|v| v.parse::<f64>().ok())
}

/// The nearest-rank `q` quantile (0 < q <= 1) of `sorted`.
#[must_use]
pub fn rank(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let k = (q * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted[k.min(sorted.len()) - 1]
}

fn sorted(mut v: Vec<f64>) -> Vec<f64> {
    v.retain(|x| x.is_finite());
    v.sort_by(f64::total_cmp);
    v
}

fn ln_choose(n: u64, k: u64) -> f64 {
    let lf = |m: u64| (1..=m).map(|i| (i as f64).ln()).sum::<f64>();
    lf(n) - lf(k) - lf(n - k)
}

/// One-sided Fisher exact p that `a_hit` of `a_n` is no more than chance against `b_hit` of
/// `b_n`: the hypergeometric probability of `a_hit` or more of the hits falling in `a`.
#[must_use]
pub fn fisher_greater(a_hit: u64, a_n: u64, b_hit: u64, b_n: u64) -> f64 {
    let hits = a_hit + b_hit;
    let n = a_n + b_n;
    let denom = ln_choose(n, hits);
    (a_hit..=hits.min(a_n))
        .filter(|&k| hits - k <= b_n)
        .map(|k| (ln_choose(a_n, k) + ln_choose(b_n, hits - k) - denom).exp())
        .sum::<f64>()
        .min(1.0)
}

/// The 95% bootstrap interval of `q`(`a`) - `q`(`b`), from `resamples` resamples of each.
#[must_use]
pub fn bootstrap_diff(a: &[f64], b: &[f64], q: f64, resamples: usize, rng: &mut Rng) -> (f64, f64) {
    if a.is_empty() || b.is_empty() {
        return (f64::NAN, f64::NAN);
    }
    let mut diffs = Vec::with_capacity(resamples);
    let mut sa = vec![0.0; a.len()];
    let mut sb = vec![0.0; b.len()];
    for _ in 0..resamples {
        for x in &mut sa {
            *x = a[rng.below(a.len())];
        }
        for x in &mut sb {
            *x = b[rng.below(b.len())];
        }
        sa.sort_by(f64::total_cmp);
        sb.sort_by(f64::total_cmp);
        diffs.push(rank(&sa, q) - rank(&sb, q));
    }
    diffs.sort_by(f64::total_cmp);
    (rank(&diffs, 0.025), rank(&diffs, 0.975))
}

/// One press's row: each column of [`COLUMNS`] as text.
type Row = std::collections::BTreeMap<String, String>;

/// Reads `presses.tsv` files into rows.
/// # Errors
/// A file cannot be read or has no header.
pub fn read_rows(paths: &[PathBuf]) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    for path in paths {
        let text = crate::util::read_regular_text(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut lines = text.lines();
        let head: Vec<&str> = lines
            .next()
            .ok_or_else(|| format!("{} is empty", path.display()))?
            .split('\t')
            .collect();
        for line in lines.filter(|l| !l.trim().is_empty()) {
            rows.push(
                head.iter()
                    .zip(line.split('\t'))
                    .map(|(k, v)| ((*k).to_string(), v.to_string()))
                    .collect(),
            );
        }
    }
    Ok(rows)
}

fn column(rows: &[&Row], key: &str) -> Vec<f64> {
    sorted(
        rows.iter()
            .filter_map(|r| r.get(key).and_then(|v| v.parse::<f64>().ok()))
            .collect(),
    )
}

/// The `present_idle_ms` at or past which a row's present counts as cold: aterm's `COLD_GAP_NS`
/// (`crates/aterm-gui/src/metrics.rs`), the 2 s by which the GPU driver has freed its block.
const COLD_IDLE_MS: f64 = 2000.0;

/// What `rows` say of their cold presents: `(rows whose present_idle_ms is read, of those the
/// cold ones, the summed n_input_cold, the rows where a cold idle and a nonzero n_input_cold
/// disagree)`, over the rows that carry both columns for the disagreement.
fn cold_figures(rows: &[&Row]) -> (usize, usize, f64, usize) {
    let read = |r: &Row, k: &str| r.get(k).and_then(|v| v.parse::<f64>().ok());
    let idle: Vec<bool> = rows
        .iter()
        .filter_map(|r| read(r, "present_idle_ms").filter(|v| v.is_finite()))
        .map(|v| v >= COLD_IDLE_MS)
        .collect();
    let counted: f64 = rows
        .iter()
        .filter_map(|r| read(r, "n_input_cold").filter(|v| v.is_finite()))
        .sum();
    let disagree = rows
        .iter()
        .filter_map(|r| {
            let idle = read(r, "present_idle_ms").filter(|v| v.is_finite())?;
            let cold = read(r, "n_input_cold").filter(|v| v.is_finite())?;
            Some((idle >= COLD_IDLE_MS) != (cold > 0.0))
        })
        .filter(|&d| d)
        .count();
    (
        idle.len(),
        idle.iter().filter(|&&c| c).count(),
        counted,
        disagree,
    )
}

/// Whether `row` books a burst press's key (`burst` 2 to 5) rather than a first key (0).
fn is_burst(row: &Row) -> bool {
    row.get("burst")
        .and_then(|v| v.parse::<u32>().ok())
        .is_some_and(|k| k > 0)
}

/// The summary of `rows`: per arm, then each arm against its base, the first arm of its kind to
/// appear in `rows` (first keys against first keys, burst keys against burst keys).
#[must_use]
pub fn summarize(rows: &[Row]) -> String {
    let mut names: Vec<String> = Vec::new();
    for r in rows {
        let name = r.get("arm").cloned().unwrap_or_default();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let of = |name: &str| -> Vec<&Row> {
        rows.iter()
            .filter(|r| r.get("arm").is_some_and(|a| a == name))
            .collect()
    };
    let q3 = |v: &[f64]| {
        format!(
            "{:.2} / {:.2} / {:.2}",
            rank(v, 0.5),
            rank(v, 0.95),
            rank(v, 0.99)
        )
    };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "arm  n_input  input p50/p95/p99 (ms)  n_present  present p50/p95/p99  key_write p50/p95/p99  echo p50/p95/p99  worst glass p50/p95  load p50"
    );
    for name in &names {
        let rows = of(name);
        let input = column(&rows, "input_ms");
        let present = column(&rows, "present_ms");
        let glass = column(&rows, "max_glass_ms");
        let _ = writeln!(
            out,
            "{name}  {}  {}  {}  {}  {}  {}  {:.2} / {:.2}  {:.1}",
            input.len(),
            q3(&input),
            present.len(),
            q3(&present),
            q3(&column(&rows, "key_write_ms")),
            q3(&column(&rows, "echo_ms")),
            rank(&glass, 0.5),
            rank(&glass, 0.95),
            rank(&column(&rows, "load_1m"), 0.5),
        );
    }
    let _ = writeln!(
        out,
        "\narm  cold share (present_idle_ms >= {COLD_IDLE_MS:.0})  n_input_cold  rows disagreeing  n_rest_wake_key  submit p50/p95 (ms)  key_queue p99"
    );
    for name in &names {
        let rows = of(name);
        let (read, cold, counted, disagree) = cold_figures(&rows);
        let submit = column(&rows, "present_submit_ms");
        let wakes: f64 = column(&rows, "n_rest_wake_key").iter().sum();
        let _ = writeln!(
            out,
            "{name}  {cold} of {read} ({:.1}%)  {counted:.0}  {disagree}  {wakes:.0}  {:.2} / {:.2}  {:.2}",
            100.0 * cold as f64 / read.max(1) as f64,
            rank(&submit, 0.5),
            rank(&submit, 0.95),
            rank(&column(&rows, "key_queue_ms"), 0.99),
        );
    }
    let _ = writeln!(
        out,
        "\narm  cold keys (n_input_cold > 0)  input_cold p50/p95/p99 (ms)  present p50/p95/p99 of cold keys  pressure normal / warn / critical / unread"
    );
    for name in &names {
        let rows = of(name);
        let cold: Vec<&Row> = rows
            .iter()
            .copied()
            .filter(|r| {
                r.get("n_input_cold")
                    .and_then(|v| v.parse::<f64>().ok())
                    .is_some_and(|n| n > 0.0)
            })
            .collect();
        let level = |l: &str| {
            rows.iter()
                .filter(|r| r.get("pressure").is_some_and(|p| p == l))
                .count()
        };
        let [normal, warn, critical] = ["normal", "warn", "critical"].map(level);
        let _ = writeln!(
            out,
            "{name}  {}  {}  {}  {normal} / {warn} / {critical} / {}",
            cold.len(),
            q3(&column(&cold, "input_ms")),
            q3(&column(&cold, "present_ms")),
            rows.len() - normal - warn - critical,
        );
    }
    let burst = |name: &str| of(name).first().is_some_and(|r| is_burst(r));
    let above = |v: &[f64], t: f64| v.iter().filter(|&&x| x > t).count() as u64;
    let mut rng = Rng::new(0x5EED_F1E5);
    for (i, name) in names.iter().enumerate() {
        let Some(base) = names[..i].iter().find(|b| burst(b) == burst(name)) else {
            continue;
        };
        let base_rows = of(base);
        let b_input = column(&base_rows, "input_ms");
        let b_present = column(&base_rows, "present_ms");
        let b_glass = column(&base_rows, "max_glass_ms");
        let (b95, b99) = (rank(&b_input, 0.95), rank(&b_input, 0.99));
        let b_present95 = rank(&b_present, 0.95);
        let rows = of(name);
        let input = column(&rows, "input_ms");
        let present = column(&rows, "present_ms");
        let glass = column(&rows, "max_glass_ms");
        let _ = writeln!(out, "\n{name} - {base}:");
        for (q, label) in [(0.5, "p50"), (0.95, "p95"), (0.99, "p99")] {
            let (lo, hi) = bootstrap_diff(&input, &b_input, q, 10_000, &mut rng);
            let _ = writeln!(
                out,
                "  input {label} {:+.2} ms (95% bootstrap {lo:+.2} .. {hi:+.2})",
                rank(&input, q) - rank(&b_input, q)
            );
            let (lo, hi) = bootstrap_diff(&present, &b_present, q, 10_000, &mut rng);
            let _ = writeln!(
                out,
                "  present {label} {:+.2} ms (95% bootstrap {lo:+.2} .. {hi:+.2})",
                rank(&present, q) - rank(&b_present, q)
            );
        }
        for (what, a, b) in [
            ("present", &present, &b_present),
            ("worst glass", &glass, &b_glass),
        ] {
            let (lo, hi) = bootstrap_diff(a, b, 0.95, 10_000, &mut rng);
            let _ = writeln!(
                out,
                "  {what} p95 {:+.2} ms (95% bootstrap {lo:+.2} .. {hi:+.2})",
                rank(a, 0.95) - rank(b, 0.95)
            );
        }
        let _ = writeln!(
            out,
            "  present p99 {:+.2} ms",
            rank(&present, 0.99) - rank(&b_present, 0.99)
        );
        for (what, a, b, t, label) in [
            ("input", &input, &b_input, b95, "p95"),
            ("input", &input, &b_input, b99, "p99"),
            ("present", &present, &b_present, b_present95, "p95"),
        ] {
            let (a_hit, b_hit) = (above(a, t), above(b, t));
            let _ = writeln!(
                out,
                "  above {base}'s {what} {label} ({t:.2} ms): {a_hit} of {} ({:.1}%) against {b_hit} of {}; one-sided Fisher p {:.4}",
                a.len(),
                100.0 * a_hit as f64 / a.len().max(1) as f64,
                b.len(),
                fisher_greater(a_hit, a.len() as u64, b_hit, b.len() as u64)
            );
        }
    }
    out
}

/// The run's options.
struct Opts {
    aterm: PathBuf,
    arms: Vec<Arm>,
    min_quiet: Duration,
    settle: (f64, f64),
    pet: bool,
    toml: Vec<String>,
    seed: u64,
    out: PathBuf,
    fixed_order: bool,
    key_shape: String,
}

fn parse(args: &[String]) -> Result<Opts, Fail> {
    let mut a = args.to_vec();
    let aterm = take_value(&mut a, "--aterm")?;
    let arms = take_values(&mut a, "--arm")?
        .iter()
        .map(|s| Arm::parse(s))
        .collect::<Result<Vec<_>, _>>()
        .map_err(Fail::usage)?;
    let mut arms = if arms.is_empty() {
        ["A=0:200", "R=30:200"]
            .iter()
            .map(|s| Arm::parse(s))
            .collect::<Result<Vec<_>, _>>()
            .map_err(Fail::usage)?
    } else {
        arms
    };
    for spec in take_values(&mut a, "--arm-toml")? {
        let (name, line) = spec
            .split_once('=')
            .ok_or_else(|| Fail::usage("--arm-toml takes NAME=LINE"))?;
        let arm = arms
            .iter_mut()
            .find(|arm| arm.name == name)
            .ok_or_else(|| Fail::usage(format!("--arm-toml {spec}: no arm {name}")))?;
        arm.toml.push(line.to_string());
    }
    let min_quiet = take_value(&mut a, "--min-quiet")?
        .map(|s| s.parse::<f64>())
        .transpose()
        .map_err(|_| Fail::usage("--min-quiet takes seconds"))?
        .unwrap_or(30.0);
    let settle = match take_value(&mut a, "--settle")? {
        None => (2.0, 6.0),
        Some(s) => {
            let (lo, hi) = s
                .split_once("..")
                .and_then(|(l, h)| Some((l.parse::<f64>().ok()?, h.parse::<f64>().ok()?)))
                .filter(|(l, h)| *l >= 0.0 && h > l)
                .ok_or_else(|| Fail::usage("--settle takes LO..HI seconds"))?;
            (lo, hi)
        }
    };
    let pet = !take_flag(&mut a, "--no-pet");
    let fixed_order = take_flag(&mut a, "--fixed-order");
    let key_shape = take_value(&mut a, "--key-shape")?.unwrap_or_else(|| "press".into());
    if !matches!(key_shape.as_str(), "press" | "built") {
        return Err(Fail::usage("--key-shape takes press or built"));
    }
    let toml = take_values(&mut a, "--toml")?;
    let seed = take_value(&mut a, "--seed")?
        .map(|s| s.parse::<u64>())
        .transpose()
        .map_err(|_| Fail::usage("--seed takes a number"))?
        .unwrap_or(0x0F1E_57EE);
    let out = take_value(&mut a, "--out")?
        .map(|p| PathBuf::from(crate::paths::from_caller(&p)))
        .ok_or_else(|| Fail::usage(USAGE))?;
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "first-key: unexpected '{extra}'\n{USAGE}"
        )));
    }
    let aterm = resolve_aterm(aterm).map_err(|e| Fail::exit(3, e))?;
    Ok(Opts {
        aterm,
        arms,
        min_quiet: Duration::from_secs_f64(min_quiet),
        settle,
        pet,
        toml,
        seed,
        out,
        fixed_order,
        key_shape,
    })
}

/// Runs `lab first-key`.
pub fn cmd_first_key(args: &[String]) -> Res {
    if args.first().is_some_and(|a| a == "summarize") {
        let paths: Vec<PathBuf> = args[1..].iter().map(PathBuf::from).collect();
        if paths.is_empty() {
            return Err(Fail::usage(USAGE));
        }
        print!("{}", summarize(&read_rows(&paths)?));
        return Ok(0);
    }
    stop::install();
    let o = parse(args)?;
    std::fs::create_dir_all(&o.out)
        .map_err(|e| Fail::exit(3, format!("cannot make {}: {e}", o.out.display())))?;
    let could_not = |f: Fail| {
        if f.stopped.is_some() || f.code == 3 {
            f
        } else {
            Fail::exit(3, f.msg.unwrap_or_default())
        }
    };
    let (rows, dropped) = drive(&o).map_err(could_not)?;
    let mut summary = summarize(&rows);
    if dropped > 0 {
        let _ = writeln!(
            summary,
            "\n{dropped} presses dropped: the window was not in its arm's state a minute past its quiet (dropped.tsv)"
        );
    }
    let _ = crate::util::write_fresh(&o.out.join("summary.txt"), &summary);
    print!("{summary}");
    Ok(0)
}

/// The config every press shares, with the arm's rest and lines at its head (the lanes' tables
/// follow).
fn config(o: &Opts, arm: &Arm) -> String {
    let mut top = vec![format!("idle_rest_secs = {}", arm.rest_secs)];
    top.extend(arm.toml.iter().cloned());
    top.extend(o.toml.iter().cloned());
    crate::instance::config_with(&top)
}

fn ctl(inst: &Instance, args: &[&str]) -> Result<String, Fail> {
    let r = inst.ctl(args);
    if r.code != 0 {
        return Err(Fail::exit(
            3,
            format!(
                "first-key: `{}` refused: {} {}",
                args.join(" "),
                r.stdout.trim(),
                r.stderr.trim()
            ),
        ));
    }
    Ok(r.stdout)
}

type Client = aterm_agent::RelayClient<aterm_uds::CtlStream>;

fn call(conn: &mut Client, line: &str) -> Result<String, Fail> {
    let reply = conn
        .request_counted(line)
        .map(|(head, body)| format!("{head}\n{body}"))
        .map_err(|e| Fail::exit(3, format!("first-key: `{line}`: {e}")))?;
    if reply.starts_with("ERR") {
        return Err(Fail::exit(
            3,
            format!("first-key: `{line}` refused: {}", reply.trim()),
        ));
    }
    Ok(reply)
}

/// `metrics` once it has booked `keys` keys since its reset, polled behind a one-minute hang
/// detector.
fn booked(
    conn: &mut Client,
    keys: u32,
    press: usize,
    arm: &Arm,
) -> Result<(std::collections::BTreeMap<String, String>, String), Fail> {
    let hung = stop::deadline_after(Duration::from_secs(60));
    loop {
        let raw = call(conn, "metrics")?;
        let m = fields(&raw);
        if num(&m, "n_input").is_some_and(|n| n >= f64::from(keys)) {
            return Ok((m, raw));
        }
        if Instant::now() >= hung {
            return Err(Fail::exit(
                3,
                format!(
                    "first-key: press {press} ({}): its key {keys} was never booked in a minute; last metrics: {raw}",
                    arm.name,
                ),
            ));
        }
        stop::sleep(Duration::from_millis(20))?;
    }
}

/// What a process used, at one instant.
#[derive(Clone, Copy, Default)]
struct Use {
    runnable_ns: u64,
    cpu_ns: u64,
    instructions: u64,
    cycles: u64,
    pageins: u64,
    footprint: u64,
}

fn used(pid: Option<i32>) -> Use {
    pid.and_then(crate::sys::rusage)
        .map_or_else(Use::default, |r| Use {
            runnable_ns: r.ri_runnable_time,
            cpu_ns: r.ri_user_time + r.ri_system_time,
            instructions: r.ri_instructions,
            cycles: r.ri_cycles,
            pageins: r.ri_pageins,
            footprint: r.ri_phys_footprint,
        })
}

/// A Mach-time span in milliseconds (`rusage`'s times are Mach ticks).
fn ticks_ms(ticks: u64) -> f64 {
    let (numer, denom) = crate::sys::mach_timebase().unwrap_or((1, 1));
    ticks as f64 * f64::from(numer) / f64::from(denom) / 1e6
}

fn drive(o: &Opts) -> Result<(Vec<Row>, usize), Fail> {
    let key_request = format!("hwkey a shape={}", o.key_shape);
    let first = o.arms.first().ok_or_else(|| Fail::usage(USAGE))?;
    let mut config_now = config(o, first);
    let world = World::new(&config_now)?;
    let toml_path = world.cfg.join("aterm/aterm.toml");
    let env = world.env();
    let mut inst = Instance::start_in(
        &world,
        Launch {
            aterm: o.aterm.clone(),
            headless: false,
            args: ["--columns", "110", "--lines", "32"]
                .map(String::from)
                .to_vec(),
            env,
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
        Band::App,
    )?;
    inst.await_socket(BOOT)?;
    let aterm_pid = inst.aterm_pid();
    // The tab's shell: aterm's child running `/bin/sh` (the world's `SHELL`).
    let shell_pid = aterm_pid
        .and_then(aterm_uds::process::child_pids)
        .unwrap_or_default()
        .into_iter()
        .find(|&pid| {
            aterm_uds::process::read_process_path(pid).is_some_and(|path| {
                path.file_name()
                    .is_some_and(|name| name == "sh" || name == "bash")
            })
        });
    eprintln!(
        "first-key: instance aterm_pid={} shell_pid={} sock={}",
        aterm_pid.unwrap_or(-1),
        shell_pid.unwrap_or(-1),
        inst.sock.display()
    );
    let _ = ctl(&inst, &["await", "idle", "3000", "timeout=60000"]);
    let mut conn = inst.connect().map_err(|e| Fail::exit(3, e))?;
    // A posted NSEvent is not evidence that the window received it. Select and raise only
    // this private world's terminal, then observe native focus before sending a warm-up key.
    let roster = call(&mut conn, "sessions")?;
    let target = crate::instance::terminal_selector(&roster)
        .ok_or_else(|| Fail::exit(3, "first-key: no private terminal to raise"))?;
    call(
        &mut conn,
        &format!("raise {}", target.trim_start_matches('@')),
    )?;
    let focus_deadline = stop::deadline_after(Duration::from_secs(60));
    loop {
        let focus = call(&mut conn, "native-focus")?;
        if fields(&focus).get("key").is_some_and(|key| key == "1") {
            crate::util::write_fresh(&o.out.join("native-focus.txt"), &focus)
                .map_err(|e| e.to_string())?;
            break;
        }
        if Instant::now() >= focus_deadline {
            crate::util::write_fresh(&o.out.join("native-focus.txt"), &focus)
                .map_err(|e| e.to_string())?;
            return Err(Fail::exit(
                3,
                format!("first-key: native focus unavailable: {focus}"),
            ));
        }
        stop::sleep(Duration::from_millis(20))?;
    }
    // A warm-up press: an instance's first key pays its own first-use costs.
    call(&mut conn, &key_request)?;
    stop::sleep(Duration::from_millis(600))?;
    ctl(&inst, &["key", "ctrl+u"])?;

    let order = if o.fixed_order {
        (0..o.arms.iter().map(|a| a.presses).max().unwrap_or(0))
            .flat_map(|round| {
                o.arms
                    .iter()
                    .enumerate()
                    .filter_map(move |(i, arm)| (round < arm.presses).then_some(i))
            })
            .collect()
    } else {
        schedule(&o.arms)
    };
    let raw_dir = o.out.join("raw");
    std::fs::create_dir_all(&raw_dir)
        .map_err(|e| format!("cannot make {}: {e}", raw_dir.display()))?;
    let tsv = o.out.join("presses.tsv");
    let mut file = crate::util::append_regular_file(&tsv)
        .map_err(|e| format!("cannot open {}: {e}", tsv.display()))?;
    if file.metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        use std::io::Write as _;
        let _ = writeln!(file, "{}", COLUMNS.join("\t"));
    }
    let mut rng = Rng::new(o.seed);
    let mut rows = Vec::new();
    let mut dropped = 0_usize;
    let mut last_key = Instant::now();
    for (press, &i) in order.iter().enumerate() {
        let arm = &o.arms[i];
        let wanted = config(o, arm);
        if wanted != config_now {
            crate::util::atomic_write(&toml_path, crate::instance::bell_off(&wanted).as_bytes())
                .map_err(|e| format!("cannot write {}: {e}", toml_path.display()))?;
            config_now = wanted;
        }
        stop::sleep(quiet_for(o.min_quiet, arm).saturating_sub(last_key.elapsed()))?;
        let mut status;
        let mut status_raw;
        let given_up = stop::deadline_after(Duration::from_secs(60));
        loop {
            status_raw = ctl(&inst, &["trail", "status"])?;
            status = fields(&status_raw);
            let rests = status.get("resting").is_some_and(|v| v == "true");
            let deep = !o.pet || status.get("pet_deep").is_some_and(|v| v == "true");
            if deep && rests == (arm.rest_secs > 0) {
                break;
            }
            if Instant::now() >= given_up {
                break;
            }
            stop::sleep(Duration::from_secs(1))?;
        }
        let rests = status.get("resting").is_some_and(|v| v == "true");
        let deep = !o.pet || status.get("pet_deep").is_some_and(|v| v == "true");
        if !(deep && rests == (arm.rest_secs > 0)) {
            // Something outside the run touched the window (a pointer over it, the focus coming
            // back) or its tab printed: the press is not the state under test, so it is dropped
            // and counted rather than measured, and the next press waits for its own state.
            let line = format!(
                "{press}\t{}\tresting={} pet_deep={} focused={}",
                arm.name,
                status.get("resting").map_or("?", String::as_str),
                status.get("pet_deep").map_or("?", String::as_str),
                status.get("focused").map_or("?", String::as_str)
            );
            eprintln!("first-key: dropped press {line}");
            let dropped_tsv = o.out.join("dropped.tsv");
            let _ = crate::util::append_regular_file(&dropped_tsv).and_then(|mut f| {
                use std::io::Write as _;
                writeln!(f, "{line}")
            });
            let _ = crate::util::write_fresh(
                &raw_dir.join(format!("{press}-{}-dropped.txt", arm.name)),
                &status_raw,
            );
            dropped += 1;
            continue;
        }
        stop::sleep(Duration::from_secs_f64(rng.between(o.settle.0, o.settle.1)))?;
        let load = aterm_sysprobe::load_average().map_or(f64::NAN, |l| l[0]);
        let pressure = aterm_sysprobe::memory_pressure().map_or("-", |l| l.as_str());
        let footprint = used(aterm_pid).footprint;
        stop::sleep(Duration::from_secs(2))?;
        let (a0, s0) = (used(aterm_pid), used(shell_pid));
        call(&mut conn, "metrics reset")?;
        let quiet = last_key.elapsed();
        let t0 = Instant::now();
        call(&mut conn, &key_request)?;
        last_key = t0;
        let get = |f: &std::collections::BTreeMap<String, String>, k: &str| {
            num(f, k).map_or_else(|| "nan".to_string(), |v| format!("{v}"))
        };
        // A burst press's keys 2 to 5, each `(input, present, cold, rest wake)` as its own
        // booking read them; the two counts are that key's own, the step from the key before.
        let mut burst_keys = Vec::new();
        let mut expect = 1;
        if arm.burst {
            // The first key is booked before the reset, so a slow one is not counted among the
            // four.
            let _ = booked(&mut conn, 1, press, arm)?;
            call(&mut conn, "metrics reset")?;
            let mut at = Duration::ZERO;
            let mut counted = [Some(0.0); 2];
            for key in 1..=4 {
                at += Duration::from_secs_f64(rng.between(0.100, 0.250));
                stop::sleep(at.saturating_sub(t0.elapsed()))?;
                call(&mut conn, &key_request)?;
                let (k, _) = booked(&mut conn, key, press, arm)?;
                let [cold, wake] = ["n_input_cold", "n_rest_wake_key"].map(|f| num(&k, f));
                let step = |now: Option<f64>, before: Option<f64>| {
                    now.zip(before)
                        .map_or_else(|| "nan".to_string(), |(n, b)| format!("{}", n - b))
                };
                burst_keys.push((
                    get(&k, "last_input_present_ms"),
                    get(&k, "last_present_latency_ms"),
                    step(cold, counted[0]),
                    step(wake, counted[1]),
                ));
                counted = [cold, wake];
            }
            expect = 4;
            last_key = Instant::now();
        }
        stop::sleep(Duration::from_millis(600))?;
        let (a1, s1) = (used(aterm_pid), used(shell_pid));
        let (m, m_raw) = booked(&mut conn, expect, press, arm)?;
        let p_raw = call(&mut conn, "metrics percentiles")?;
        let p = fields(&p_raw);
        // Every field of both replies, beside the row, for what a column does not carry.
        let _ = crate::util::write_fresh(
            &raw_dir.join(format!("{press}-{}.txt", arm.name)),
            format!("{status_raw}\n{m_raw}\n{p_raw}\n"),
        );
        ctl(&inst, &["key", "ctrl+u"])?;
        // The key's arrival on the metrics clock: its booking less its key-to-present.
        let key_at = num(&m, "max_input_present_at_ms")
            .zip(num(&m, "max_input_present_ms"))
            .map(|(at, d)| at - d);
        let rel = |k: &str| match (num(&m, k).filter(|v| *v > 0.0), key_at) {
            (Some(v), Some(at)) => format!("{:.2}", v - at),
            _ => "nan".to_string(),
        };
        let input = get(&m, "last_input_present_ms");
        let present = get(&m, "last_present_latency_ms");
        let ipc = |a: Use, b: Use| {
            let cycles = b.cycles.saturating_sub(a.cycles);
            if cycles == 0 {
                "nan".to_string()
            } else {
                format!(
                    "{:.3}",
                    b.instructions.saturating_sub(a.instructions) as f64 / cycles as f64
                )
            }
        };
        let unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let values = [
            press.to_string(),
            arm.name.clone(),
            "0".to_string(),
            unix_ms.to_string(),
            format!("{:.1}", quiet.as_secs_f64()),
            format!("{load:.1}"),
            pressure.to_string(),
            format!("{:.1}", footprint as f64 / (1024.0 * 1024.0)),
            status.get("resting").cloned().unwrap_or_default(),
            status.get("pet_deep").cloned().unwrap_or_default(),
            status.get("focused").cloned().unwrap_or_default(),
            get(&m, "n_input"),
            get(&m, "n_input_cold"),
            get(&m, "n_rest_wake_key"),
            input,
            get(&m, "max_input_present_ms"),
            get(&m, "n_present"),
            present,
            get(&m, "max_present_latency_ms"),
            get(&m, "last_key_write_ms"),
            get(&p, "last_key_queue_ms"),
            get(&m, "echo_last_ms"),
            get(&p, "n_present_glass"),
            get(&p, "max_present_glass_ms"),
            get(&m, "max_gpu_time_ms"),
            get(&m, "frames"),
            get(&m, "max_frame_render_ms"),
            rel("last_reading_asked_at_ms"),
            rel("last_reading_began_at_ms"),
            rel("last_reading_handed_at_ms"),
            format!(
                "{:.3}",
                ticks_ms(a1.runnable_ns.saturating_sub(a0.runnable_ns))
            ),
            format!("{:.3}", ticks_ms(a1.cpu_ns.saturating_sub(a0.cpu_ns))),
            ipc(a0, a1),
            a1.pageins.saturating_sub(a0.pageins).to_string(),
            format!(
                "{:.3}",
                ticks_ms(s1.runnable_ns.saturating_sub(s0.runnable_ns))
            ),
            format!("{:.3}", ticks_ms(s1.cpu_ns.saturating_sub(s0.cpu_ns))),
            get(&p, "max_out_req_ms"),
            get(&m, "max_present_latency_gap_ms"),
            get(&m, "max_present_latency_idle_ms"),
            get(&m, "max_present_latency_pre_present_ms"),
            get(&m, "max_present_latency_acquire_ms"),
            get(&m, "max_present_latency_render_ms"),
            get(&m, "max_present_latency_submit_wall_ms"),
            get(&m, "max_present_latency_submit_cpu_ms"),
            get(&m, "max_present_latency_submit_faults"),
            get(&m, "max_present_latency_encode_ms"),
        ];
        debug_assert_eq!(values.len(), COLUMNS.len());
        eprintln!(
            "first-key: press {press} {} input={} present={} echo={} load={load:.1} reading_began={}",
            arm.name,
            values[col("input_ms")],
            values[col("present_ms")],
            values[col("echo_ms")],
            values[col("reading_began_ms")]
        );
        // A first-key press is one row; a burst press is a row per key 2 to 5, each with that
        // key's own booking and the press's other columns.
        let keyed: Vec<_> = if burst_keys.is_empty() {
            vec![values]
        } else {
            (2_u8..)
                .zip(burst_keys)
                .map(|(key, (input, present, cold, wake))| {
                    let mut v = values.clone();
                    v[col("burst")] = key.to_string();
                    v[col("input_ms")] = input;
                    v[col("present_ms")] = present;
                    v[col("n_input_cold")] = cold;
                    v[col("n_rest_wake_key")] = wake;
                    v
                })
                .collect()
        };
        for values in keyed {
            {
                use std::io::Write as _;
                let _ = writeln!(file, "{}", values.join("\t"));
            }
            rows.push(
                COLUMNS
                    .iter()
                    .zip(values)
                    .map(|(k, v)| ((*k).to_string(), v))
                    .collect(),
            );
        }
    }
    inst.stop().map_err(Fail::die)?;
    Ok((rows, dropped))
}

#[cfg(test)]
#[path = "first_key_tests.rs"]
mod tests;
