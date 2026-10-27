// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify perf` — aterm's performance lanes, each runnable alone or in any
//! combination.
//!
//! ```text
//! targo --unverified verify perf [LANE...] [--record LANE...] [OPTIONS]
//! targo --unverified verify perf --list
//! ```
//!
//! Three kinds of lane. The FLOOR lanes ([`floors`], [`wasm`]) measure the
//! engine headless and judge each metric against its committed baseline in
//! `crates/aterm-verify/perf/golden/`; whatever they measured then meets the
//! same-box trend ledger ([`trend`]), and a green run appends to it. A bare `verify
//! perf` runs every floor lane. The ARENA lanes ([`arena`]) open real
//! windows and take minutes; they run only when named, and what they measure
//! has no floor, so they report MEASURED, never a pass. The MEASURED lanes
//! ([`fabric`], [`restore_e2e`], [`aos`]) are headless but just as unjudgeable —
//! their numbers are a disk's fsync, or a shared desk's scheduler, before they
//! are aterm's — so they too run only when named and report MEASURED (fabric
//! writing a ledger entry under `docs/measured/fabric/`, restore-e2e one under
//! `docs/measured/restore/`, aos a summary, and its JSON with `--json-out`).
//!
//! `--no-windows` (the house switch for an agent sharing the owner's desktop)
//! turns every lane that opens a window into a named skip and runs the rest.
//!
//! Nothing runs this automatically: it measures when someone asks it to.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use aterm_capacity::Machine as _;

#[cfg(unix)]
mod aos;
#[cfg(unix)]
pub(crate) mod arena;
#[cfg(unix)]
mod bench_build;
mod driver;
#[cfg(unix)]
mod fabric;
mod floors;
#[cfg(unix)]
mod restore_e2e;
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
    } else if code == ExitCode::from(3) {
        crate::exit::COULD_NOT_RUN
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
    CrashRestore,
    Resize,
    Zstd,
    Effects,
    TrailDarkComposer,
    TrailLightComposer,
    Wasm,
    Start,
    Compare,
    Cat,
    Scroll,
    Vte,
    Fabric,
    RestoreE2e,
    Aos,
}

/// Which kind of lane, and so whether a bare run includes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Headless, judged against a committed baseline; the default set.
    Floor,
    /// A real-window head-to-head; run only when named.
    Arena,
    /// Headless, real processes, nothing to judge against; run only when
    /// named.
    Measured,
}

