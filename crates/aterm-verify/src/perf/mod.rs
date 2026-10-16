// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify perf` — aterm's performance lanes, each runnable alone or in any
//! combination:
//!
//! ```text
//! targo --unverified verify perf [LANE...] [--record LANE...] [OPTIONS]
//! targo --unverified verify perf --list
//! ```
//!
//! Two kinds of lane. The FLOOR lanes ([`floors`], [`wasm`]) measure the
//! engine headless and judge each metric against its committed baseline in
//! `crates/aterm-verify/perf/golden/`; whatever they measured then meets the
//! same-box trend ledger ([`trend`]), and a green run appends to it. A bare `verify
//! perf` runs every floor lane. The ARENA lanes ([`arena`]) open real
//! windows and take minutes; they run only when named, and what they measure
//! has no floor, so they report MEASURED, never a pass.
//!
//! Nothing runs this automatically: it measures when someone asks it to.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

#[cfg(unix)]
pub(crate) mod arena;
mod driver;
mod floors;
#[cfg(unix)]
mod stats;
mod trend;
pub(crate) mod wasm;

pub(crate) use trend::TrendSample;

/// The hidden first argument that runs the arena lanes' in-terminal half
/// (`aterm-verify --arena-inner <verb> …`): the lanes launch this binary
/// inside each terminal under test.
pub const ARENA_INNER_FLAG: &str = "--arena-inner";

/// The in-terminal half's program, answering its exit code.
#[cfg(unix)]
#[must_use]
pub fn arena_inner(args: &[String]) -> i32 {
    exit_code(arena::inner::main(args))
}

/// `verify perf [LANE…] [--record LANE…] [--list]`, answering its exit code.
#[must_use]
pub fn run(args: &[String]) -> i32 {
    exit_code(main(args))
}

fn exit_code(code: ExitCode) -> i32 {
    if code == ExitCode::SUCCESS {
        crate::exit::PASS
    } else if code == ExitCode::from(2) {
        crate::exit::USAGE
    } else {
        crate::exit::FAILED
    }
}

/// The checkout this binary was built from: the lanes build, measure and
/// record against its tree.
pub(crate) fn workspace_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| manifest.to_path_buf(), Path::to_path_buf)
}

/// Every lane, in the order a run executes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Lane {
    Throughput,
    Pathological,
    ScrollScrub,
    Search,
    Restore,
    Resize,
    Zstd,
    Wasm,
    Start,
    Compare,
    Cat,
    Scroll,
    Vte,
}

/// Which kind of lane, and so whether a bare run includes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Headless, judged against a committed baseline; the default set.
    Floor,
    /// A real-window head-to-head; run only when named.
    Arena,
}

