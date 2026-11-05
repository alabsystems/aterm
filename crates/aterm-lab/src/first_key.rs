// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `first-key`: the first key a window takes after a quiet, arm against arm (ATERM-KI-1027-4,
//! `docs/measured/idle-memory-rest-2026-10-29.md`).
//!
//! One windowed private instance (110x32) of the aterm given runs every press. Each arm names the
//! window's `idle_rest_secs` and any `aterm.toml` lines of its own; the arms take turns press by
//! press, each round of them reversed after the last, so a drift in the Mac's load falls on every
//! arm alike.
//! A press waits out its quiet (`--min-quiet`, then until `trail status` says the pet sleeps
//! deeply and the window rests exactly when its arm rests it, then a random `--settle` more, so
//! the key meets the resting window's 5 s reading at any phase), reads the load, the memory
//! pressure and aterm's footprint, waits 2 s, resets the metrics, presses `a` through the OS event
//! path (`hwkey`), and reads `metrics` 600 ms later. A burst arm presses four more keys once the
//! first is booked, each a random 100 to 250 ms after the one before, and books each of those four
//! from its own booking as a row of its own (`burst` 2 to 5), compared with the first burst arm's.
//! `key ctrl+u` then clears the shell's line, which starts the next quiet. A press whose window is
//! not in its arm's state a minute past its quiet (a pointer crossed it, the focus came back to
//! it, its tab printed) is dropped and counted, with its status in `<out>/dropped.tsv`.
//!
//! Every first-key press is a row of `<out>/presses.tsv` (a burst press four), written as it
//! ends, with its `trail status`, `metrics` and `metrics percentiles` replies whole in
//! `<out>/raw/<press>-<arm>.txt`; the summary (nearest-rank percentiles per arm, bootstrap
//! intervals and one-sided exact tests against the first arm of its kind) is `<out>/summary.txt`, and `first-key summarize <presses.tsv>...` reads
//! it again from rows. Exit 0 once every press ran or was dropped, 3 when the run could not: the
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
     [--seed N] --out DIR\n       \
     first-key summarize PRESSES.TSV...";

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

/// Whether `row` books a burst press's key (`burst` 2 to 5) rather than a first key (0).
fn is_burst(row: &Row) -> bool {
    row.get("burst")
        .and_then(|v| v.parse::<u32>().ok())
        .is_some_and(|k| k > 0)
}

