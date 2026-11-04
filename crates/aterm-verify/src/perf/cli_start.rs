// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `cli-start` floor lane: the instructions one aterm process start retires, and how much of
//! them loading the dylibs aterm links costs.
//!
//! Every agent's `aterm ctl` call, and every bridge, broker and notifier relaunch, is a fresh
//! start of the one shipped executable. The lane times none of them by the clock: it counts what
//! the kernel's per-thread counters say each start retired. Each launch is spawned through
//! [`aterm_uds::spawnfd::command`], observed exited without being reaped (`waitid` with
//! `WNOWAIT`, [`aterm_uds::spawnfd::Child::exited`], woken by the kernel's exit report), read
//! while the zombie still holds its usage (`proc_pid_rusage`'s `RUSAGE_INFO_V4`:
//! `ri_instructions`, `ri_cycles`, and its user and system time), and only then reaped.
//! Instructions retired move far less with the machine's load than time does, so the lane takes
//! no quiet claim and reports the load it met beside its numbers, and the wall time from spawn to
//! exit, which moves with it.
//!
//! The arms, each launched `--samples` times (default 50), every round of them in turn and the
//! order reversed every other round (A B B A), so a drift falls on every arm alike:
//!
//! - `version`: `aterm --version`;
//! - `ctl-help`: `aterm ctl --help`;
//! - `ctl-status`: `aterm ctl --sock <sock> @<sid> status` against a headless `aterm-lab
//!   instance` of the same aterm, its first session;
//! - the controls, the programs of `aterm-start-controls`, each an empty `main`:
//!   `start-control-none` links nothing beyond what every Rust program links;
//!   `start-control-all` links every dylib aterm's load commands name; `start-control-cli` all of
//!   those but the window-only frameworks ([`WINDOW_ONLY`]). The lane builds them at every run
//!   with the flags [`link_flag`] makes of aterm's load commands, so the controls follow what
//!   aterm links and nothing lists it by hand, and it holds each control's own load commands to
//!   what it was built to load ([`mislinked`]).
//!
//! The split of `aterm --version`: `none` is the trivial start any program pays; `all` over it
//! is loading aterm's dylibs; aterm over `all` is aterm's own start (its fixups, static
//! initializers and the verb); `all` over `cli` is what taking the window-only frameworks out of
//! the link would save a CLI start, which is less than what they cost alone, because the dylibs
//! `cli` keeps load most of them in turn. Every arm's one unmeasured probe launch runs with
//! `DYLD_PRINT_LIBRARIES`, and the report attributes the split by the images each one loaded:
//! the window-only frameworks `cli` loads all the same, the images `all` loads and `cli` does
//! not, and what aterm loads beyond `all`.
//!
//! The controls link; they do not insert. `DYLD_INSERT_LIBRARIES` naming aterm's dylibs into
//! the bare control loads the same 588 images as linking them, yet retired 39.03 M instructions
//! to the linked control's 29.53 M in one run (`docs/measured/cli-start-2026-11-04.md`).
//!
//! Judged: each aterm arm's median instructions against a ceiling, its committed baseline times
//! [`CEILING_RATIO`] (`perf-baseline-cli-start.json`, `--record cli-start` rewrites it). A gated
//! arm the run did not measure is NOT JUDGED, by name and why. The controls are the OS's costs
//! and are reported, never judged.

use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::trend::utc_date;
use super::{LaneRun, Status, TrendSample};

/// The lane's name: the CLI's, the trend ledger's and the baseline's.
pub(crate) const LANE: &str = "cli-start";

/// Launches per arm when `--samples` names none.
pub(crate) const DEFAULT_SAMPLES: usize = 50;

/// A gated arm passes while its median is at most its baseline times this. A start's
/// instructions vary by a few percent run to run; a quarter more is a new cost.
pub(crate) const CEILING_RATIO: f64 = 1.25;

/// The committed baseline's file under [`super::floors::golden_dir`].
const BASELINE_FILE: &str = "perf-baseline-cli-start.json";

/// The `_comment` a first recording writes.
const COMMENT: &str = "aterm CLI-START baseline: the median instructions retired, in millions, \
                       of one fresh aterm process per arm (`aterm --version`, `aterm ctl --help`, \
                       `aterm ctl --sock <headless lab instance> @<sid> status`), read by \
                       proc_pid_rusage (RUSAGE_INFO_V4) before the reap, release build. \
                       SMALLER-IS-BETTER: a median above baseline x ceiling_ratio fails. The \
                       lane runs at the load it finds and reports it. \
                       Re-record with targo --unverified verify perf --record cli-start.";

/// The package of the control programs.
const CONTROLS: &str = "aterm-start-controls";

/// The controls: each one's name and its program's.
const CONTROL_BINS: [(&str, &str); 3] = [
    ("none", "start-control-none"),
    ("all", "start-control-all"),
    ("cli", "start-control-cli"),
];

/// The frameworks only aterm's window uses, by short name: the `cli` control links every dylib
/// aterm links but these. They are what E08's step 2 would defer behind `dlopen`; the report
/// names those the `cli` control loads all the same, through the dylibs it keeps.
const WINDOW_ONLY: [&str; 8] = [
    "AppKit",
    "AudioToolbox",
    "Carbon",
    "CoreText",
    "CoreVideo",
    "ImageIO",
    "Metal",
    "QuartzCore",
];

/// The gated arms: each one's name and its key in the baseline.
const GATED: [(&str, &str); 3] = [
    ("version", "version_minstr"),
    ("ctl-help", "ctl_help_minstr"),
    ("ctl-status", "ctl_status_minstr"),
];

/// One arm's launches, as measured.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Takes {
    pub instructions: Vec<f64>,
    pub cycles: Vec<f64>,
    pub wall_ms: Vec<f64>,
    pub user_ms: Vec<f64>,
    pub system_ms: Vec<f64>,
}

/// One arm's summary: medians and the instruction range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Summary {
    pub n: usize,
    pub instructions: f64,
    pub min: f64,
    pub max: f64,
    pub cycles: f64,
    pub wall_ms: f64,
    pub user_ms: f64,
    pub system_ms: f64,
}

impl Takes {
    pub(crate) fn summary(&self) -> Option<Summary> {
        if self.instructions.is_empty() {
            return None;
        }
        let low = self
            .instructions
            .iter()
            .copied()
            .fold(f64::INFINITY, f64::min);
        let high = self
            .instructions
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        Some(Summary {
            n: self.instructions.len(),
            instructions: super::stats::median(&self.instructions),
            min: low,
            max: high,
            cycles: super::stats::median(&self.cycles),
            wall_ms: super::stats::median(&self.wall_ms),
            user_ms: super::stats::median(&self.user_ms),
            system_ms: super::stats::median(&self.system_ms),
        })
    }
}