impl Lane {
    pub(crate) const ALL: [Lane; 13] = [
        Lane::Throughput,
        Lane::Pathological,
        Lane::ScrollScrub,
        Lane::Search,
        Lane::Restore,
        Lane::Resize,
        Lane::Zstd,
        Lane::Wasm,
        Lane::Start,
        Lane::Compare,
        Lane::Cat,
        Lane::Scroll,
        Lane::Vte,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Lane::Throughput => "throughput",
            Lane::Pathological => "pathological",
            Lane::ScrollScrub => "scroll-scrub",
            Lane::Search => "search",
            Lane::Restore => "restore",
            Lane::Resize => "resize",
            Lane::Zstd => "zstd",
            Lane::Wasm => "wasm",
            Lane::Start => "start",
            Lane::Compare => "compare",
            Lane::Cat => "cat",
            Lane::Scroll => "scroll",
            Lane::Vte => "vte",
        }
    }

    pub(crate) fn kind(self) -> Kind {
        match self {
            Lane::Throughput
            | Lane::Pathological
            | Lane::ScrollScrub
            | Lane::Search
            | Lane::Restore
            | Lane::Resize
            | Lane::Zstd
            | Lane::Wasm => Kind::Floor,
            Lane::Start | Lane::Compare | Lane::Cat | Lane::Scroll | Lane::Vte => Kind::Arena,
        }
    }

    /// What the lane measures, for `--list`.
    fn what(self) -> &'static str {
        match self {
            Lane::Throughput => "Terminal::process MB/s over a ~32 MiB mixed VT workload",
            Lane::Pathological => "per-corpus MB/s under hostile input (floors per corpus)",
            Lane::ScrollScrub => "scrollback scrub read-path rates over a 100k-line tiered fill",
            Lane::Search => "search index build, cached query and retained memory",
            Lane::Restore => "serialize -> fresh engine -> replay rate",
            Lane::Resize => "resize/rewrap rates plus the absolute 42s-freeze fences",
            Lane::Zstd => {
                "aterm-zstd decode MB/s over recorded zstd CLI frames (atpkg's installer)"
            }
            Lane::Wasm => "the shipped wasm modules under node, plus the module-size ceiling",
            Lane::Start => "fresh-process real-window startup timing of one aterm",
            Lane::Compare => "paired ABBA startup A/B of two aterm binaries",
            Lane::Cat => "cat 150 MB ascii + unicode, aterm vs the pinned ghostty tip",
            Lane::Scroll => "windowed 100k-line scrub + jump-to-top frame pacing",
            Lane::Vte => "vtebench's default suite, aterm vs the pinned ghostty tip",
        }
    }

    /// What the box needs, for `--list`.
    fn needs(self) -> &'static str {
        match self {
            Lane::Throughput
            | Lane::Pathological
            | Lane::ScrollScrub
            | Lane::Search
            | Lane::Restore
            | Lane::Resize
            | Lane::Zstd => "targo (release build of aterm-bench)",
            Lane::Wasm => {
                "node, rustup stable + wasm32-unknown-unknown, wasm-opt, wasm-bindgen (pinned)"
            }
            Lane::Start => "macOS, a built aterm (--binary)",
            Lane::Compare => "macOS, two built aterm binaries (--baseline, --candidate)",
            Lane::Cat => "macOS on the M4-class bench box (or --dev-smoke), curl",
            Lane::Vte => "macOS on the M4-class bench box (or --dev-smoke), curl, git",
            Lane::Scroll => "macOS on the M4-class bench box (or --dev-smoke)",
        }
    }

    fn from_name(name: &str) -> Option<Lane> {
        Lane::ALL.into_iter().find(|l| l.name() == name)
    }
}

/// What one lane concluded.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Status {
    /// Everything it judged cleared (see [`LaneRun::unjudged`] for what it
    /// measured but could not judge).
    Pass,
    /// It measured, but nothing it measured could be judged: why (no
    /// baseline, a seed run).
    NotJudged(String),
    /// Its baseline was rewritten from this run; nothing was compared.
    Recorded(PathBuf),
    /// It measured and reported, with no floor to judge against: where the
    /// numbers went. Only the arena lanes measure this way, and they are unix.
    #[cfg_attr(not(unix), allow(dead_code))]
    Measured(String),
    /// It could not measure on this box: why, and what to do about it.
    Skip { why: String, fix: String },
    /// A floor, fence or validation failed.
    Fail(Vec<String>),
}

/// One lane's typed result.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LaneRun {
    pub status: Status,
    /// What it measured without being able to judge, and why (no baseline,
    /// a non-positive floor, a seed run).
    pub unjudged: Vec<String>,
    /// Medians headed for the same-box trend ledger.
    pub samples: Vec<TrendSample>,
}

impl LaneRun {
    pub(crate) fn pass() -> Self {
        Self {
            status: Status::Pass,
            unjudged: Vec::new(),
            samples: Vec::new(),
        }
    }

    pub(crate) fn fail(finding: impl Into<String>) -> Self {
        Self {
            status: Status::Fail(vec![finding.into()]),
            ..Self::pass()
        }
    }

    pub(crate) fn not_judged(why: impl Into<String>) -> Self {
        Self {
            status: Status::NotJudged(why.into()),
            ..Self::pass()
        }
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn measured(at: impl Into<String>) -> Self {
        Self {
            status: Status::Measured(at.into()),
            ..Self::pass()
        }
    }

    pub(crate) fn skip(why: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            status: Status::Skip {
                why: why.into(),
                fix: fix.into(),
            },
            ..Self::pass()
        }
    }

    pub(crate) fn failed(&self) -> bool {
        matches!(self.status, Status::Fail(_))
    }

    /// An absolute fence of `lane` held, so the lane judged something even
    /// when its ratio floors had nothing to judge against.
    pub(crate) fn fence_held(&mut self, lane: &str) {
        if let Status::NotJudged(why) = &self.status {
            self.unjudged.push(format!("{lane} floors ({why})"));
            self.status = Status::Pass;
        }
    }