impl Lane {
    pub(crate) const ALL: [Lane; 20] = [
        Lane::Throughput,
        Lane::Pathological,
        Lane::ScrollScrub,
        Lane::Search,
        Lane::Restore,
        Lane::CrashRestore,
        Lane::Resize,
        Lane::Zstd,
        Lane::Effects,
        Lane::TrailDarkComposer,
        Lane::TrailLightComposer,
        Lane::Wasm,
        Lane::Start,
        Lane::Compare,
        Lane::Cat,
        Lane::Scroll,
        Lane::Vte,
        Lane::Fabric,
        Lane::RestoreE2e,
        Lane::Aos,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Lane::Throughput => "throughput",
            Lane::Pathological => "pathological",
            Lane::ScrollScrub => "scroll-scrub",
            Lane::Search => "search",
            Lane::Restore => "restore",
            Lane::CrashRestore => "crash-restore",
            Lane::Resize => "resize",
            Lane::Zstd => "zstd",
            Lane::Effects => "effects",
            Lane::TrailDarkComposer => "trail-dark-composer",
            Lane::TrailLightComposer => "trail-light-composer",
            Lane::Wasm => "wasm",
            Lane::Start => "start",
            Lane::Compare => "compare",
            Lane::Cat => "cat",
            Lane::Scroll => "scroll",
            Lane::Vte => "vte",
            Lane::Fabric => "fabric",
            Lane::RestoreE2e => "restore-e2e",
            Lane::Aos => "aos",
        }
    }

    pub(crate) const fn kind(self) -> Kind {
        match self {
            Lane::Throughput
            | Lane::Pathological
            | Lane::ScrollScrub
            | Lane::Search
            | Lane::Restore
            | Lane::CrashRestore
            | Lane::Resize
            | Lane::Zstd
            | Lane::Effects
            | Lane::TrailDarkComposer
            | Lane::TrailLightComposer
            | Lane::Wasm => Kind::Floor,
            Lane::Start | Lane::Compare | Lane::Cat | Lane::Scroll | Lane::Vte => Kind::Arena,
            Lane::Fabric | Lane::RestoreE2e | Lane::Aos => Kind::Measured,
        }
    }

    /// Whether the lane puts a window on the desktop: the lanes `--no-windows`
    /// skips. Every arena lane does; no other lane does.
    pub(crate) fn opens_windows(self) -> bool {
        self.kind() == Kind::Arena
    }

    /// What the lane measures, for `--list`.
    const fn what(self) -> &'static str {
        match self {
            Lane::Throughput => "Terminal::process MB/s over a ~32 MiB mixed VT workload",
            Lane::Pathological => "Terminal::process MB/s per hostile-input corpus",
            Lane::ScrollScrub => "scrollback scrub read-path rates over a 120k-line tiered fill",
            Lane::Search => "search index build, cached query and retained memory",
            Lane::Restore => "serialize -> fresh engine -> replay rate",
            Lane::CrashRestore => {
                "crash-restore journal: tail-tap overhead, keyframe and recover rates"
            }
            Lane::Resize => "resize/rewrap rates plus the absolute 42s-freeze fences",
            Lane::Zstd => {
                "aterm-zstd decode MB/s over recorded zstd CLI frames (atpkg's installer)"
            }
            Lane::Effects => {
                "harness card, cursor glow and inline-scroll frames: emission, CPU raster, GPU \
                 encode and GPU time, and the pass-economy ablation"
            }
            Lane::TrailDarkComposer => {
                "the cursor trail typed to a 117-cell line in Claude Code's composer, on the dark \
                 ground (Default): emission, CPU raster, GPU encode and GPU time"
            }
            Lane::TrailLightComposer => {
                "trail-dark-composer's scene on Default Light, the trail's light (paper) arm: \
                 emission, CPU raster, GPU encode and GPU time"
            }
            Lane::Wasm => {
                "the shipped wasm modules in the pinned chrome-headless-shell, plus their \
                 module-size ceilings"
            }
            Lane::Start => "fresh-process real-window startup timing of one aterm",
            Lane::Compare => "paired ABBA startup A/B of two aterm binaries",
            Lane::Cat => "cat 150 MB ascii + unicode, aterm vs the pinned ghostty tip",
            Lane::Scroll => "windowed 120k-line scrub + jump-to-top frame pacing",
            Lane::Vte => "vtebench's default suite, aterm vs the pinned ghostty tip",
            Lane::Fabric => {
                "fabric post->deliver latency, burst throughput, request/reply ping-pong, attach \
                 time and records per message over a real broker, bridge and headless aterm"
            }
            Lane::RestoreE2e => {
                "a crashed window's tabs back for 1, 8 and 32 sessions: this build's keeper, a \
                 headless instance SIGKILLed by its own pid, its successor's spawn to the keeper's \
                 HELLO answered, the one-after-another admission of the offers, every recovered \
                 session placed and its history imported, and the first ctl request served past \
                 the recovery-count fence, per arm (clean, copy, lookalike)"
            }
            Lane::Aos => {
                "the momentum glow hosted under Aos on one recorded typing story: per-call cost \
                 on the engine, the unattached host and the attached host, checkpoint, event \
                 log, shadow replay and repair, warm and cold"
            }
        }
    }

    /// What the box needs, for `--list`.
    const fn needs(self) -> &'static str {
        match self {
            Lane::Throughput
            | Lane::Pathological
            | Lane::ScrollScrub
            | Lane::Search
            | Lane::Restore
            | Lane::CrashRestore
            | Lane::Resize
            | Lane::Zstd => "targo (release build of aterm-bench)",
            Lane::Effects | Lane::TrailDarkComposer | Lane::TrailLightComposer => {
                "targo (release build of aterm-bench with dev-seams), a GPU (its GPU keys need \
                 Metal's clock)"
            }
            Lane::Wasm => {
                "chrome-headless-shell (verify provision), rustup stable + \
                 wasm32-unknown-unknown, wasm-opt"
            }
            Lane::Start => "macOS, a built aterm (--binary)",
            Lane::Compare => "macOS, two built aterm binaries (--baseline, --candidate)",
            Lane::Cat => {
                "macOS on the M4 Max bench box (or --dev-smoke), targo (release build of \
                 aterm), curl"
            }
            Lane::Vte => {
                "macOS on the M4 Max bench box (or --dev-smoke), targo (release builds of aterm \
                 and vtebench), curl, git"
            }
            Lane::Scroll => {
                "macOS on the M4 Max bench box (or --dev-smoke), targo (release build of aterm)"
            }
            Lane::Fabric => {
                "unix, targo (builds aterm and its fabric_bench bench: test, or opt-test with \
                 --profile opt)"
            }
            Lane::RestoreE2e => {
                "macOS, targo (builds aterm and its restore_bench bench: test, or opt-test with \
                 --profile opt)"
            }
            Lane::Aos => {
                "unix, targo (builds aterm-bench's aos_harness twice: release, and opt-test \
                 for the stranded repair); macOS for its instruction, fault and syscall counters"
            }
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
    /// numbers went. The arena and MEASURED lanes ([`Kind::Arena`],
    /// [`Kind::Measured`]) report this way, and they are unix.
    #[cfg_attr(not(unix), allow(dead_code))]
    Measured(String),
    /// It could not measure on this box: why, and what to do about it.
    Skip { why: String, fix: String },
    /// A required provisioned tool is missing or did not start: why, and the
    /// provider's remedy.
    CouldNotRun { why: String, fix: String },
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
    pub(crate) const fn pass() -> Self {
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

    pub(crate) fn could_not_run(why: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            status: Status::CouldNotRun {
                why: why.into(),
                fix: fix.into(),
            },
            ..Self::pass()
        }
    }

    pub(crate) const fn failed(&self) -> bool {
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
const JSON_LANES: &[Lane] = &[
    Lane::Start,
    Lane::Compare,
    Lane::Fabric,
    Lane::RestoreE2e,
    Lane::Aos,
];
const BUILT_LANES: &[Lane] = &[Lane::Fabric, Lane::RestoreE2e];
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
        lanes: JSON_LANES,
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
        help: "run off the M4 Max bench box, labelled non-publishable",
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
    OptSpec {
        flag: "--scenario",
        value: Some("all|messaging|pingpong|attach|handoff|ask"),
        lanes: &[Lane::Fabric],
        help: "measure only this fabric scenario (default all)",
    },
    OptSpec {
        flag: "--latency-samples",
        value: Some("N"),
        lanes: &[Lane::Fabric],
        help: "measured post->deliver round trips (default 200)",
    },
    OptSpec {
        flag: "--burst",
        value: Some("N"),
        lanes: &[Lane::Fabric],
        help: "posts in the back-to-back burst (default 500)",
    },
    OptSpec {
        flag: "--pingpong-rounds",
        value: Some("N"),
        lanes: &[Lane::Fabric],
        help: "measured request/reply round trips, B replying on receipt (default 200)",
    },
    OptSpec {
        flag: "--attach-sessions",
        value: Some("N,N,..."),
        lanes: &[Lane::Fabric],
        help: "rising session counts a restarted bridge attaches with (default 1,8,32)",
    },
    OptSpec {
        flag: "--attach-rounds",
        value: Some("N"),
        lanes: &[Lane::Fabric],
        help: "bridge restarts measured per session count (default 3)",
    },
    OptSpec {
        flag: "--attach-backlog",
        value: Some("N"),
        lanes: &[Lane::Fabric],
        help: "records filled behind every attach session's refill watermark, after each one's \
               note is seen, with a paced publisher timing the writer's stall (default 0: none)",
    },
    OptSpec {
        flag: "--durability",
        value: Some("TIER,..."),
        lanes: &[Lane::Fabric],
        help: "the arms, one run and one ledger entry each, opened with aterm's options: \
               ordered (default, once per --horizon-ms), strict and relaxed (no flush: the \
               ceiling arm)",
    },
    OptSpec {
        flag: "--retention",
        value: Some("on|off"),
        lanes: &[Lane::Fabric],
        help: "whether every world's broker retains its log by aterm's default rules (default \
               on, what ships; off also keeps the bridge's auto mirror off)",
    },
    OptSpec {
        flag: "--log",
        value: Some("auto|off"),
        lanes: &[Lane::Fabric],
        help: "the bridge's bus mirror (default auto, what ships: on over a retaining broker)",
    },
    OptSpec {
        flag: "--retention-interval-ms",
        value: Some("N"),
        lanes: &[Lane::Fabric],
        help: "the retaining broker's retention tick interval (default astream's minute, what \
               ships)",
    },
    OptSpec {
        flag: "--bystanders",
        value: Some("N"),
        lanes: &[Lane::Fabric],
        help: "idle sessions spawned beside the messaging, ping-pong and ask worlds' talking \
               sessions (default 0)",
    },
    OptSpec {
        flag: "--asks",
        value: Some("N"),
        lanes: &[Lane::Fabric],
        help: "posts per leg of the ask scenario: each kind's idle releases, the busy ones, the \
               receipts and the person-lane asks without a pager and with one (default 100)",
    },
    OptSpec {
        flag: "--mix-every",
        value: Some("N"),
        lanes: &[Lane::Fabric],
        help: "the ask scenario's escalated mix: one person-lane post before every Nth ordinary \
               message (default 10)",
    },
    OptSpec {
        flag: "--horizon-ms",
        value: Some("N,N,..."),
        lanes: &[Lane::Fabric],
        help: "the ordered arm's horizons, one run each (default aterm's 100); needs \
               --durability naming ordered",
    },
    OptSpec {
        flag: "--passes",
        value: Some("N"),
        lanes: BUILT_LANES,
        help: "run the arm list N times, forward then backward: A B B A (default 1)",
    },
    OptSpec {
        flag: "--profile",
        value: Some("dev|opt"),
        lanes: BUILT_LANES,
        help: "the build measured: dev (default) or opt (cargo profile opt-test: opt-level 3, \
               debug assertions on)",
    },
    OptSpec {
        flag: "--sessions",
        value: Some("N,N,..."),
        lanes: &[Lane::RestoreE2e],
        help: "rising session counts one crash takes down (default 1,8,32)",
    },
    OptSpec {
        flag: "--rounds",
        value: Some("N"),
        lanes: &[Lane::RestoreE2e],
        help: "crashes and recoveries per session count (default 3)",
    },
    OptSpec {
        flag: "--lines",
        value: Some("N"),
        lanes: &[Lane::RestoreE2e],
        help: "history lines each session prints before the crash (default 5000)",
    },
    OptSpec {
        flag: "--arm",
        value: Some("clean|copy|lookalike,..."),
        lanes: &[Lane::RestoreE2e],
        help: "the arms, one run and one ledger entry each (default all three): clean (what \
               ships), copy (each region restored from a heap copy) and lookalike (that copy with \
               the look-alike tail planted); the seam's own copy and plant are off the readings",
    },
    OptSpec {
        flag: "--lookalike-kib",
        value: Some("N"),
        lanes: &[Lane::RestoreE2e],
        help: "the lookalike arm's look-alikes past each ring's commit, in KiB, stopping at the \
               ring's lap (default 6912)",
    },
    OptSpec {
        flag: "--no-windows",
        value: None,
        lanes: &Lane::ALL,
        help: "skip every lane that opens a window (the arena lanes); run the rest",
    },
    OptSpec {
        flag: "--admit",
        value: Some("wait|now"),
        lanes: &Lane::ALL,
        help: "wait for a quiet machine before each lane (default wait, saying why once a \
               minute), or start each at once (now)",
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
                // An arena or measured lane has no baseline: after `--record` it just runs.
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
    if cli.options.contains_key("--json-out") {
        let writers: Vec<&str> = cli
            .lanes
            .iter()
            .filter(|l| JSON_LANES.contains(l))
            .map(|l| l.name())
            .collect();
        if writers.len() > 1 {
            return Err(format!(
                "`--json-out` names one file, and {} would each write their own report to it; \
                 run them one at a time",
                writers.join(" and ")
            ));
        }
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
         named after it; an arena or measured lane named there just runs.\n\
         Exit: 0 nothing failed (a SKIP names its fix) · 1 a lane failed · 2 usage · 3 a lane\n\
         could not run (a COULD NOT RUN names its fix).\n\
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
    for kind in [Kind::Floor, Kind::Arena, Kind::Measured] {
        s.push_str(match kind {
            Kind::Floor => {
                "FLOOR lanes (the default; judged against crates/aterm-verify/perf/golden, \
                 then the same-box trend)\n"
            }
            Kind::Arena => {
                "ARENA lanes (only when named; real windows, minutes; measured, never judged: \
                 cat, scroll and vte write a docs/measured/arena entry, start and compare print \
                 a summary, and JSON with --json-out)\n"
            }
            Kind::Measured => {
                "MEASURED lanes (only when named; headless, real processes, minutes; measured, \
                 not judged: fabric writes a docs/measured/fabric entry, restore-e2e a \
                 docs/measured/restore entry, aos prints a summary, and JSON with --json-out)\n"
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

/// Filter `options` to those accepted by lanes matching `reads`.
#[cfg(unix)]
fn options_for(
    options: &BTreeMap<&'static str, String>,
    reads: impl Fn(Lane) -> bool,
) -> BTreeMap<&'static str, String> {
    options
        .iter()
        .filter(|(flag, _)| {
            OPTIONS
                .iter()
                .find(|o| o.flag == **flag)
                .is_some_and(|o| o.lanes.iter().any(|l| reads(*l)))
        })
        .map(|(flag, value)| (*flag, value.clone()))
        .collect()
}

/// Each lane family's own options, parsed — before any lane runs, so a bad
/// value is a usage error (exit 2) and never a lane that failed after a build.
/// A measured lane's parser runs only when its lane is named: `--json-out`,
/// `--profile` and `--passes` are shared, and one lane's rules for them never
/// refuse the other's run.
#[cfg(unix)]
fn lane_args(cli: &Cli) -> Result<(arena::Args, fabric::Args, restore_e2e::Args), String> {
    let named = |lane: Lane| cli.lanes.contains(&lane);
    Ok((
        arena::Args::from_options(&options_for(&cli.options, |l| l.kind() == Kind::Arena))?,
        if named(Lane::Fabric) {
            fabric::Args::from_options(&options_for(&cli.options, |l| l == Lane::Fabric))?
        } else {
            fabric::Args::default()
        },
        if named(Lane::RestoreE2e) {
            restore_e2e::Args::from_options(&options_for(&cli.options, |l| l == Lane::RestoreE2e))?
        } else {
            restore_e2e::Args::default()
        },
    ))
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
    let admit = match cli
        .options
        .get("--admit")
        .map(|w| (w, aterm_capacity::Admit::parse(w)))
    {
        None => aterm_capacity::Admit::Wait,
        Some((_, Some(mode))) => mode,
        Some((word, None)) => {
            eprintln!(
                "verify perf: `--admit` takes wait or now, not `{word}`\n\n{}",
                usage()
            );
            return ExitCode::from(2);
        }
    };
    #[cfg(unix)]
    let (arena_args, fabric_args, restore_args) = match lane_args(&cli) {
        Ok(a) => a,
        Err(why) => {
            eprintln!("verify perf: {why}\n\n{}", usage());
            return ExitCode::from(2);
        }
    };
    let no_windows = cli.options.contains_key("--no-windows");
    #[cfg(unix)]
    if cli
        .lanes
        .iter()
        .any(|l| l.kind() == Kind::Arena && !(no_windows && l.opens_windows()))
    {
        arena::live::guard();
    }
    let names: Vec<&str> = cli.lanes.iter().map(|l| l.name()).collect();
    eprintln!("=== verify perf: {} ===", names.join(" "));
    let tree = trend::head_sha();

    let mut ran: Vec<(Lane, LaneRun, f64)> = Vec::new();
    for lane in &cli.lanes {
        let record = cli.record.contains(lane);
        eprintln!("--- {} ---", lane.name());
        let skip = no_windows && lane.opens_windows();
        let quiet = match (!skip).then(|| quiet(*lane, admit)).transpose() {
            Ok(quiet) => quiet.flatten(),
            Err(why) => {
                eprintln!("verify perf: COULD NOT RUN — {why}");
                return ExitCode::from(3);
            }
        };
        let started = Instant::now();
        let from = aterm_capacity::Live.read(None);
        let run = match lane {
            _ if skip => LaneRun::skip(
                "it opens a window on the desktop (--no-windows)",
                "run it without --no-windows when the desktop is free",
            ),
            Lane::Throughput => floors::gate_throughput(record),
            Lane::Pathological => floors::gate_keyed(&floors::PATHOLOGICAL_LANE, record),
            Lane::ScrollScrub => floors::gate_keyed(&floors::SCROLL_LANE, record),
            Lane::Search => floors::gate_keyed(&floors::SEARCH_LANE, record),
            Lane::Restore => floors::gate_keyed(&floors::RESTORE_LANE, record),
            Lane::CrashRestore => floors::gate_crash_restore(record),
            Lane::Resize => floors::gate_resize(record),
            Lane::Zstd => floors::gate_keyed(&floors::ZSTD_LANE, record),
            Lane::Effects => floors::gate_effects(&floors::EFFECTS, record),
            Lane::TrailDarkComposer => floors::gate_effects(&floors::TRAIL_DARK_COMPOSER, record),
            Lane::TrailLightComposer => floors::gate_effects(&floors::TRAIL_LIGHT_COMPOSER, record),
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
            #[cfg(unix)]
            Lane::Fabric => fabric::run(&fabric_args),
            #[cfg(not(unix))]
            Lane::Fabric => LaneRun::skip(
                "the fabric bridge is unix-only (aterm-link)",
                "run it on macOS or Linux",
            ),
            #[cfg(unix)]
            Lane::RestoreE2e => restore_e2e::run(&restore_args),
            #[cfg(not(unix))]
            Lane::RestoreE2e => LaneRun::skip(
                "the crash-restore journal's live lane is macOS-only",
                "run it on a Mac",
            ),
            #[cfg(unix)]
            Lane::Aos => aos::run(cli.options.get("--json-out").map(Path::new)),
            #[cfg(not(unix))]
            Lane::Aos => LaneRun::skip(
                "the aos lane spawns its harness through the unix process helpers",
                "run it on macOS or Linux",
            ),
        };
        if quiet.is_some() {
            let to = aterm_capacity::Live.read(None);
            eprintln!("  machine: {}", aterm_capacity::between(&from, &to));
        }
        drop(quiet);
        ran.push((*lane, run, started.elapsed().as_secs_f64()));
    }

    let samples: Vec<TrendSample> = ran.iter().flat_map(|(_, r, _)| r.samples.clone()).collect();
    let mut trend_run = None;
    if !samples.is_empty() {
        let started = Instant::now();
        eprintln!("--- trend ---");
        let floors: Vec<&LaneRun> = ran
            .iter()
            .filter(|(l, _, _)| l.kind() == Kind::Floor)
            .map(|(_, r, _)| r)
            .collect();
        let lanes_ok = floors.iter().all(|r| !r.failed());
        let recorded: Vec<&str> = floors
            .iter()
            .filter(|r| matches!(r.status, Status::Recorded(_)))
            .flat_map(|r| r.samples.iter().map(|s| s.lane))
            .collect();
        trend_run = Some((
            trend::gate_trend(&samples, lanes_ok, &recorded, &tree),
            started.elapsed().as_secs_f64(),
        ));
    }

    let mut rows: Vec<(&str, &LaneRun, f64)> =
        ran.iter().map(|(l, r, s)| (l.name(), r, *s)).collect();
    if let Some((run, secs)) = &trend_run {
        rows.push(("trend", run, *secs));
    }
    let (text, code) = verdict(&rows);
    print!("{text}");
    match code {
        crate::exit::PASS => ExitCode::SUCCESS,
        crate::exit::COULD_NOT_RUN => ExitCode::from(3),
        _ => ExitCode::FAILURE,
    }
}

/// The quiet machine for one lane (`docs/DESIGN-adaptive-build-2026-10-24.md`, QUIET-ADMIT and
/// QUIET-BOUND): admitted alone among quiet claims once the load is low, or at its bound whatever
/// the load (saying once a minute what it waits for, and how it started), or at once under
/// `--admit now`; its own build runs under it too. A ledger that cannot be used is said, and the
/// lane runs unadmitted.
///
/// # Errors
/// An interrupt ended the wait (WAIT-INTERRUPT): why, and the run ends COULD NOT RUN.
fn quiet(lane: Lane, mode: aterm_capacity::Admit) -> Result<Option<aterm_capacity::Claim>, String> {
    let need = aterm_capacity::Need::timing(format!("verify perf {}", lane.name()));
    match aterm_capacity::admit(need, mode) {
        Ok(claim) => {
            let to = aterm_capacity::Live.read(None);
            eprintln!(
                "  quiet machine: admitted{} at {}{}",
                if claim.waited().as_secs() > 0 {
                    format!(" after {}", aterm_capacity::span(claim.waited()))
                } else {
                    String::new()
                },
                to.brief(),
                claim
                    .quiet()
                    .filter(|q| q.at_bound || !q.reserved.is_zero())
                    .map_or_else(String::new, |q| format!(" ({q})"))
            );
            Ok(Some(claim))
        }
        Err(e @ aterm_capacity::Error::Stopped { .. }) => Err(format!(
            "{}: its quiet claim was {e}; it started nothing",
            lane.name()
        )),
        Err(e) => {
            eprintln!("  {e}; the lane runs unadmitted");
            Ok(None)
        }
    }
}

/// Format the per-lane summary and overall exit code.
pub(crate) fn verdict(rows: &[(&str, &LaneRun, f64)]) -> (String, i32) {
    let mut s = String::from("== verify perf ==\n");
    let mut failed = Vec::new();
    let mut unrun = Vec::new();
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
            Status::CouldNotRun { why, fix } => ("NRUN", format!("COULD NOT RUN: {why} — {fix}")),
            Status::Fail(findings) => ("FAIL", findings.join("; ")),
        };
        s.push_str(&format!("{tag:<5} {name:<13} {secs:>7.1}s  {detail}\n"));
        match &run.status {
            Status::Fail(_) => failed.push(*name),
            Status::CouldNotRun { .. } => unrun.push(*name),
            Status::Skip { .. } => unmeasured.push(format!("{name} (skipped)")),
            Status::Recorded(_) => unmeasured.push(format!("{name} (recorded, not compared)")),
            Status::Measured(_) => unmeasured.push(format!("{name} (measured; no floor)")),
            Status::NotJudged(why) => unmeasured.push(format!("{name} ({why})")),
            Status::Pass => unmeasured.extend(run.unjudged.iter().cloned()),
        }
    }
    if !failed.is_empty() {
        let mut head = failed.join(", ");
        if !unrun.is_empty() {
            head.push_str(&format!("; {} could not run", unrun.join(", ")));
        }
        s.push_str(&format!("verify perf: FAILED — {head}\n"));
        return (s, crate::exit::FAILED);
    }
    if !unrun.is_empty() {
        s.push_str(&format!(
            "verify perf: COULD NOT RUN — {} could not run\n",
            unrun.join(", ")
        ));
        return (s, crate::exit::COULD_NOT_RUN);
    }
    if unmeasured.is_empty() {
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
    (s, crate::exit::PASS)
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
                Lane::CrashRestore,
                Lane::Resize,
                Lane::Zstd,
                Lane::Effects,
                Lane::TrailDarkComposer,
                Lane::TrailLightComposer,
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
        assert!(parse(&args("--record")).is_err());
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
    fn the_fabric_lane_runs_only_when_named_and_takes_its_own_options() {
        let cli = parse(&args(
            "fabric --burst 200 --attach-sessions 1,4 --json-out /tmp/f.json",
        ))
        .expect("parses");
        assert_eq!(cli.lanes, vec![Lane::Fabric]);
        assert_eq!(Lane::Fabric.kind(), Kind::Measured);
        assert!(!Lane::Fabric.opens_windows());
        assert!(!parse(&[]).expect("parses").lanes.contains(&Lane::Fabric));
        let why = parse(&args("restore --burst 9")).unwrap_err();
        assert!(why.contains("--burst") && why.contains("fabric"), "{why}");
        let cli = parse(&args("--record restore fabric")).expect("parses");
        assert_eq!(cli.record, vec![Lane::Restore]);
        assert_eq!(cli.lanes, vec![Lane::Restore, Lane::Fabric]);
    }

    #[test]
    fn the_aos_lane_runs_only_when_named_and_writes_its_json_alone() {
        let cli = parse(&args("aos --json-out /tmp/a.json")).expect("parses");
        assert_eq!(cli.lanes, vec![Lane::Aos]);
        assert_eq!(
            cli.options.get("--json-out").map(String::as_str),
            Some("/tmp/a.json")
        );
        assert_eq!(Lane::Aos.kind(), Kind::Measured);
        assert!(!Lane::Aos.opens_windows());
        assert!(!parse(&[]).expect("parses").lanes.contains(&Lane::Aos));
        let why = parse(&args("aos --burst 9")).unwrap_err();
        assert_eq!(why, "`--burst` is read only by fabric; name one of them");
        assert!(parse(&args("aos fabric --json-out /tmp/a.json")).is_err());
        assert!(parse(&args("aos fabric")).is_ok());
        let cli = parse(&args("--record restore aos")).expect("parses");
        assert_eq!(cli.record, vec![Lane::Restore]);
        assert_eq!(cli.lanes, vec![Lane::Restore, Lane::Aos]);
    }

    #[test]
    fn list_names_every_lane_once_under_its_kind() {
        let listed = list();
        for lane in Lane::ALL {
            let rows = listed
                .lines()
                .filter(|l| l.starts_with("  ") && l.split_whitespace().next() == Some(lane.name()))
                .count();
            assert_eq!(rows, 1, "{}:\n{listed}", lane.name());
        }
        let mut kind_at = Vec::new();
        for line in listed.lines() {
            if !line.starts_with(' ') {
                kind_at.push(line);
            } else if let Some(lane) = line.split_whitespace().next().and_then(Lane::from_name) {
                let heading = kind_at.len().checked_sub(1).expect("a heading first");
                let kind = [Kind::Floor, Kind::Arena, Kind::Measured][heading];
                assert_eq!(lane.kind(), kind, "{}", lane.name());
            }
        }
        assert_eq!(kind_at.len(), 3, "{listed}");
    }

    /// The path `main` takes from a command line to each lane family's
    /// options: the arena parser, then the fabric parser, each handed only its
    /// own flags.
    #[cfg(unix)]
    #[test]
    fn every_lane_family_parses_only_its_own_options_before_anything_runs() {
        let cli = parse(&args(
            "fabric --scenario pingpong --latency-samples 7 --burst 200 --pingpong-rounds 30 --attach-sessions 1,4 \
             --attach-rounds 2 --durability relaxed --json-out /tmp/f.json --no-windows --admit now",
        ))
        .expect("parses");
        let (_, fabric_args, _) = lane_args(&cli).expect("both parsers take it");
        assert_eq!(fabric_args.scenario.word(), "pingpong");
        assert_eq!(fabric_args.burst, Some(200));
        assert_eq!(fabric_args.pingpong, Some(30));
        assert_eq!(fabric_args.json_out, Some(PathBuf::from("/tmp/f.json")));
        assert_eq!(
            fabric_args
                .arms
                .iter()
                .map(|arm| arm.tier.word())
                .collect::<Vec<_>>(),
            ["relaxed"]
        );
        assert!(
            arena::Args::from_options(&options_for(&cli.options, |l| l.kind() == Kind::Arena))
                .is_ok()
        );
        for line in [
            "fabric --burst 6000",
            "fabric --pingpong-rounds 0",
            "fabric --attach-sessions 1,200",
            "fabric --profile release",
            "fabric --scenario unknown",
        ] {
            let cli = parse(&args(line)).expect(line);
            assert!(lane_args(&cli).is_err(), "{line}");
        }
        let cli = parse(&args("fabric --profile opt --no-windows")).expect("parses");
        let (_, fabric_args, _) = lane_args(&cli).expect("parses");
        assert_eq!(fabric_args.profile.name(), "opt");
        let cli = parse(&args(
            "restore-e2e --sessions 2,4 --arm copy --json-out /tmp/r.json --profile opt \
             --no-windows",
        ))
        .expect("parses");
        let (_, _, restore_args) = lane_args(&cli).expect("the restore parser takes it");
        assert_eq!(restore_args.sessions, Some(vec![2, 4]));
        assert_eq!(restore_args.profile.name(), "opt");
        assert_eq!(restore_args.plan().len(), 1);
        for line in [
            "restore-e2e --sessions 4,2",
            "restore-e2e --json-out /tmp/r.json",
        ] {
            let cli = parse(&args(line)).expect(line);
            assert!(lane_args(&cli).is_err(), "{line}");
        }
        assert!(
            parse(&args("fabric --sessions 4")).is_err(),
            "restore-e2e's alone"
        );
        let why = parse(&args("start --profile opt")).unwrap_err();
        assert!(why.contains("--profile") && why.contains("fabric"), "{why}");
    }

    #[test]
    fn two_lanes_never_share_one_json_out() {
        assert!(parse(&args("fabric --json-out /tmp/r.json")).is_ok());
        assert!(parse(&args("start --json-out /tmp/r.json")).is_ok());
        assert!(parse(&args("start fabric --json-out /tmp/r.json")).is_err());
        assert!(
            parse(&args(
                "start compare --baseline /a --candidate /b --json-out /tmp/r.json"
            ))
            .is_err()
        );
    }

    #[test]
    fn no_windows_is_accepted_beside_any_lane_and_names_the_windowed_ones() {
        for line in ["fabric --no-windows", "--no-windows", "start --no-windows"] {
            let cli = parse(&args(line)).expect(line);
            assert!(cli.options.contains_key("--no-windows"), "{line}");
        }
        let windowed: Vec<&str> = Lane::ALL
            .into_iter()
            .filter(|l| l.opens_windows())
            .map(Lane::name)
            .collect();
        assert_eq!(windowed, ["start", "compare", "cat", "scroll", "vte"]);
    }

    #[test]
    fn the_verdict_fails_on_a_failure_and_names_what_was_not_judged() {
        let pass = LaneRun::pass();
        let partial = LaneRun::not_judged("no baseline");
        let skip = LaneRun::skip("no Metal device", "run it on a Mac with a GPU");
        let fail = LaneRun::fail("x < floor");
        let measured = LaneRun::measured("ledger docs/measured/arena/x.md");
        let (text, code) = verdict(&[("throughput", &pass, 1.0)]);
        assert_eq!(code, crate::exit::PASS);
        assert!(text.contains("GREEN — every lane"), "{text}");
        let (text, code) = verdict(&[
            ("throughput", &pass, 1.0),
            ("restore", &partial, 1.0),
            ("wasm", &skip, 0.1),
        ]);
        assert_eq!(code, crate::exit::PASS);
        assert!(
            text.contains("NOT JUDGED: restore (no baseline), wasm (skipped)"),
            "{text}"
        );
        assert!(text.contains("skip  wasm"), "{text}");
        let (text, code) = verdict(&[("throughput", &pass, 1.0), ("compare", &measured, 60.0)]);
        assert_eq!(code, crate::exit::PASS);
        assert!(text.contains("meas  compare"), "{text}");
        assert!(
            text.contains("NOT JUDGED: compare (measured; no floor)"),
            "{text}"
        );
        assert!(!text.contains("GREEN — every lane"), "{text}");
        let (text, code) = verdict(&[("compare", &measured, 60.0)]);
        assert_eq!(code, crate::exit::PASS);
        assert!(
            text.contains("NOTHING JUDGED — compare (measured; no floor)"),
            "{text}"
        );
        let (text, code) = verdict(&[("throughput", &partial, 1.0), ("wasm", &skip, 0.1)]);
        assert_eq!(code, crate::exit::PASS);
        assert!(
            text.contains("NOTHING JUDGED — throughput (no baseline), wasm (skipped)"),
            "{text}"
        );
        let mut fenced = LaneRun::not_judged("no baseline");
        fenced.fence_held("resize");
        let (text, _) = verdict(&[("resize", &fenced, 1.0)]);
        assert!(
            text.contains("GREEN for what was judged; NOT JUDGED: resize floors (no baseline)"),
            "{text}"
        );
        let (text, code) = verdict(&[("throughput", &pass, 1.0), ("search", &fail, 2.0)]);
        assert_eq!(code, crate::exit::FAILED);
        assert!(text.contains("FAILED — search"), "{text}");
        let unrun = LaneRun::could_not_run(
            "no pinned chrome-headless-shell",
            "targo --unverified verify provision",
        );
        let (text, code) = verdict(&[("throughput", &pass, 1.0), ("wasm", &unrun, 0.1)]);
        assert_eq!(code, crate::exit::COULD_NOT_RUN);
        assert!(text.contains("NRUN  wasm"), "{text}");
        assert!(
            text.contains("verify perf: COULD NOT RUN — wasm could not run"),
            "{text}"
        );
        assert!(!text.contains("GREEN"), "{text}");
        let (text, code) = verdict(&[("search", &fail, 1.0), ("wasm", &unrun, 0.1)]);
        assert_eq!(code, crate::exit::FAILED);
        assert!(
            text.contains("FAILED — search; wasm could not run"),
            "{text}"
        );
    }

    #[test]
    fn add_failure_keeps_earlier_findings() {
        let mut run = LaneRun::pass();
        run.add_failure("a");
        run.add_failure("b");
        assert_eq!(run.status, Status::Fail(vec!["a".into(), "b".into()]));
    }
}