/// The summary of `rows`: per arm, then each arm against the first arm (by first appearance) of
/// its kind, first keys against first keys and burst keys against burst keys.
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
        }
        let _ = writeln!(
            out,
            "  present p99 {:+.2} ms; worst glass p95 {:+.2} ms",
            rank(&present, 0.99) - rank(&b_present, 0.99),
            rank(&glass, 0.95) - rank(&b_glass, 0.95)
        );
        for (t, label) in [(b95, "p95"), (b99, "p99")] {
            let (a_hit, b_hit) = (above(&input, t), above(&b_input, t));
            let _ = writeln!(
                out,
                "  above {base}'s input {label} ({t:.2} ms): {a_hit} of {} ({:.1}%) against {b_hit} of {}; one-sided Fisher p {:.4}",
                input.len(),
                100.0 * a_hit as f64 / input.len().max(1) as f64,
                b_input.len(),
                fisher_greater(a_hit, input.len() as u64, b_hit, b_input.len() as u64)
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
                    "first-key: press {press} ({}): its key {keys} was never booked in a minute",
                    arm.name
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
    // A warm-up press: an instance's first key pays its own first-use costs.
    call(&mut conn, "hwkey a")?;
    stop::sleep(Duration::from_millis(600))?;
    ctl(&inst, &["key", "ctrl+u"])?;

    let order = schedule(&o.arms);
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
        stop::sleep(o.min_quiet.saturating_sub(last_key.elapsed()))?;
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
        call(&mut conn, "hwkey a")?;
        last_key = t0;
        let get = |f: &std::collections::BTreeMap<String, String>, k: &str| {
            num(f, k).map_or_else(|| "nan".to_string(), |v| format!("{v}"))
        };
        // A burst press's keys 2 to 5, each `(input, present)` as its own booking read them.
        let mut burst_keys = Vec::new();
        let mut expect = 1;
        if arm.burst {
            // The first key is booked before the reset, so a slow one is not counted among the
            // four.
            let _ = booked(&mut conn, 1, press, arm)?;
            call(&mut conn, "metrics reset")?;
            let mut at = Duration::ZERO;
            for key in 1..=4 {
                at += Duration::from_secs_f64(rng.between(0.100, 0.250));
                stop::sleep(at.saturating_sub(t0.elapsed()))?;
                call(&mut conn, "hwkey a")?;
                let (k, _) = booked(&mut conn, key, press, arm)?;
                burst_keys.push((
                    get(&k, "last_input_present_ms"),
                    get(&k, "last_present_latency_ms"),
                ));
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
                .map(|(key, (input, present))| {
                    let mut v = values.clone();
                    v[col("burst")] = key.to_string();
                    v[col("input_ms")] = input;
                    v[col("present_ms")] = present;
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
mod tests {
    use super::*;

    fn arm_names(rows: &[Row]) -> Vec<String> {
        rows.iter().filter_map(|r| r.get("arm").cloned()).collect()
    }

    #[test]
    fn an_arm_reads_its_rest_presses_and_burst() {
        assert_eq!(
            Arm::parse("R=30:200").unwrap(),
            Arm {
                name: "R".into(),
                rest_secs: 30,
                presses: 200,
                burst: false,
                toml: Vec::new(),
            }
        );
        assert!(Arm::parse("Rb=30:50:burst").unwrap().burst);
        for bad in [
            "R",
            "R=30",
            "=30:1",
            "R=x:1",
            "R=30:1:loud",
            "R=30:150:1",
            "R-1=30:1",
        ] {
            assert!(Arm::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_schedule_spreads_each_arm_and_reverses_every_other_round() {
        let arm = |s: &str| Arm::parse(s).unwrap();
        let arms = [arm("A=0:4"), arm("H=30:4"), arm("R=30:2")];
        let names: Vec<&str> = schedule(&arms)
            .into_iter()
            .map(|i| arms[i].name.as_str())
            .collect();
        assert_eq!(names, ["A", "H", "R", "H", "A", "A", "H", "R", "H", "A"]);
        let two = [arm("A=0:3"), arm("H=30:3")];
        let names: Vec<&str> = schedule(&two)
            .into_iter()
            .map(|i| two[i].name.as_str())
            .collect();
        assert_eq!(names, ["A", "H", "H", "A", "A", "H"], "ABBA");
    }

    #[test]
    fn nearest_rank_quantiles() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(rank(&v, 0.5), 50.0);
        assert_eq!(rank(&v, 0.95), 95.0);
        assert_eq!(rank(&v, 0.99), 99.0);
        assert_eq!(rank(&v[..24], 0.99), 24.0, "of 24, p99 is the worst");
        assert!(rank(&[], 0.5).is_nan());
    }

    #[test]
    fn the_exact_test_matches_the_hypergeometric_tail() {
        // 5 of 24 against 0 of 24: the design's figure, one-sided p 0.025.
        let p = fisher_greater(5, 24, 0, 24);
        assert!((p - 0.0250).abs() < 0.0005, "{p}");
        assert!((fisher_greater(0, 10, 5, 10) - 1.0).abs() < 1e-9);
        let even = fisher_greater(3, 10, 3, 10);
        assert!(even > 0.5 && even < 1.0, "{even}");
    }

    #[test]
    fn a_bootstrap_of_two_equal_samples_straddles_zero() {
        let a: Vec<f64> = (0..200).map(|i| f64::from(i % 17)).collect();
        let mut rng = Rng::new(7);
        let (lo, hi) = bootstrap_diff(&sorted(a.clone()), &sorted(a), 0.5, 2000, &mut rng);
        assert!(lo <= 0.0 && hi >= 0.0, "{lo}..{hi}");
    }

    #[test]
    fn rows_round_trip_through_the_tsv_and_summarize() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("presses.tsv");
        let mut text = format!("{}\n", COLUMNS.join("\t"));
        for (i, (arm, input)) in [("A", 1.5), ("R", 4.0), ("R", 2.0), ("A", 1.7)]
            .into_iter()
            .enumerate()
        {
            let row: Vec<String> = COLUMNS
                .iter()
                .map(|c| match *c {
                    "press" => i.to_string(),
                    "arm" => arm.to_string(),
                    "input_ms" => input.to_string(),
                    "burst" => "0".to_string(),
                    _ => "1".to_string(),
                })
                .collect();
            let _ = writeln!(text, "{}", row.join("\t"));
        }
        std::fs::write(&path, text).unwrap();
        let rows = read_rows(&[path]).unwrap();
        assert_eq!(arm_names(&rows), ["A", "R", "R", "A"]);
        let s = summarize(&rows);
        assert!(s.contains("A  2  1.50 / 1.70 / 1.70"), "{s}");
        assert!(s.contains("R - A:"), "{s}");
        assert!(s.contains("input p99 +2.30 ms"), "{s}");
    }

    #[test]
    fn burst_keys_are_compared_with_the_first_burst_arm_alone() {
        let row = |arm: &str, burst: &str, input: f64| -> Row {
            [("arm", arm), ("burst", burst)]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .chain([("input_ms".to_string(), input.to_string())])
                .collect()
        };
        let rows = [
            row("A", "0", 1.0),
            row("Ab", "2", 5.0),
            row("R", "0", 2.0),
            row("Rb", "3", 7.0),
        ];
        let s = summarize(&rows);
        assert!(s.contains("R - A:"), "{s}");
        assert!(s.contains("Rb - Ab:"), "{s}");
        assert!(
            s.contains("input p50 +2.00 ms"),
            "Rb's 7 against Ab's 5: {s}"
        );
        assert!(
            !s.contains("Ab - A:") && !s.contains("Rb - A:"),
            "a burst key is never set against a first key: {s}"
        );
    }

    #[test]
    fn a_reply_s_fields_are_read_by_key() {
        let f = fields("OK metrics n_input=1 last_input_present_ms=2.10 note");
        assert_eq!(num(&f, "n_input"), Some(1.0));
        assert_eq!(num(&f, "last_input_present_ms"), Some(2.1));
        assert_eq!(num(&f, "note"), None);
    }
}