    /// Fold `finding` in: the run fails, keeping any findings it had.
    pub(crate) fn add_failure(&mut self, finding: impl Into<String>) {
        match &mut self.status {
            Status::Fail(findings) => findings.push(finding.into()),
            _ => self.status = Status::Fail(vec![finding.into()]),
        }
    }
}

/// One option: its flag, its value's name (`None` for a switch), and the
/// lanes that read it. An option given without any of its lanes is a usage
/// error, never silently ignored.
struct OptSpec {
    flag: &'static str,
    value: Option<&'static str>,
    lanes: &'static [Lane],
    help: &'static str,
}

const START_LANES: &[Lane] = &[Lane::Start, Lane::Compare];
const GHOSTTY_LANES: &[Lane] = &[Lane::Cat, Lane::Vte];
const BENCH_BOX_LANES: &[Lane] = &[Lane::Cat, Lane::Scroll, Lane::Vte];

const OPTIONS: &[OptSpec] = &[
    OptSpec {
        flag: "--binary",
        value: Some("PATH"),
        lanes: &[Lane::Start],
        help: "the aterm to time (default: target/profiling/aterm, else target/release/aterm)",
    },
    OptSpec {
        flag: "--baseline",
        value: Some("PATH"),
        lanes: &[Lane::Compare],
        help: "the A arm's aterm (required)",
    },
    OptSpec {
        flag: "--candidate",
        value: Some("PATH"),
        lanes: &[Lane::Compare],
        help: "the B arm's aterm (required)",
    },
    OptSpec {
        flag: "--samples",
        value: Some("N"),
        lanes: &[Lane::Start],
        help: "independent launches (default 7)",
    },
    OptSpec {
        flag: "--pairs",
        value: Some("N"),
        lanes: &[Lane::Compare],
        help: "AB/BA pairs; two pairs make one reported ABBA round (default 20)",
    },
    OptSpec {
        flag: "--backend",
        value: Some("auto|cpu|gpu"),
        lanes: START_LANES,
        help: "the renderer to request (default auto)",
    },
    OptSpec {
        flag: "--settle-ms",
        value: Some("N"),
        lanes: START_LANES,
        help: "delay after the socket appears before the one metrics read (default 1500)",
    },
    OptSpec {
        flag: "--timeout-s",
        value: Some("N"),
        lanes: START_LANES,
        help: "per-launch socket and query timeout (default 15)",
    },
    OptSpec {
        flag: "--cooldown-s",
        value: Some("SECS"),
        lanes: &[Lane::Compare],
        help: "seconds between trials (default 5; shorter is dev-only)",
    },
    OptSpec {
        flag: "--json-out",
        value: Some("PATH"),
        lanes: START_LANES,
        help: "write every raw sample and the summary as JSON",
    },
    OptSpec {
        flag: "--allow-identical",
        value: None,
        lanes: &[Lane::Compare],
        help: "accept equal hashes (an identical-binary negative control)",
    },
    OptSpec {
        flag: "--allow-legacy-metric",
        value: None,
        lanes: START_LANES,
        help: "accept a binary predating the router/phase/attach startup schemas",
    },
    OptSpec {
        flag: "--warm",
        value: None,
        lanes: START_LANES,
        help: "measure the second launch into a home a discarded launch primed",
    },
    OptSpec {
        flag: "--dev-smoke",
        value: None,
        lanes: BENCH_BOX_LANES,
        help: "run off the M4-class bench box, labelled non-publishable",
    },
    OptSpec {
        flag: "--dmg",
        value: Some("PATH"),
        lanes: GHOSTTY_LANES,
        help: "a local copy of the pinned ghostty tip dmg",
    },
    OptSpec {
        flag: "--trials",
        value: Some("N"),
        lanes: GHOSTTY_LANES,
        help: "interleaved trials per terminal (default: cat 5, vte 3)",
    },
    OptSpec {
        flag: "--fill-lines",
        value: Some("N"),
        lanes: &[Lane::Scroll],
        help: "scrollback fill depth (default 120000)",
    },
    OptSpec {
        flag: "--scrub-steps",
        value: Some("N"),
        lanes: &[Lane::Scroll],
        help: "wheel notches in the scrub sweep (default 250)",
    },
    OptSpec {
        flag: "--columns",
        value: Some("N"),
        lanes: &[Lane::Vte],
        help: "grid columns both terminals run at (default 120)",
    },
    OptSpec {
        flag: "--rows",
        value: Some("N"),
        lanes: &[Lane::Vte],
        help: "grid rows both terminals run at (default 40)",
    },
    OptSpec {
        flag: "--max-secs",
        value: Some("N"),
        lanes: &[Lane::Vte],
        help: "vtebench's per-benchmark sampling time (default 10)",
    },
];