/// A measured arm: its name, what it links (a control's own load commands; empty for aterm's
/// arms), the images its probe launch loaded (its main executable left out; none when dyld
/// printed none) and its summary.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Measured {
    pub name: String,
    pub loads: Vec<String>,
    pub images: Vec<String>,
    pub summary: Summary,
}

/// The split of one aterm start by the controls, in instructions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Split {
    pub start: f64,
    pub trivial: f64,
    pub frameworks: f64,
    pub deferral: f64,
    pub own: f64,
}

impl Split {
    /// `start`'s median split by the `none`, `all` and `cli` controls' medians.
    pub(crate) fn of(start: f64, none: f64, all: f64, cli: f64) -> Split {
        Split {
            start,
            trivial: none,
            frameworks: all - none,
            deferral: all - cli,
            own: start - all,
        }
    }

    /// `part` as a percentage of the start.
    pub(crate) fn share(&self, part: f64) -> f64 {
        if self.start > 0.0 {
            100.0 * part / self.start
        } else {
            0.0
        }
    }

    /// How many rows [`Split::rows`] gives.
    pub(crate) const ROWS: usize = 4;

    /// Its parts as the report prints them: each one's label and its instructions.
    pub(crate) fn rows(&self) -> [(&'static str, f64); Self::ROWS] {
        [
            ("trivial start (none)", self.trivial),
            ("frameworks (all - none)", self.frameworks),
            ("  window-only deferral (all - cli)", self.deferral),
            ("aterm's own (it - all)", self.own),
        ]
    }
}

/// A dylib's short name: its file name (`AppKit`, `libobjc.A.dylib`).
pub(crate) fn short(load: &str) -> &str {
    load.rsplit('/').next().unwrap_or(load)
}

/// What `paths` names beyond `base`, by short name, in `paths`' order.
pub(crate) fn beyond<'a>(paths: &'a [String], base: &[String]) -> Vec<&'a str> {
    paths
        .iter()
        .filter(|l| !base.contains(l))
        .map(|l| short(l))
        .collect()
}

/// The linker flag that makes a program load `dylib`, an install name from aterm's load
/// commands: `-needed_framework` for a framework of `/System/Library/Frameworks`, `-needed-l` for
/// a library of `/usr/lib`; why not, for any other.
pub(crate) fn link_flag(dylib: &str) -> Result<String, String> {
    let name = if let Some(rest) = dylib.strip_prefix("/System/Library/Frameworks/") {
        rest.split_once(".framework/")
            .map(|(name, _)| name)
            .filter(|name| !name.contains('/') && short(dylib) == *name)
            .map(|name| format!("-Wl,-needed_framework,{name}"))
    } else {
        dylib
            .strip_prefix("/usr/lib/lib")
            .filter(|file| !file.contains('/'))
            .and_then(|file| file.strip_suffix(".dylib"))
            .and_then(|stem| stem.split('.').next())
            .filter(|name| !name.is_empty())
            .map(|name| format!("-Wl,-needed-l{name}"))
    };
    name.ok_or_else(|| {
        format!(
            "no control can link {dylib}: it is neither a framework of /System/Library/Frameworks \
             nor a library of /usr/lib"
        )
    })
}

/// What the `cli` control links of `aterm`'s dylibs: all but [`WINDOW_ONLY`], in aterm's order.
pub(crate) fn cli_of(aterm: &[String]) -> Vec<String> {
    aterm
        .iter()
        .filter(|d| !WINDOW_ONLY.contains(&short(d)))
        .cloned()
        .collect()
}

/// The linker flags for `dylibs`, one each, joined as the controls' build script reads them.
pub(crate) fn link_flags(dylibs: &[String]) -> Result<String, String> {
    let flags = dylibs
        .iter()
        .map(|d| link_flag(d))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(flags.join(" "))
}

/// Why the control `name`, whose load commands name `got`, is not the program it was built to
/// be, one that loads `want` and what `none` loads: what it misses and what it adds, by short
/// name; `None` when it is that program.
pub(crate) fn mislinked(
    name: &str,
    got: &[String],
    want: &[String],
    none: &[String],
) -> Option<String> {
    let missing: Vec<&str> = want
        .iter()
        .filter(|d| !got.contains(d))
        .map(|d| short(d))
        .collect();
    let extra: Vec<&str> = got
        .iter()
        .filter(|d| !want.contains(d) && !none.contains(d))
        .map(|d| short(d))
        .collect();
    if missing.is_empty() && extra.is_empty() {
        return None;
    }
    Some(format!(
        "the `{name}` control is not the program it was built to be: it misses {} and adds {}, \
         so its split would be of another program",
        listed(&missing),
        listed(&extra)
    ))
}

/// The images a launch under `DYLD_PRINT_LIBRARIES` loaded, read from what dyld wrote to its
/// stderr (`dyld[<pid>]: <<uuid>> <path>`), in load order, the first (its main executable) left
/// out. Any other line is not dyld's and is passed over.
pub(crate) fn images_of(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("dyld[")?.split_once("]: ")?.1;
            let path = match rest.strip_prefix('<') {
                Some(tagged) => tagged.split_once("> ")?.1,
                None => rest,
            };
            path.starts_with('/').then(|| path.to_string())
        })
        .skip(1)
        .collect()
}

/// The sid of the first session a `ctl ls` reply lists: a row's third word (`<pid> <local>
/// <sid> …`).
pub(crate) fn sid_of_ls(listed: &str) -> Option<String> {
    listed
        .lines()
        .find_map(|line| line.split_whitespace().nth(2))
        .map(str::to_string)
}

/// Millions, as the report and the baseline write instructions.
fn m(v: f64) -> f64 {
    v / 1e6
}