/// A parsed command line.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Cli {
    pub lanes: Vec<Lane>,
    pub record: Vec<Lane>,
    pub list: bool,
    pub help: bool,
    /// Every option given, by flag (switches hold an empty value).
    pub options: BTreeMap<&'static str, String>,
}

/// Parse `perf`'s arguments. Pure (unit-tested).
pub(crate) fn parse(args: &[String]) -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut recording = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--list" => cli.list = true,
            "-h" | "--help" => cli.help = true,
            "--record" => recording = true,
            flag if flag.starts_with('-') => {
                let spec = OPTIONS
                    .iter()
                    .find(|o| o.flag == flag)
                    .ok_or_else(|| format!("unknown option `{flag}`"))?;
                let value = match spec.value {
                    Some(meta) => it
                        .next()
                        .filter(|v| !v.starts_with("--"))
                        .ok_or_else(|| format!("`{flag}` needs {meta}"))?
                        .clone(),
                    None => String::new(),
                };
                if cli.options.insert(spec.flag, value).is_some() {
                    return Err(format!("`{flag}` given twice"));
                }
            }
            name => {
                let lane = Lane::from_name(name).ok_or_else(|| {
                    format!("unknown lane `{name}` (`verify perf --list` names them)")
                })?;
                // An arena lane has no baseline: after `--record` it just runs.
                if recording && lane.kind() == Kind::Floor {
                    cli.record.push(lane);
                }
                cli.lanes.push(lane);
            }
        }
    }
    if recording && cli.record.is_empty() {
        return Err(
            "`--record` names the floor lanes whose baselines this run rewrites, e.g. \
             `--record restore`"
                .to_string(),
        );
    }
    if cli.lanes.is_empty() {
        cli.lanes = Lane::ALL
            .into_iter()
            .filter(|l| l.kind() == Kind::Floor)
            .collect();
    }
    cli.lanes.sort_unstable();
    cli.lanes.dedup();
    cli.record.sort_unstable();
    cli.record.dedup();
    if cli.lanes.contains(&Lane::Compare)
        && !(cli.options.contains_key("--baseline") && cli.options.contains_key("--candidate"))
    {
        return Err("`compare` needs --baseline PATH and --candidate PATH".to_string());
    }
    for flag in cli.options.keys() {
        let spec = OPTIONS
            .iter()
            .find(|o| o.flag == *flag)
            .expect("parsed from OPTIONS");
        if !spec.lanes.iter().any(|l| cli.lanes.contains(l)) {
            let names: Vec<&str> = spec.lanes.iter().map(|l| l.name()).collect();
            return Err(format!(
                "`{flag}` is read only by {}; name one of them",
                names.join(", ")
            ));
        }
    }
    Ok(cli)
}

fn usage() -> String {
    let mut s = String::from(
        "usage: targo --unverified verify perf [LANE...] [--record LANE...] [OPTIONS]\n\
         \x20      targo --unverified verify perf --list\n\
         \n\
         No LANE runs every floor lane, then the same-box trend; a green run appends its\n\
         medians to crates/aterm-verify/perf/golden/perf-trend.tsv (commit it: that is the\n\
         history the trend reads). `--record` rewrites, from this run, the baseline of every floor lane\n\
         named after it; an arena lane named there just runs.\n\
         Exit: 0 nothing failed (a SKIP names its fix) · 1 a lane failed · 2 usage.\n\
         \n\
         Options:\n",
    );
    for o in OPTIONS {
        let flag = match o.value {
            Some(v) => format!("{} {v}", o.flag),
            None => o.flag.to_string(),
        };
        let lanes: Vec<&str> = o.lanes.iter().map(|l| l.name()).collect();
        s.push_str(&format!("  {flag:<26} [{}] {}\n", lanes.join(","), o.help));
    }
    s
}