/// `names` joined, or `none`.
fn listed(names: &[&str]) -> String {
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

/// The report: one row per arm, the split of each aterm arm and the images that attribute it.
pub(crate) fn report(arms: &[Measured]) -> String {
    let mut s = String::from(
        "    arm                  instr M  (min-max)          cycles M  user ms   sys ms  wall ms\n",
    );
    for a in arms {
        let x = a.summary;
        s.push_str(&format!(
            "    {:<20} {:>7.2}  ({:>6.2}-{:<6.2})    {:>7.2}  {:>7.2}  {:>7.2}  {:>7.2}\n",
            a.name,
            m(x.instructions),
            m(x.min),
            m(x.max),
            m(x.cycles),
            x.user_ms,
            x.system_ms,
            x.wall_ms
        ));
    }
    let find = |name: &str| arms.iter().find(|a| a.name == name);
    let (Some(none), Some(all), Some(cli)) = (find("none"), find("all"), find("cli")) else {
        return s;
    };
    let starts: Vec<&Measured> = GATED.iter().filter_map(|(arm, _)| find(arm)).collect();
    let splits: Vec<Split> = starts
        .iter()
        .map(|start| {
            Split::of(
                start.summary.instructions,
                none.summary.instructions,
                all.summary.instructions,
                cli.summary.instructions,
            )
        })
        .collect();
    s.push_str("    split, instr M and share of the start:  ");
    for start in &starts {
        s.push_str(&format!(" {:>15}", start.name));
    }
    s.push('\n');
    for row in 0..Split::ROWS {
        let label = splits.first().map_or("", |x| x.rows()[row].0);
        s.push_str(&format!("      {label:<36}"));
        for split in &splits {
            let v = split.rows()[row].1;
            s.push_str(&format!(" {:>7.2} {:>4.0}%  ", m(v), split.share(v)));
        }
        s.push('\n');
    }
    s.push_str(&format!(
        "      images at launch: none {}, all {}, cli {}\n",
        none.images.len(),
        all.images.len(),
        cli.images.len()
    ));
    let deferred: Vec<String> = all
        .loads
        .iter()
        .filter(|l| !cli.loads.contains(l))
        .cloned()
        .collect();
    let kept: Vec<String> = deferred
        .iter()
        .filter(|l| cli.images.contains(l))
        .cloned()
        .collect();
    s.push_str(&format!(
        "      window-only frameworks aterm links: {}\n",
        listed(&beyond(&deferred, &[]))
    ));
    s.push_str(&format!(
        "      of them, `cli` loads through the dylibs it keeps: {}\n",
        listed(&beyond(&kept, &[]))
    ));
    let dropped = beyond(&all.images, &cli.images);
    s.push_str(&format!(
        "      the {} images `all` loads and `cli` does not: {}\n",
        dropped.len(),
        listed(&dropped)
    ));
    for start in &starts {
        if start.images.is_empty() {
            s.push_str(&format!(
                "      {}: dyld printed none of its images (it ignores DYLD_* variables for a \
                 hardened or restricted program)\n",
                start.name
            ));
            continue;
        }
        s.push_str(&format!(
            "      {}: {} images; beyond `all`'s: {}",
            start.name,
            start.images.len(),
            listed(&beyond(&start.images, &all.images))
        ));
        let fewer = beyond(&all.images, &start.images);
        if !fewer.is_empty() {
            s.push_str(&format!(
                "; `all` loads and it does not: {}",
                listed(&fewer)
            ));
        }
        s.push('\n');
    }
    s
}

/// The gated medians, in millions, keyed as the baseline keys them.
pub(crate) fn gated(arms: &[Measured]) -> Vec<(&'static str, f64)> {
    GATED
        .iter()
        .filter_map(|(arm, key)| {
            arms.iter()
                .find(|a| a.name == *arm)
                .map(|a| (*key, m(a.summary.instructions)))
        })
        .collect()
}

/// The baseline file's text for `values`.
pub(crate) fn baseline_json(values: &[(&str, f64)], comment: &str) -> String {
    let mut s = format!(
        "{{\n  \"_comment\": {},\n",
        aterm_json::to_string(&Value::String(comment.to_string()))
            .unwrap_or_else(|_| "\"\"".to_string())
    );
    for (key, v) in values {
        s.push_str(&format!("  \"{key}\": {v:.3},\n"));
    }
    s.push_str(&format!("  \"ceiling_ratio\": {CEILING_RATIO:.3}\n}}\n"));
    s
}

/// Judge `values` against `baseline` (the committed file's JSON), key by key: a value above its
/// baseline times the file's `ceiling_ratio` fails; a key the baseline lacks is not judged.
pub(crate) fn judge(values: &[(&'static str, f64)], baseline: &Value) -> LaneRun {
    let ratio = super::floors::number(baseline, "ceiling_ratio").unwrap_or(CEILING_RATIO);
    let mut run = LaneRun::pass();
    let mut judged = 0;
    for (key, v) in values {
        match super::floors::number(baseline, key).filter(|b| *b > 0.0) {
            None => {
                eprintln!("    {key}: NOT JUDGED — the baseline has no positive {key}");
                run.unjudged.push(format!("{LANE}/{key} (no baseline)"));
            }
            Some(b) => {
                judged += 1;
                let ceiling = b * ratio;
                if *v <= ceiling {
                    eprintln!("    {key}: GREEN — {v:.2} M <= ceiling {ceiling:.2} M");
                } else {
                    eprintln!("    {key}: FAILED — {v:.2} M > ceiling {ceiling:.2} M");
                    run.add_failure(format!(
                        "{key} {v:.2} M > ceiling {ceiling:.2} M (baseline {b:.2} x {ratio:.2})"
                    ));
                }
            }
        }
    }
    if judged == 0 && run.status == Status::Pass {
        return LaneRun::not_judged("no gated arm has a baseline");
    }
    run
}

/// The refusal `--record` meets when the aterm measured is not this tree's own build (`built`
/// false: an aterm `--binary` names), whose baseline it would otherwise rewrite.
fn record_refused(record: bool, built: bool) -> Option<LaneRun> {
    (record && !built).then(|| {
        LaneRun::fail(
            "REFUSING to record an aterm --binary names: the baseline is of this tree's own \
             release build, which --record cli-start builds; drop --binary",
        )
    })
}

/// Record or judge the gated medians against `baseline` (the committed file's path), and hand
/// them to the trend (smaller is better) when the aterm measured is this tree's own build
/// (`built`): the trend keys its rows by the tree, so an aterm `--binary` names is judged but
/// feeds it nothing, and never records. A gated arm `arms` lacks is NOT JUDGED, named with the
/// reason `unmeasured` gives for it (`(arm, why)`), and a recording refuses it.
fn conclude(
    arms: &[Measured],
    unmeasured: &[(String, String)],
    record: bool,
    built: bool,
    baseline: &Path,
) -> LaneRun {
    if let Some(refused) = record_refused(record, built) {
        return refused;
    }
    let values = gated(arms);
    let missing: Vec<String> = GATED
        .iter()
        .filter(|(arm, _)| !arms.iter().any(|a| a.name == *arm))
        .map(|(arm, key)| {
            let why = unmeasured
                .iter()
                .find(|(name, _)| name == arm)
                .map_or("no launch of it was measured", |(_, why)| why.as_str());
            eprintln!("    {key}: NOT JUDGED — not measured: {why}");
            format!("{LANE}/{key} (not measured: {why})")
        })
        .collect();
    let previous = crate::read_regular_text(baseline)
        .ok()
        .and_then(|t| aterm_json::from_str::<Value>(t.trim()).ok())
        .filter(Value::is_object);
    let mut run = if record {
        if !missing.is_empty() {
            return LaneRun::fail(format!(
                "REFUSING to record — {}; {} left untouched",
                missing.join("; "),
                baseline.display()
            ));
        }
        let (comment, _) = super::floors::carried_comment(previous.as_ref(), COMMENT, &utc_date());
        match std::fs::write(baseline, baseline_json(&values, &comment)) {
            Ok(()) => {
                eprintln!("  RECORDED baseline -> {}", baseline.display());
                LaneRun {
                    status: Status::Recorded(baseline.to_path_buf()),
                    ..LaneRun::pass()
                }
            }
            Err(e) => LaneRun::fail(format!("could not write {}: {e}", baseline.display())),
        }
    } else {
        match previous {
            Some(base) => judge(&values, &base),
            None => {
                eprintln!(
                    "  {LANE}: no baseline at {} — judged against nothing (record with --record \
                     {LANE}).",
                    baseline.display()
                );
                LaneRun::not_judged("no baseline")
            }
        }
    };
    run.unjudged.extend(missing);
    if !built {
        eprintln!(
            "  {LANE}: the trend is not fed: --binary names an aterm this tree did not build"
        );
        return run;
    }
    run.samples = values
        .iter()
        .map(|(key, v)| TrendSample {
            lane: LANE,
            metric: (*key).to_string(),
            value: *v,
            inverted: true,
            at: Vec::new(),
        })
        .collect();
    run
}

/// The JSON `--json-out` writes: every arm's summary, what it links (a control's load commands)
/// and how many images it loaded, and the load the lane met.
fn json(arms: &[Measured], aterm: &Path, load: &str) -> Value {
    let mut rows = aterm_json::Map::new();
    for a in arms {
        let x = a.summary;
        let count = |n: usize| Value::from(u64::try_from(n).unwrap_or(u64::MAX));
        let mut row = aterm_json::Map::new();
        row.insert("n".into(), count(x.n));
        row.insert("instructions_median".into(), Value::from(x.instructions));
        row.insert("instructions_min".into(), Value::from(x.min));
        row.insert("instructions_max".into(), Value::from(x.max));
        row.insert("cycles_median".into(), Value::from(x.cycles));
        row.insert("wall_ms_median".into(), Value::from(x.wall_ms));
        row.insert("user_ms_median".into(), Value::from(x.user_ms));
        row.insert("system_ms_median".into(), Value::from(x.system_ms));
        row.insert(
            "loads".into(),
            Value::Array(a.loads.iter().map(|l| Value::from(l.as_str())).collect()),
        );
        row.insert("images".into(), count(a.images.len()));
        rows.insert(a.name.clone(), Value::Object(row));
    }
    let mut out = aterm_json::Map::new();
    out.insert("lane".into(), Value::from(LANE));
    out.insert("aterm".into(), Value::from(aterm.display().to_string()));
    out.insert("load_average".into(), Value::from(load));
    out.insert("arms".into(), Value::Object(rows));
    Value::Object(out)
}

/// What the lane was asked: the aterm to measure (a fresh release build when `None`), the
/// launches per arm, where its JSON goes.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Args {
    pub binary: Option<PathBuf>,
    pub samples: Option<usize>,
    pub json_out: Option<PathBuf>,
}

/// What one run measured: every arm, the aterm it launched, the load average the launches met,
/// and each gated arm it could not measure, with why (`(arm, why)`).
struct Measurement {
    arms: Vec<Measured>,
    aterm: PathBuf,
    load: String,
    unmeasured: Vec<(String, String)>,
}

/// The lane (macOS only: the perf module mounts it there and skips it elsewhere).
pub(crate) fn run(args: &Args, record: bool) -> LaneRun {
    let built = args.binary.is_none();
    if let Some(refused) = record_refused(record, built) {
        return refused;
    }
    match live::measure(args) {
        Ok(Measurement {
            arms,
            aterm,
            load,
            unmeasured,
        }) => {
            eprint!("{}", report(&arms));
            if let Some(path) = &args.json_out {
                let text = json(&arms, &aterm, &load).to_string();
                if let Err(e) = std::fs::write(path, text + "\n") {
                    return LaneRun::fail(format!("could not write {}: {e}", path.display()));
                }
                eprintln!("  json -> {}", path.display());
            }
            let baseline = super::floors::golden_dir().join(BASELINE_FILE);
            conclude(&arms, &unmeasured, record, built, &baseline)
        }
        Err(why) => why,
    }
}

mod live {
    //! The launches: the builds, the headless instance the `ctl-status` arm asks, and the
    //! counted spawns.

    use std::ffi::OsString;
    use std::io::BufRead as _;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use aterm_uds::deadline::ExitWait;
    use aterm_uds::spawnfd::{Child, Stdio, command};

    use super::super::arena::{self, without_aterm_env};
    use super::super::{LaneRun, driver, workspace_root};
    use super::{Args, CONTROL_BINS, CONTROLS, DEFAULT_SAMPLES, Measured, Takes};

    /// How long one launch may run before the lane calls it hung: a start takes milliseconds,
    /// and `ctl status` answers within its own default timeout.
    const HUNG: Duration = Duration::from_secs(60);

    /// How long `aterm-lab instance` may take to print its ready line: its own one-minute
    /// boot bound plus a one-minute hang detector.
    const INSTANCE_HUNG: Duration = Duration::from_secs(120);

    /// One arm: its name, the program and arguments it launches, and what it links (a
    /// control's own load commands; empty for aterm's arms).
    struct Arm {
        name: String,
        program: PathBuf,
        args: Vec<OsString>,
        loads: Vec<String>,
    }

    /// What one launch retired, and how long it took from spawn to its exit.
    struct Take {
        instructions: u64,
        cycles: u64,
        user: Duration,
        system: Duration,
        wall: Duration,
    }

    /// Build, probe every arm, launch each `samples` times, and summarize: the arms, the aterm
    /// measured, the load average the launches met and the gated arm it could not measure.
    pub(super) fn measure(args: &Args) -> Result<super::Measurement, LaneRun> {
        let aterm = match &args.binary {
            Some(path) => path.clone(),
            None => arena::build_aterm().map_err(LaneRun::fail)?.bin,
        };
        if !aterm.is_file() {
            return Err(LaneRun::fail(format!(
                "{} is not a file; build it, or name one with --binary",
                aterm.display()
            )));
        }
        let aterm_loads = loads(&aterm).map_err(LaneRun::fail)?;
        let controls = build_controls(&aterm_loads).map_err(LaneRun::fail)?;
        let mut arms = vec![
            Arm {
                name: "version".into(),
                program: aterm.clone(),
                args: vec!["--version".into()],
                loads: Vec::new(),
            },
            Arm {
                name: "ctl-help".into(),
                program: aterm.clone(),
                args: vec!["ctl".into(), "--help".into()],
                loads: Vec::new(),
            },
        ];
        let mut unmeasured = Vec::new();
        let instance = match Instance::start(&aterm) {
            Ok(instance) => {
                arms.push(Arm {
                    name: "ctl-status".into(),
                    program: aterm.clone(),
                    args: vec![
                        "ctl".into(),
                        "--sock".into(),
                        instance.sock.clone().into(),
                        format!("@{}", instance.sid).into(),
                        "status".into(),
                    ],
                    loads: Vec::new(),
                });
                Some(instance)
            }
            Err(why) => {
                let why = format!("the headless instance: {why}");
                eprintln!("  ctl-status: NOT MEASURED — {why}");
                unmeasured.push(("ctl-status".to_string(), why));
                None
            }
        };
        arms.extend(controls);
        let mut images = Vec::with_capacity(arms.len());
        for arm in &arms {
            images.push(probe(arm).map_err(LaneRun::fail)?);
        }
        let samples = args.samples.unwrap_or(DEFAULT_SAMPLES);
        let load_before = super::super::bench_build::load_average();
        eprintln!(
            "  {} arms x {samples} launches of {} and its controls, A B B A, at load {load_before}",
            arms.len(),
            aterm.display()
        );
        let mut takes: Vec<Takes> = arms.iter().map(|_| Takes::default()).collect();
        for round in 0..samples {
            let order: Vec<usize> = if round % 2 == 0 {
                (0..arms.len()).collect()
            } else {
                (0..arms.len()).rev().collect()
            };
            for i in order {
                let take = launch(&arms[i]).map_err(LaneRun::fail)?;
                takes[i].instructions.push(take.instructions as f64);
                takes[i].cycles.push(take.cycles as f64);
                takes[i].wall_ms.push(take.wall.as_secs_f64() * 1e3);
                takes[i].user_ms.push(take.user.as_secs_f64() * 1e3);
                takes[i].system_ms.push(take.system.as_secs_f64() * 1e3);
            }
        }
        let load_after = super::super::bench_build::load_average();
        drop(instance);
        let load = format!("{load_before} -> {load_after}");
        eprintln!("  load average: {load}");
        let measured = arms
            .into_iter()
            .zip(takes)
            .zip(images)
            .filter_map(|((arm, takes), images)| {
                takes.summary().map(|summary| Measured {
                    name: arm.name,
                    loads: arm.loads,
                    images,
                    summary,
                })
            })
            .collect();
        Ok(super::Measurement {
            arms: measured,
            aterm,
            load,
            unmeasured,
        })
    }

    /// The dylibs the Mach-O at `path` loads, in load-command order.
    fn loads(path: &Path) -> Result<Vec<String>, String> {
        atpkg::macho::load_names_at(path)
            .map(|names| names.once().loads)
            .map_err(|e| format!("cannot read the load commands of {}: {e}", path.display()))
    }

    /// `targo --unverified build --release -p aterm-start-controls`, its `all` control linking
    /// every dylib in `aterm` (aterm's load commands) and its `cli` control all but the
    /// window-only ones: each control's arm, held to its own load commands.
    fn build_controls(aterm: &[String]) -> Result<Vec<Arm>, String> {
        let cli = super::cli_of(aterm);
        let all_flags = super::link_flags(aterm)?;
        let cli_flags = super::link_flags(&cli)?;
        let messages = build(
            &[
                "--release",
                "-q",
                "-p",
                CONTROLS,
                "--message-format=json-render-diagnostics",
            ],
            &[
                ("ATERM_START_CONTROL_ALL", &all_flags),
                ("ATERM_START_CONTROL_CLI", &cli_flags),
            ],
        )?;
        let mut arms = Vec::new();
        for (name, bin) in CONTROL_BINS {
            let program = super::super::bench_build::built_executable(&messages, bin, "bin")?;
            arms.push(Arm {
                name: name.to_string(),
                loads: loads(&program)?,
                program,
                args: Vec::new(),
            });
        }
        let none = arms[0].loads.clone();
        for (arm, want) in arms.iter().zip([&none[..], aterm, &cli[..]]) {
            if let Some(why) = super::mislinked(&arm.name, &arm.loads, want, &none) {
                return Err(why);
            }
        }
        Ok(arms)
    }

    /// One `targo --unverified build` in the workspace, with `env` set for it: cargo's
    /// messages on success.
    fn build(args: &[&str], env: &[(&str, &str)]) -> Result<String, String> {
        let driver = driver::cargo_driver()?;
        eprintln!("  $ {}", driver.display_workspace("build", args));
        let mut cmd = driver.workspace_command("build");
        cmd.args(args)
            .current_dir(workspace_root())
            .stderr(Stdio::inherit());
        for (key, value) in env {
            cmd.env(key, value);
        }
        let out = cmd
            .output()
            .map_err(|e| format!("could not run targo: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(format!(
                "`targo build {}` exited {:?}",
                args.join(" "),
                out.status.code()
            ))
        }
    }

    /// A command for `arm` as every launch of it runs: no inherited `ATERM_*` override and no
    /// inherited `DYLD_*` variable.
    fn command_of(arm: &Arm) -> aterm_uds::spawnfd::Command {
        let mut cmd = command(&arm.program);
        cmd.args(&arm.args);
        without_aterm_env(&mut cmd);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("DYLD_") {
                cmd.env_remove(key);
            }
        }
        cmd
    }

    /// Launch `arm` once unmeasured, its output kept and `DYLD_PRINT_LIBRARIES` set: it must
    /// exit 0 (`ctl-status` must answer `OK`), the program's first exec is behind it before any
    /// launch is counted, and what it gives back is the images it loaded.
    fn probe(arm: &Arm) -> Result<Vec<String>, String> {
        let mut cmd = command_of(arm);
        cmd.env("DYLD_PRINT_LIBRARIES", "1");
        let out = aterm_uds::deadline::output_within(&mut cmd, HUNG)
            .map_err(|e| format!("{}: {e}", arm.name))?;
        let said = String::from_utf8_lossy(&out.stdout);
        let told = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() {
            let own: Vec<&str> = told.lines().filter(|l| !l.starts_with("dyld[")).collect();
            return Err(format!(
                "{} exited {:?}: {}",
                arm.name,
                out.status.code(),
                own.join("\n").trim_end()
            ));
        }
        if arm.name == "ctl-status" && !said.starts_with("OK") {
            return Err(format!(
                "ctl-status answered `{}`",
                said.lines().next().unwrap_or("")
            ));
        }
        Ok(super::images_of(&told))
    }

    /// Launch `arm` once: what it retired, read before the reap, and its spawn-to-exit time.
    fn launch(arm: &Arm) -> Result<Take, String> {
        let mut cmd = command_of(arm);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let started = Instant::now();
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("{}: could not spawn: {e}", arm.name))?;
        let exited = await_exit(&mut child);
        let wall = started.elapsed();
        if let Err(why) = exited {
            aterm_uds::deadline::kill_within(&mut child, HUNG);
            return Err(format!("{}: {why}", arm.name));
        }
        let usage = rusage(child.id());
        let status = child
            .wait()
            .map_err(|e| format!("{}: could not reap: {e}", arm.name))?;
        if !status.success() {
            return Err(format!("{} exited {status}", arm.name));
        }
        let usage = usage.ok_or_else(|| format!("{}: proc_pid_rusage named no usage", arm.name))?;
        Ok(Take {
            instructions: usage.ri_instructions,
            cycles: usage.ri_cycles,
            user: mach(usage.ri_user_time),
            system: mach(usage.ri_system_time),
            wall,
        })
    }

    /// Block until `child` has exited, leaving it unreaped, or say why not within [`HUNG`].
    fn await_exit(child: &mut Child) -> Result<(), String> {
        let hung = Instant::now() + HUNG;
        let mut exit = ExitWait::new(child);
        loop {
            if child.exited().map_err(|e| format!("waitid: {e}"))? {
                return Ok(());
            }
            let left = hung.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!("it did not exit in {HUNG:?}"));
            }
            exit.wait(left);
        }
    }

    /// A span of `rusage`'s Mach ticks (`ri_user_time`, `ri_system_time`), by the host's
    /// timebase; ticks as nanoseconds where the timebase cannot be read.
    fn mach(ticks: u64) -> Duration {
        static TIMEBASE: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();
        let (numer, denom) = *TIMEBASE.get_or_init(|| {
            let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
            // SAFETY: `info` is writable for the one struct the call fills.
            let rc = unsafe { libc::mach_timebase_info(&raw mut info) };
            if rc == 0 && info.numer > 0 && info.denom > 0 {
                (info.numer, info.denom)
            } else {
                (1, 1)
            }
        });
        let nanos = u128::from(ticks) * u128::from(numer) / u128::from(denom);
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }

    /// What `pid` (an exited, unreaped child) used, by `proc_pid_rusage`'s `RUSAGE_INFO_V4`.
    fn rusage(pid: u32) -> Option<libc::rusage_info_v4> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        // SAFETY: `info` is writable for one `rusage_info_v4`, the flavor asked for, and the
        // kernel keeps no pointer to it.
        let rc = unsafe {
            libc::proc_pid_rusage(
                pid,
                libc::RUSAGE_INFO_V4,
                info.as_mut_ptr().cast::<libc::rusage_info_t>(),
            )
        };
        // SAFETY: zeroed at creation, so initialized whatever the call did.
        (rc == 0).then(|| unsafe { info.assume_init() })
    }

    /// A headless `aterm-lab instance` of the aterm measured, held for the `ctl-status` arm and
    /// ended (`SIGTERM` to the lab, which ends its instance) on drop.
    struct Instance {
        lab: Child,
        sock: PathBuf,
        sid: String,
    }

    impl Instance {
        fn start(aterm: &Path) -> Result<Instance, String> {
            let messages = build(
                &[
                    "-q",
                    "-p",
                    "aterm-lab",
                    "--bin",
                    "aterm-lab",
                    "--message-format=json-render-diagnostics",
                ],
                &[],
            )?;
            let lab = super::super::bench_build::built_executable(&messages, "aterm-lab", "bin")?;
            let mut cmd = command(&lab);
            cmd.arg("instance")
                .arg("--aterm")
                .arg(aterm)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit());
            eprintln!("  $ {} instance --aterm {}", lab.display(), aterm.display());
            let mut child = cmd
                .spawn()
                .map_err(|e| format!("could not spawn aterm-lab: {e}"))?;
            let stdout = child
                .stdout
                .take()
                .ok_or("aterm-lab's stdout was not piped")?;
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
            let sock = match ready(&rx) {
                Ok(sock) => sock,
                Err(why) => {
                    aterm_uds::deadline::kill_within(&mut child, HUNG);
                    return Err(why);
                }
            };
            let mut instance = Instance {
                lab: child,
                sock,
                sid: String::new(),
            };
            instance.sid = first_session(aterm, &instance.sock)?;
            Ok(instance)
        }
    }

    /// The socket of the instance `aterm-lab instance` says is up (`instance pid=… sock=<path>
    /// …`), within [`INSTANCE_HUNG`].
    fn ready(lines: &mpsc::Receiver<String>) -> Result<PathBuf, String> {
        let hung = Instant::now() + INSTANCE_HUNG;
        loop {
            let left = hung.saturating_duration_since(Instant::now());
            match lines.recv_timeout(left) {
                Ok(line) => {
                    if let Some(sock) = line.strip_prefix("instance ").and_then(|rest| {
                        rest.split_whitespace()
                            .find_map(|w| w.strip_prefix("sock="))
                    }) {
                        return Ok(PathBuf::from(sock));
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(format!(
                        "aterm-lab instance said nothing ready in {INSTANCE_HUNG:?}"
                    ));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("aterm-lab instance exited before its instance was ready".into());
                }
            }
        }
    }

    /// The first session `ls` lists on the instance at `sock`: its sid, a row's third word
    /// (`<pid> <local> <sid> …`).
    fn first_session(aterm: &Path, sock: &Path) -> Result<String, String> {
        let listed = arena::ctl_out(aterm, sock, &["ls"])?;
        super::sid_of_ls(&listed)
            .ok_or_else(|| format!("`ctl ls` named no session: {}", listed.trim_end()))
    }

    impl Drop for Instance {
        fn drop(&mut self) {
            let _ = self.lab.terminate();
            let hung = Instant::now() + HUNG;
            let mut exit = ExitWait::new(&self.lab);
            while !self.lab.exited().unwrap_or(true) {
                let left = hung.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    eprintln!("  aterm-lab instance outlived its SIGTERM by {HUNG:?}; killing it");
                    aterm_uds::deadline::kill_within(&mut self.lab, HUNG);
                    return;
                }
                exit.wait(left);
            }
            let _ = self.lab.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arm(name: &str, instructions: f64, links: &[&str], images: &[&str]) -> Measured {
        Measured {
            name: name.to_string(),
            loads: owned(links),
            images: owned(images),
            summary: Summary {
                n: 1,
                instructions,
                min: instructions,
                max: instructions,
                cycles: instructions / 2.0,
                wall_ms: 1.0,
                user_ms: 1.0,
                system_ms: 1.0,
            },
        }
    }

    const SYS: &str = "/usr/lib/libSystem.B.dylib";
    const OBJC: &str = "/usr/lib/libobjc.A.dylib";
    const APPKIT: &str = "/System/Library/Frameworks/AppKit.framework/Versions/C/AppKit";
    const HITOOLBOX: &str = "/System/Library/Frameworks/Carbon.framework/Versions/A/Frameworks/\
                             HIToolbox.framework/Versions/A/HIToolbox";
    const CORETEXT: &str = "/System/Library/Frameworks/CoreText.framework/Versions/A/CoreText";
    const FOUNDATION: &str =
        "/System/Library/Frameworks/Foundation.framework/Versions/C/Foundation";

    /// The gated arms, each at `instructions`.
    fn starts(instructions: f64) -> Vec<Measured> {
        GATED
            .iter()
            .map(|(name, _)| arm(name, instructions, &[], &[]))
            .collect()
    }

    /// A scratch directory under the run's `TMPDIR`, removed on drop.
    fn scratch() -> aterm_tempfile::TempDir {
        aterm_tempfile::tempdir().expect("a scratch directory")
    }

    #[test]
    fn the_status_arm_asks_the_first_session_ls_lists() {
        let listed = "0 0 s-93b14b966d5e7c08438d - alive aterm meta=0\n\
                      0 1 s-0123456789abcdef0123 - alive aterm meta=0\n";
        assert_eq!(sid_of_ls(listed).as_deref(), Some("s-93b14b966d5e7c08438d"));
        assert_eq!(sid_of_ls(""), None);
        assert_eq!(sid_of_ls("0 0\n"), None);
    }

    #[test]
    fn the_split_takes_the_controls_from_the_start() {
        let split = Split::of(50e6, 15e6, 32e6, 29e6);
        assert_eq!(split.frameworks, 17e6);
        assert_eq!(split.deferral, 3e6);
        assert_eq!(split.own, 18e6);
        assert!((split.share(split.frameworks) - 34.0).abs() < 1e-9);
        assert_eq!(Split::of(0.0, 0.0, 0.0, 0.0).share(1.0), 0.0);
    }

    fn owned(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| (*p).to_string()).collect()
    }

    /// Each dylib aterm's load commands name becomes the flag that links it: a top-level system
    /// framework by name, a `/usr/lib` library by its stem; anything else is refused by name.
    #[test]
    fn a_dylib_is_linked_by_the_flag_its_install_name_gives() {
        assert_eq!(
            link_flag(APPKIT).as_deref(),
            Ok("-Wl,-needed_framework,AppKit")
        );
        assert_eq!(link_flag(OBJC).as_deref(), Ok("-Wl,-needed-lobjc"));
        assert_eq!(link_flag(SYS).as_deref(), Ok("-Wl,-needed-lSystem"));
        assert_eq!(
            link_flag("/usr/lib/libc++.1.dylib").as_deref(),
            Ok("-Wl,-needed-lc++")
        );
        for refused in [
            HITOOLBOX,
            "/usr/lib/swift/libswiftCore.dylib",
            "@rpath/libfoo.dylib",
            "/usr/lib/lib.dylib",
        ] {
            let why = link_flag(refused).expect_err(refused);
            assert!(why.contains(refused), "{why}");
        }
    }

    /// The controls come from aterm's load commands at every run: `all` links every dylib aterm
    /// does and `cli` all but the window-only frameworks, so a framework aterm starts or stops
    /// linking changes both.
    #[test]
    fn the_controls_follow_aterms_load_commands() {
        let aterm = owned(&[APPKIT, FOUNDATION, OBJC, CORETEXT, SYS]);
        assert_eq!(cli_of(&aterm), owned(&[FOUNDATION, OBJC, SYS]));
        assert_eq!(
            link_flags(&aterm).as_deref(),
            Ok(
                "-Wl,-needed_framework,AppKit -Wl,-needed_framework,Foundation -Wl,-needed-lobjc \
                 -Wl,-needed_framework,CoreText -Wl,-needed-lSystem"
            )
        );
        let security = "/System/Library/Frameworks/Security.framework/Versions/A/Security";
        let grown = owned(&[APPKIT, FOUNDATION, OBJC, CORETEXT, SYS, security]);
        assert_eq!(cli_of(&grown), owned(&[FOUNDATION, OBJC, SYS, security]));
        assert!(
            link_flags(&grown)
                .expect("flags")
                .ends_with(" -Wl,-needed_framework,Security")
        );
        assert!(link_flags(&owned(&[APPKIT, HITOOLBOX])).is_err());
    }

    /// A control whose own load commands are not what it was built to load (a flag the linker
    /// dropped, a build without the lane's flags) fails the lane, naming what it misses and adds;
    /// what the bare control loads is not counted against it.
    #[test]
    fn a_control_that_links_other_than_it_was_built_to_is_named() {
        let none = owned(&[SYS]);
        let want = owned(&[APPKIT, OBJC, SYS]);
        assert_eq!(
            mislinked("all", &owned(&[SYS, APPKIT, OBJC]), &want, &none),
            None
        );
        assert_eq!(mislinked("none", &none, &[], &none), None);
        let bare = mislinked("all", &none, &want, &none).expect("a finding");
        assert!(
            bare.contains("`all` control") && bare.contains("misses AppKit, libobjc.A.dylib"),
            "{bare}"
        );
        let more = mislinked("cli", &owned(&[SYS, APPKIT, OBJC, CORETEXT]), &want, &none)
            .expect("a finding");
        assert!(more.contains("misses none and adds CoreText"), "{more}");
    }

    #[test]
    fn dyld_s_printed_images_are_read_without_the_main_executable() {
        let told = "dyld[22789]: <9F0BFFE1-66F1-3089-90BD-8832CDA4568A> /x/target/release/ctl\n\
                    dyld[22789]: <4FDC9AA6-B344-37FE-B8C6-A4C94A038F57> /usr/lib/libSystem.B.dylib\n\
                    a line of the program's own\n\
                    dyld[22789]: /usr/lib/libobjc.A.dylib\n";
        assert_eq!(images_of(told), [SYS, OBJC]);
        assert_eq!(images_of("no dyld here\n"), Vec::<String>::new());
    }

    #[test]
    fn a_median_above_its_ceiling_fails_and_a_missing_key_is_unjudged() {
        let base = aterm_json::from_str::<Value>(&baseline_json(
            &[("version_minstr", 40.0), ("ctl_help_minstr", 40.0)],
            "c",
        ))
        .expect("the baseline parses");
        let green = judge(
            &[("version_minstr", 50.0), ("ctl_help_minstr", 41.0)],
            &base,
        );
        assert_eq!(green.status, Status::Pass);
        let red = judge(&[("version_minstr", 50.1)], &base);
        assert!(red.failed(), "{red:?}");
        let half = judge(
            &[("version_minstr", 41.0), ("ctl_status_minstr", 9.0)],
            &base,
        );
        assert_eq!(half.status, Status::Pass);
        assert_eq!(half.unjudged, ["cli-start/ctl_status_minstr (no baseline)"]);
        let none = judge(&[("ctl_status_minstr", 9.0)], &base);
        assert!(matches!(none.status, Status::NotJudged(_)), "{none:?}");
    }

    #[test]
    fn the_gated_medians_are_in_millions_and_only_the_aterm_arms() {
        let arms = vec![
            arm("version", 54e6, &[], &[]),
            arm("none", 15e6, &[], &[SYS]),
            arm("ctl-status", 60e6, &[], &[]),
        ];
        assert_eq!(
            gated(&arms),
            vec![("version_minstr", 54.0), ("ctl_status_minstr", 60.0)]
        );
        let text = report(&arms);
        assert!(text.contains("version"), "{text}");
        assert!(
            !text.contains("split"),
            "no split without the controls: {text}"
        );
    }

    /// A gated arm the run could not measure (the headless instance never came up) is NOT
    /// JUDGED by name and why, so the verdict cannot say every lane was judged; the arms it
    /// measured are still judged and trended, and a recording refuses to write a baseline
    /// without it.
    #[test]
    fn a_gated_arm_not_measured_is_named_not_judged() {
        let dir = scratch();
        let baseline = dir.path().join(BASELINE_FILE);
        let three = [
            ("version_minstr", 50.0),
            ("ctl_help_minstr", 50.0),
            ("ctl_status_minstr", 50.0),
        ];
        let written = baseline_json(&three, "c");
        std::fs::write(&baseline, &written).expect("the baseline is written");
        let measured: Vec<Measured> = starts(50e6)
            .into_iter()
            .filter(|a| a.name != "ctl-status")
            .collect();
        let why = vec![(
            "ctl-status".to_string(),
            "the headless instance: aterm-lab instance said nothing ready".to_string(),
        )];
        let run = conclude(&measured, &why, false, true, &baseline);
        assert_eq!(run.status, Status::Pass, "{run:?}");
        assert_eq!(
            run.unjudged,
            [
                "cli-start/ctl_status_minstr (not measured: the headless instance: aterm-lab \
                 instance said nothing ready)"
            ]
        );
        assert_eq!(run.samples.len(), 2, "{run:?}");
        let (text, _) = super::super::verdict(&[(LANE, &run, 1.0)]);
        assert!(!text.contains("GREEN — every lane"), "{text}");
        assert!(
            text.contains("NOT JUDGED: cli-start/ctl_status_minstr (not measured"),
            "{text}"
        );
        let unexplained = conclude(&measured, &[], false, true, &baseline);
        assert_eq!(
            unexplained.unjudged,
            ["cli-start/ctl_status_minstr (not measured: no launch of it was measured)"]
        );
        let refused = conclude(&measured, &why, true, true, &baseline);
        assert!(refused.failed(), "{refused:?}");
        assert_eq!(
            std::fs::read_to_string(&baseline).expect("the baseline reads"),
            written
        );
        let whole = conclude(&starts(50e6), &[], false, true, &baseline);
        assert!(whole.unjudged.is_empty(), "{whole:?}");
    }

    /// An aterm `--binary` names is judged but never feeds the trend (whose rows are keyed by
    /// this tree), and a recording of it refuses before it writes; this tree's own build
    /// records.
    #[test]
    fn only_this_trees_own_build_feeds_the_trend_or_records() {
        let dir = scratch();
        let baseline = dir.path().join(BASELINE_FILE);
        let own = conclude(&starts(50e6), &[], false, true, &baseline);
        assert_eq!(own.samples.len(), 3, "{own:?}");
        assert!(own.samples.iter().all(|s| s.inverted && s.lane == LANE));
        let named = conclude(&starts(50e6), &[], false, false, &baseline);
        assert!(named.samples.is_empty(), "{named:?}");
        assert_eq!(named.status, own.status);
        let refused = conclude(&starts(50e6), &[], true, false, &baseline);
        match &refused.status {
            Status::Fail(why) => assert!(why[0].contains("REFUSING to record"), "{why:?}"),
            other => panic!("a --binary aterm was recorded: {other:?}"),
        }
        assert!(
            !baseline.exists(),
            "the refusal wrote {}",
            baseline.display()
        );
        let recorded = conclude(&starts(50e6), &[], true, true, &baseline);
        assert_eq!(recorded.status, Status::Recorded(baseline.clone()));
        assert!(baseline.is_file());
    }

    #[test]
    fn the_report_splits_each_start_and_attributes_it_by_the_images() {
        let arms = vec![
            arm(
                "version",
                50e6,
                &[],
                &[SYS, FOUNDATION, CORETEXT, APPKIT, HITOOLBOX, OBJC],
            ),
            arm("none", 15e6, &[], &[SYS]),
            arm(
                "all",
                32e6,
                &[APPKIT, FOUNDATION, CORETEXT],
                &[SYS, APPKIT, HITOOLBOX, FOUNDATION, CORETEXT],
            ),
            arm("cli", 29e6, &[FOUNDATION], &[SYS, FOUNDATION, CORETEXT]),
        ];
        let text = report(&arms);
        let row = |label: &str| {
            text.lines()
                .find(|l| l.trim_start().starts_with(label))
                .unwrap_or_else(|| panic!("no `{label}` row: {text}"))
                .split_whitespace()
                .rev()
                .take(2)
                .collect::<Vec<_>>()
        };
        assert_eq!(row("frameworks (all - none)"), ["34%", "17.00"]);
        assert_eq!(row("window-only deferral (all - cli)"), ["6%", "3.00"]);
        assert_eq!(row("aterm's own (it - all)"), ["36%", "18.00"]);
        assert!(
            text.contains("images at launch: none 1, all 5, cli 3\n"),
            "{text}"
        );
        assert!(
            text.contains("window-only frameworks aterm links: AppKit, CoreText\n"),
            "{text}"
        );
        assert!(
            text.contains("of them, `cli` loads through the dylibs it keeps: CoreText\n"),
            "{text}"
        );
        assert!(
            text.contains("the 2 images `all` loads and `cli` does not: AppKit, HIToolbox\n"),
            "{text}"
        );
        assert!(
            text.contains("version: 6 images; beyond `all`'s: libobjc.A.dylib\n"),
            "{text}"
        );
    }

    #[test]
    fn a_summary_is_the_median_and_the_range() {
        let takes = Takes {
            instructions: vec![3.0, 1.0, 2.0],
            cycles: vec![1.0, 1.0, 4.0],
            wall_ms: vec![0.5, 0.7, 0.6],
            user_ms: vec![0.2, 0.1, 0.3],
            system_ms: vec![0.4, 0.4, 0.4],
        };
        let s = takes.summary().expect("a summary");
        assert_eq!(
            (s.n, s.instructions, s.min, s.max, s.cycles),
            (3, 2.0, 1.0, 3.0, 1.0)
        );
        assert_eq!((s.user_ms, s.system_ms), (0.2, 0.4));
        assert_eq!(Takes::default().summary(), None);
    }
}