fn list() -> String {
    let mut s = String::new();
    for kind in [Kind::Floor, Kind::Arena] {
        s.push_str(match kind {
            Kind::Floor => {
                "FLOOR lanes (the default set; judged against crates/aterm-verify/perf/golden, \
                 then the same-box trend, which a green run appends to)\n"
            }
            Kind::Arena => {
                "ARENA lanes (only when named; real windows, minutes; measured, not judged: \
                 cat, scroll and vte write a docs/measured/arena entry, start and compare \
                 print a summary, and JSON with --json-out)\n"
            }
        });
        for lane in Lane::ALL.into_iter().filter(|l| l.kind() == kind) {
            s.push_str(&format!(
                "  {:<13} {}\n  {:<13}   needs: {}\n",
                lane.name(),
                lane.what(),
                "",
                lane.needs()
            ));
        }
    }
    s
}

/// The `verify perf` verb.
fn main(args: &[String]) -> ExitCode {
    let cli = match parse(args) {
        Ok(cli) => cli,
        Err(why) => {
            eprintln!("verify perf: {why}\n\n{}", usage());
            return ExitCode::from(2);
        }
    };
    if cli.help {
        print!("{}", usage());
        return ExitCode::SUCCESS;
    }
    if cli.list {
        print!("{}", list());
        return ExitCode::SUCCESS;
    }
    #[cfg(unix)]
    let arena_args = match arena::Args::from_options(&cli.options) {
        Ok(a) => a,
        Err(why) => {
            eprintln!("verify perf: {why}\n\n{}", usage());
            return ExitCode::from(2);
        }
    };
    // Before any lane starts a thread: an interrupted arena run retires the
    // windows and scratch directories it made.
    #[cfg(unix)]
    if cli.lanes.iter().any(|l| l.kind() == Kind::Arena) {
        arena::live::guard();
    }
    let names: Vec<&str> = cli.lanes.iter().map(|l| l.name()).collect();
    eprintln!("=== verify perf: {} ===", names.join(" "));

    let mut ran: Vec<(Lane, LaneRun, f64)> = Vec::new();
    for lane in &cli.lanes {
        let started = Instant::now();
        let record = cli.record.contains(lane);
        eprintln!("--- {} ---", lane.name());
        let run = match lane {
            Lane::Throughput => floors::gate_throughput(record),
            Lane::Pathological => floors::gate_keyed(&floors::PATHOLOGICAL_LANE, record),
            Lane::ScrollScrub => floors::gate_keyed(&floors::SCROLL_LANE, record),
            Lane::Search => floors::gate_keyed(&floors::SEARCH_LANE, record),
            Lane::Restore => floors::gate_keyed(&floors::RESTORE_LANE, record),
            Lane::Resize => floors::gate_resize(record),
            Lane::Zstd => floors::gate_keyed(&floors::ZSTD_LANE, record),
            Lane::Wasm => wasm::gate_wasm(record),
            #[cfg(unix)]
            Lane::Start => arena::start::run(&arena_args),
            #[cfg(unix)]
            Lane::Compare => arena::compare::run(&arena_args),
            #[cfg(unix)]
            Lane::Cat => arena::cat::run(&arena_args),
            #[cfg(unix)]
            Lane::Scroll => arena::scroll::run(&arena_args),
            #[cfg(unix)]
            Lane::Vte => arena::vte::run(&arena_args),
            #[cfg(not(unix))]
            Lane::Start | Lane::Compare | Lane::Cat | Lane::Scroll | Lane::Vte => LaneRun::skip(
                "the arena lanes launch unix process groups and macOS windows",
                "run them on the bench Mac",
            ),
        };
        ran.push((*lane, run, started.elapsed().as_secs_f64()));
    }

    // The trend meets whatever the floor lanes measured.
    let samples: Vec<TrendSample> = ran.iter().flat_map(|(_, r, _)| r.samples.clone()).collect();
    let mut trend_run = None;
    if !samples.is_empty() {
        let started = Instant::now();
        eprintln!("--- trend ---");
        // Only the floor lanes measured these samples, so only they decide
        // whether the run may write itself into the ledger.
        let floors: Vec<&LaneRun> = ran
            .iter()
            .filter(|(l, _, _)| l.kind() == Kind::Floor)
            .map(|(_, r, _)| r)
            .collect();
        let lanes_ok = floors.iter().all(|r| !r.failed());
        // The trend lanes whose baselines this run rewrote, by the name their
        // samples carry into the ledger.
        let recorded: Vec<&str> = floors
            .iter()
            .filter(|r| matches!(r.status, Status::Recorded(_)))
            .flat_map(|r| r.samples.iter().map(|s| s.lane))
            .collect();
        trend_run = Some((
            trend::gate_trend(&samples, lanes_ok, &recorded),
            started.elapsed().as_secs_f64(),
        ));
    }

    let mut rows: Vec<(&str, &LaneRun, f64)> =
        ran.iter().map(|(l, r, s)| (l.name(), r, *s)).collect();
    if let Some((run, secs)) = &trend_run {
        rows.push(("trend", run, *secs));
    }
    let (text, failed) = verdict(&rows);
    print!("{text}");
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// The closing ladder: one line per lane, then the verdict. Pure
/// (unit-tested); `true` when a lane failed.
pub(crate) fn verdict(rows: &[(&str, &LaneRun, f64)]) -> (String, bool) {
    let mut s = String::from("== verify perf ==\n");
    let mut failed = Vec::new();
    let mut unmeasured = Vec::new();
    let judged = rows
        .iter()
        .filter(|(_, run, _)| run.status == Status::Pass)
        .count();
    for (name, run, secs) in rows {
        let (tag, detail) = match &run.status {
            Status::Pass if run.unjudged.is_empty() => ("ok", "within bounds".to_string()),
            Status::Pass => ("ok", format!("NOT JUDGED: {}", run.unjudged.join("; "))),
            Status::NotJudged(why) => ("meas", format!("NOT JUDGED: {why}")),
            Status::Recorded(path) => ("rec", format!("baseline rewritten -> {}", path.display())),
            Status::Measured(at) => ("meas", format!("measured, no floor to judge — {at}")),
            Status::Skip { why, fix } => ("skip", format!("{why} — {fix}")),
            Status::Fail(findings) => ("FAIL", findings.join("; ")),
        };
        s.push_str(&format!("{tag:<5} {name:<13} {secs:>7.1}s  {detail}\n"));
        match &run.status {
            Status::Fail(_) => failed.push(*name),
            Status::Skip { .. } => unmeasured.push(format!("{name} (skipped)")),
            Status::Recorded(_) => unmeasured.push(format!("{name} (recorded, not compared)")),
            Status::Measured(_) => unmeasured.push(format!("{name} (measured; no floor)")),
            Status::NotJudged(why) => unmeasured.push(format!("{name} ({why})")),
            Status::Pass => unmeasured.extend(run.unjudged.iter().cloned()),
        }
    }
    if !failed.is_empty() {
        s.push_str(&format!("verify perf: FAILED — {}\n", failed.join(", ")));
    } else if unmeasured.is_empty() {
        s.push_str("verify perf: GREEN — every lane that ran was judged and held\n");
    } else if judged == 0 {
        s.push_str(&format!(
            "verify perf: NOTHING JUDGED — {}\n",
            unmeasured.join(", ")
        ));
    } else {
        s.push_str(&format!(
            "verify perf: GREEN for what was judged; NOT JUDGED: {}\n",
            unmeasured.join(", ")
        ));
    }
    (s, !failed.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn a_bare_run_is_every_floor_lane_and_no_arena_lane() {
        let cli = parse(&[]).expect("parses");
        assert_eq!(
            cli.lanes,
            vec![
                Lane::Throughput,
                Lane::Pathological,
                Lane::ScrollScrub,
                Lane::Search,
                Lane::Restore,
                Lane::Resize,
                Lane::Zstd,
                Lane::Wasm
            ]
        );
        assert!(cli.record.is_empty());
    }

    #[test]
    fn named_lanes_run_alone_in_canonical_order() {
        let cli = parse(&args("restore throughput restore")).expect("parses");
        assert_eq!(cli.lanes, vec![Lane::Throughput, Lane::Restore]);
    }

    #[test]
    fn record_names_exactly_the_lanes_it_rewrites_and_runs_them() {
        let cli = parse(&args("search --record restore resize")).expect("parses");
        assert_eq!(cli.lanes, vec![Lane::Search, Lane::Restore, Lane::Resize]);
        assert_eq!(cli.record, vec![Lane::Restore, Lane::Resize]);
        // A bare --record would rewrite nothing it names: refused.
        assert!(parse(&args("--record")).is_err());
        // An arena lane has no baseline: named after --record, it just runs.
        let cli = parse(&args("--record restore start resize")).expect("parses");
        assert_eq!(cli.lanes, vec![Lane::Restore, Lane::Resize, Lane::Start]);
        assert_eq!(cli.record, vec![Lane::Restore, Lane::Resize]);
        let why = parse(&args("--record start")).unwrap_err();
        assert!(why.contains("floor lanes"), "{why}");
    }

    #[test]
    fn an_option_needs_one_of_its_lanes() {
        let cli = parse(&args("start --samples 3 --backend cpu")).expect("parses");
        assert_eq!(cli.options.get("--samples").map(String::as_str), Some("3"));
        let why = parse(&args("restore --samples 3")).unwrap_err();
        assert!(why.contains("--samples") && why.contains("start"), "{why}");
        assert!(parse(&args("start --samples")).is_err(), "a missing value");
        assert!(parse(&args("start --warm --warm")).is_err(), "given twice");
        assert!(parse(&args("bogus")).is_err());
        assert!(
            parse(&args("compare --baseline a")).is_err(),
            "both arms are required"
        );
        assert!(parse(&args("compare --baseline a --candidate b")).is_ok());
        assert!(parse(&args("--bogus")).is_err());
    }

    #[test]
    fn the_verdict_fails_on_a_failure_and_names_what_was_not_judged() {
        let pass = LaneRun::pass();
        let partial = LaneRun::not_judged("no baseline");
        let skip = LaneRun::skip("no node", "install node");
        let fail = LaneRun::fail("x < floor");
        let measured = LaneRun::measured("ledger docs/measured/arena/x.md");
        let (text, failed) = verdict(&[("throughput", &pass, 1.0)]);
        assert!(!failed);
        assert!(text.contains("GREEN — every lane"), "{text}");
        let (text, failed) = verdict(&[
            ("throughput", &pass, 1.0),
            ("restore", &partial, 1.0),
            ("wasm", &skip, 0.1),
        ]);
        assert!(!failed);
        assert!(
            text.contains("NOT JUDGED: restore (no baseline), wasm (skipped)"),
            "{text}"
        );
        assert!(text.contains("skip  wasm"), "{text}");
        // A lane that measured without a floor never reads as judged.
        let (text, failed) = verdict(&[("throughput", &pass, 1.0), ("compare", &measured, 60.0)]);
        assert!(!failed);
        assert!(text.contains("meas  compare"), "{text}");
        assert!(
            text.contains("NOT JUDGED: compare (measured; no floor)"),
            "{text}"
        );
        assert!(!text.contains("GREEN — every lane"), "{text}");
        let (text, failed) = verdict(&[("compare", &measured, 60.0)]);
        assert!(!failed);
        assert!(
            text.contains("NOTHING JUDGED — compare (measured; no floor)"),
            "{text}"
        );
        // A lane with no baseline judged nothing, so it is never "GREEN for
        // what was judged".
        let (text, failed) = verdict(&[("throughput", &partial, 1.0), ("wasm", &skip, 0.1)]);
        assert!(!failed);
        assert!(
            text.contains("NOTHING JUDGED — throughput (no baseline), wasm (skipped)"),
            "{text}"
        );
        // A held fence is a judgment even when the floors had no baseline.
        let mut fenced = LaneRun::not_judged("no baseline");
        fenced.fence_held("resize");
        let (text, _) = verdict(&[("resize", &fenced, 1.0)]);
        assert!(
            text.contains("GREEN for what was judged; NOT JUDGED: resize floors (no baseline)"),
            "{text}"
        );
        let (text, failed) = verdict(&[("throughput", &pass, 1.0), ("search", &fail, 2.0)]);
        assert!(failed);
        assert!(text.contains("FAILED — search"), "{text}");
    }

    #[test]
    fn add_failure_keeps_earlier_findings() {
        let mut run = LaneRun::pass();
        run.add_failure("a");
        run.add_failure("b");
        assert_eq!(run.status, Status::Fail(vec!["a".into(), "b".into()]));
    }
}
