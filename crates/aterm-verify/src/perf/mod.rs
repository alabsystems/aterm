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
//! Three kinds of lane. The FLOOR lanes ([`floors`], [`deck`], [`wasm`], [`vte_engine`],
//! [`vte_marks`], `cli_start`, [`fabric_counts`]) measure headless, opening no window, and
//! judge each metric against its committed baseline in
//! `crates/aterm-verify/perf/golden/`; whatever every one but `fabric-counts` measured then
//! meets the same-box trend ledger ([`trend`]), and a green run appends to it. `fabric-counts`
//! keeps no trend: a same-box ratio means nothing for its exact counts, which its baseline and
//! each count's law judge alone. A bare `verify perf` runs every floor lane. Each lane waits for a quiet machine first but
//! `vte-engine`, `vte-marks`, `cli-start` and `fabric-counts`, which run at the load they
//! find, report it, and judge cycles, instructions or exact counts, not time. The ARENA lanes
//! ([`arena`]) open real windows and take minutes; they run only when named, and what they measure
//! has no floor, so they report MEASURED, never a pass. The MEASURED lanes
//! ([`fabric`], [`restore_e2e`], [`aos`], [`commons`], [`journal`]) are headless but just as unjudgeable —
//! their numbers are a disk's fsync, or a shared desk's scheduler, before they
//! are aterm's — so they too run only when named and report MEASURED (fabric
//! writing a ledger entry under `docs/measured/fabric/`, restore-e2e one under
//! `docs/measured/restore/`, aos, commons-quiet and journal-note a summary, and JSON with `--json-out`).
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
#[cfg(target_os = "macos")]
mod cli_start;
#[cfg(unix)]
mod commons;
mod deck;
mod driver;
#[cfg(unix)]
mod fabric;
#[cfg(unix)]
mod fabric_counts;
mod floors;
#[cfg(unix)]
mod journal;
mod publication;
#[cfg(unix)]
mod restore_e2e;
#[cfg(unix)]
mod stats;
mod trend;
#[cfg(unix)]
pub(crate) mod vte_engine;
#[cfg(unix)]
mod vte_marks;
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

/// The checkout whose aterm-verify is running: the lanes build, measure and record against its
/// tree. Every reader runs under `targo run -p aterm-verify` (the `verify` alias, a release's
/// pilot lanes) or this crate's own tests, whose runner names this package's directory in that
/// checkout when it starts the process ([`aterm_tempfile::manifest_dir`]); the verbs the Linux
/// lane's guest runs bare (`browser-drive`, `provision`) read none.
pub(crate) fn workspace_root() -> PathBuf {
    let manifest = aterm_tempfile::manifest_dir();
    manifest
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| manifest.clone(), Path::to_path_buf)
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
    Deck,
    Wasm,
    VteEngine,
    VteMarks,
    CliStart,
    FabricCounts,
    Start,
    Compare,
    Cat,
    Scroll,
    Vte,
    Fabric,
    RestoreE2e,
    Aos,
    CommonsQuiet,
    JournalNote,
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
    pub(crate) const ALL: [Lane; 27] = [
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
        Lane::Deck,
        Lane::Wasm,
        Lane::VteEngine,
        Lane::VteMarks,
        Lane::CliStart,
        Lane::FabricCounts,
        Lane::Start,
        Lane::Compare,
        Lane::Cat,
        Lane::Scroll,
        Lane::Vte,
        Lane::Fabric,
        Lane::RestoreE2e,
        Lane::Aos,
        Lane::CommonsQuiet,
        Lane::JournalNote,
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
            Lane::Deck => "deck",
            Lane::Wasm => "wasm",
            Lane::VteEngine => "vte-engine",
            Lane::VteMarks => "vte-marks",
            Lane::CliStart => "cli-start",
            Lane::FabricCounts => "fabric-counts",
            Lane::Start => "start",
            Lane::Compare => "compare",
            Lane::Cat => "cat",
            Lane::Scroll => "scroll",
            Lane::Vte => "vte",
            Lane::Fabric => "fabric",
            Lane::RestoreE2e => "restore-e2e",
            Lane::Aos => "aos",
            Lane::CommonsQuiet => "commons-quiet",
            Lane::JournalNote => "journal-note",
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
            | Lane::Deck
            | Lane::Wasm
            | Lane::VteEngine
            | Lane::VteMarks
            | Lane::CliStart
            | Lane::FabricCounts => Kind::Floor,
            Lane::Start | Lane::Compare | Lane::Cat | Lane::Scroll | Lane::Vte => Kind::Arena,
            Lane::Fabric
            | Lane::RestoreE2e
            | Lane::Aos
            | Lane::CommonsQuiet
            | Lane::JournalNote => Kind::Measured,
        }
    }

    /// Whether the lane puts a window on the desktop: the lanes `--no-windows`
    /// skips. Every arena lane does; no other lane does.
    pub(crate) fn opens_windows(self) -> bool {
        self.kind() == Kind::Arena
    }

    /// Whether the lane waits for a quiet machine before it starts ([`quiet`]). `vte-engine`,
    /// `vte-marks`, `cli-start` and `fabric-counts` do not: they measure at the load they find,
    /// report that load beside their numbers, and are judged by what the clock and the core a
    /// sample ran on do not move (performance-core cycles; instructions retired; a world's exact
    /// structural counts).
    fn takes_quiet_claim(self) -> bool {
        !matches!(
            self,
            Lane::VteEngine | Lane::VteMarks | Lane::CliStart | Lane::FabricCounts
        )
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
                "trail-dark-composer's scene on Default Light, the trail's light arm at the \
                 live level (prism_glass::LIVE): emission, CPU raster, GPU encode and GPU time"
            }
            Lane::Deck => {
                "the stats deck's frame cost (deck_frame_cost: needle, grid and new-reading \
                 frames, Folded and Full at 2238 px), plus a headless idle take of it, reported"
            }
            Lane::Wasm => {
                "the shipped wasm modules in the pinned chrome-headless-shell, plus their \
                 module-size ceilings"
            }
            Lane::VteEngine => {
                "performance-core cycles and ms per MiB (min and median) of the pinned \
                 vtebench's default suite and the d1 ascii and unicode corpora replayed into \
                 Terminal::process at 120x40; no PTY, GUI or window, at the load it finds"
            }
            Lane::VteMarks => {
                "performance-core cycles and ms per MiB (min and median) of four mark-heavy \
                 corpora (NFD Latin, Hindi and Thai, conjoining Hangul jamo, emoji selectors, \
                 skin tones, joiners and flags) replayed into Terminal::process at 120x40: the \
                 write path's mark branch, at the load it finds"
            }
            Lane::CliStart => {
                "instructions retired by one fresh aterm process (`--version`, `ctl --help`, `ctl \
                 status` against a headless instance), split by empty programs linking none, all \
                 or all but the window-only of the dylibs aterm's load commands name, by one \
                 linking all and binding every symbol aterm imports, and by aterm stopped at \
                 `main`'s first call and past the front door's prelude (hidden verbs it refuses); \
                 at the load it finds"
            }
            Lane::FabricCounts => {
                "the bus campaign's structural wins as the fabric bench's exact counts, on its \
                 in-process broker at the load it finds: the attach's `@<sid> meta` calls at 8 \
                 and 32 sessions (0, E05), ping-pong's fleet-mailbox transactions (0, D01), \
                 messaging's refusals for the rate (0, E06) and each restart's cold reads (2N - \
                 1, then N - 1, D10d); no ratio and no time"
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
            Lane::CommonsQuiet => {
                "process CPU per converged loopback Commons repair round, both peers together"
            }
            Lane::JournalNote => {
                "one journal note's latency, the spool's single append: p50, p95 and p99"
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
            Lane::Deck => {
                "macOS, targo (opt-test build of aterm-gui's tests; attested debug builds of \
                 aterm and aterm-lab for the diagnostic idle take)"
            }
            Lane::Wasm => {
                "chrome-headless-shell (verify provision), the checkout's stock pin + \
                 wasm32-unknown-unknown, wasm-opt"
            }
            Lane::VteEngine => {
                "unix, targo (release build of aterm-bench), git and the network for the pinned \
                 vtebench's first clone; the vte lane's vtebench build to cross-check its \
                 payloads; macOS's per-core-type cycle counters to judge anything"
            }
            Lane::VteMarks => {
                "unix, targo (release build of aterm-bench); macOS's per-core-type cycle \
                 counters to judge anything"
            }
            Lane::CliStart => {
                "macOS, targo (release builds of aterm, unless --binary names one, and of \
                 aterm-start-controls; test-profile builds of aterm-lab and the current debug \
                 aterm for ctl-status's headless fixture)"
            }
            Lane::FabricCounts => {
                "unix, targo (builds aterm and its fabric_bench bench in the test profile, as \
                 `verify perf fabric` does)"
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
                "unix, targo (builds aterm and its fabric_bench bench: test, opt-test with \
                 --profile opt, or fabric-shipped with --profile shipped)"
            }
            Lane::RestoreE2e => {
                "macOS, targo (builds aterm and its restore_bench bench: test, or opt-test with \
                 --profile opt)"
            }
            Lane::Aos => {
                "unix, targo (builds aterm-bench's aos_harness twice: release, and opt-test \
                 for the stranded repair); macOS for its instruction, fault and syscall counters"
            }
            Lane::CommonsQuiet => "unix, targo (opt-test build of aterm-db's isolated repair test)",
            Lane::JournalNote => {
                "unix, targo (opt-test build of aterm-db's ignored journal_note measurement)"
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

    #[cfg(test)]
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
    Lane::CliStart,
    Lane::Start,
    Lane::Compare,
    Lane::Fabric,
    Lane::RestoreE2e,
    Lane::Aos,
    Lane::CommonsQuiet,
    Lane::JournalNote,
];
const BUILT_LANES: &[Lane] = &[Lane::Fabric, Lane::RestoreE2e];
const GHOSTTY_LANES: &[Lane] = &[Lane::Cat, Lane::Vte];
const BENCH_BOX_LANES: &[Lane] = &[Lane::Cat, Lane::Scroll, Lane::Vte];

const OPTIONS: &[OptSpec] = &[
    OptSpec {
        flag: "--size-only",
        value: None,
        lanes: &[Lane::Wasm],
        help: "judge optimized wasm module byte ceilings without running a browser or timing work",
    },
    OptSpec {
        flag: "--binary",
        value: Some("PATH"),
        lanes: &[Lane::Start, Lane::CliStart, Lane::Cat],
        help: "the aterm to time (start: default target/profiling/aterm, else \
               target/release/aterm; cat and cli-start: default a fresh release build)",
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
        lanes: &[Lane::Start, Lane::CliStart],
        help: "independent launches (start: default 7; cli-start: per arm, default 50)",
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
        help: "write measurements and provenance as JSON",
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
        help: "the attach world's backlog (default 0: none). In the bench's process (inproc): \
               records filled behind every attach session's refill watermark, after each one's \
               note is seen, with a paced publisher timing the writer's stall. With an outside \
               --broker-process: N records for each population seeded into the log before \
               `aterm link broker` opens it, enough in all for the cold reads (the lane refuses \
               fewer and names the least), no session's mail seen; two more opens beside it (an \
               empty log, the seed plaintext), the broker's own process read through each open, \
               the world and a wait for a retention tick, then timed cold-read passes over the \
               sealed and plaintext logs in the bench's process",
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
        flag: "--rate-limit",
        value: Some("on|off"),
        lanes: &[Lane::Fabric],
        help: "whether every world's broker limits each producer by aterm's default rate limit \
               (default on, what ships)",
    },
    OptSpec {
        flag: "--at-rest",
        value: Some("on|off,..."),
        lanes: &[Lane::Fabric],
        help: "whether every world's log is sealed at rest under a key, one run and one ledger \
               entry per word: `off,on --passes 2` is an A B B A set (default on, what ships)",
    },
    OptSpec {
        flag: "--broker-process",
        value: Some("inproc|background|interactive,..."),
        lanes: &[Lane::Fabric],
        help: "where every world's broker runs, one run and one ledger entry per word: in the \
               bench's process (default inproc), or as `aterm link broker` started with its \
               launchd job's argv as a Background job's task type (background, the band that \
               ships) or an Interactive one's (interactive), measuring the messaging, ping-pong \
               and attach worlds' outside stamps, and with --attach-backlog the broker's own \
               process over a seeded log: `background,interactive --passes 2` is an A B B A set",
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
        flag: "--asked-prefill",
        value: Some("N"),
        lanes: &[Lane::Fabric],
        help: "rows the ask scenario's asked table is pre-filled to before its legs, written as \
               an older build's table and taken in by a restarted bridge (default 0: none; at \
               most 1024, the table's cap)",
    },
    OptSpec {
        flag: "--paced-notes",
        value: Some("N"),
        lanes: &[Lane::Fabric],
        help: "one-way notes the messaging scenario posts after its burst, 1.1 s apart and none \
               answered, so each delivery owes the bus mirror its snapshot (default 0: none; at \
               most 545)",
    },
    OptSpec {
        flag: "--tally",
        value: None,
        lanes: &[Lane::Fabric],
        help: "boot the messaging, attach, handoff and ask worlds with the bridge's wait tally \
               and aterm's handler tally on, and report both tallies' rows for every round \
               (default off: the tallies' own writes move what they measure; the ping-pong \
               world counts with both in every run)",
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
        value: Some("dev|opt|shipped"),
        lanes: BUILT_LANES,
        help: "the build measured: dev (default), opt (cargo profile opt-test: opt-level 3, \
               debug assertions on) or, on the fabric lane, shipped (cargo profile \
               fabric-shipped with dev-seams: fat LTO, 1 codegen unit, debug assertions off)",
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
        flag: "--catalog",
        value: None,
        lanes: &[Lane::CommonsQuiet],
        help: "measure catalog-discovered application tables instead of one explicit table",
    },
    OptSpec {
        flag: "--long-log",
        value: None,
        lanes: &[Lane::CommonsQuiet],
        help: "measure the explicit table over a long vault history (1,200 rotations of the \
               vault's key, 9,605 records) instead of a short one",
    },
    OptSpec {
        flag: "--late-joiner",
        value: None,
        lanes: &[Lane::CommonsQuiet],
        help: "measure --long-log's history where the second Mac was admitted after it and opens \
               each older epoch through a handoff",
    },
    OptSpec {
        flag: "--large-table",
        value: None,
        lanes: &[Lane::CommonsQuiet],
        help: "measure the explicit table holding 3,000 rows of 1 KB instead of one, which each of \
               a round's store opens replays",
    },
    OptSpec {
        flag: "--sustained",
        value: None,
        lanes: &[Lane::CommonsQuiet],
        help: "measure 256 quiet repair rounds instead of 8, for sustained CPU profiling",
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
        help: "wait for a quiet machine before each lane but those that run at the load they \
               find (vte-engine, vte-marks, cli-start, fabric-counts; default wait, saying why \
               once a minute), or start each at once (now)",
    },
];

/// A parsed command line.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Cli {
    pub lanes: Vec<Lane>,
    pub record: Vec<Lane>,
    pub list: bool,
    pub help: bool,
    /// Compare floor lanes without reading or appending the same-box trend.
    pub no_trend: bool,
    /// Every option given, by flag (switches hold an empty value).
    pub options: BTreeMap<&'static str, String>,
}

impl Cli {
    fn runs_trend(&self, has_samples: bool) -> bool {
        has_samples && !self.no_trend
    }
    fn takes_quiet_claim(&self, lane: Lane) -> bool {
        lane.takes_quiet_claim()
            && !(lane == Lane::Wasm && self.options.contains_key("--size-only"))
    }
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
            "--no-trend" => cli.no_trend = true,
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
    if cli.record.contains(&Lane::Wasm) && cli.options.contains_key("--size-only") {
        return Err("--size-only cannot re-record wasm runtime floors".into());
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
    let workloads: Vec<&str> = ["--catalog", "--long-log", "--late-joiner", "--large-table"]
        .into_iter()
        .filter(|flag| cli.options.contains_key(flag))
        .collect();
    if workloads.len() > 1 {
        return Err(format!(
            "{} each name a commons-quiet workload; measure one at a time",
            workloads.join(" and ")
        ));
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
         --no-trend compares the requested lanes without reading or appending trend history.\n\
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
                 then the same-box trend; fabric-counts by its baseline and laws alone, with no \
                 trend)\n"
            }
            Kind::Arena => {
                "ARENA lanes (only when named; real windows, minutes; measured, never judged: \
                 cat, scroll and vte write a docs/measured/arena entry, start and compare print \
                 a summary, and JSON with --json-out)\n"
            }
            Kind::Measured => {
                "MEASURED lanes (only when named; headless, real processes, minutes; measured, \
                 not judged: fabric writes a docs/measured/fabric entry, restore-e2e a \
                 docs/measured/restore entry, aos, commons-quiet and journal-note print a summary, and JSON with --json-out)\n"
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

/// `cli-start`'s options, parsed before any lane runs, so a bad value is a usage error (exit 2):
/// the aterm to measure, the launches per arm and where its JSON goes.
#[cfg(target_os = "macos")]
fn cli_start_args(cli: &Cli) -> Result<cli_start::Args, String> {
    let samples = match cli.options.get("--samples") {
        Some(v) if cli.lanes.contains(&Lane::CliStart) => Some(
            v.parse::<usize>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("`--samples` takes a positive count, not `{v}`"))?,
        ),
        _ => None,
    };
    Ok(cli_start::Args {
        binary: cli.options.get("--binary").map(PathBuf::from),
        samples,
        json_out: cli.options.get("--json-out").map(PathBuf::from),
    })
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
    #[cfg(target_os = "macos")]
    let cli_start_args = match cli_start_args(&cli) {
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
        let quiet = match (!skip && cli.takes_quiet_claim(*lane))
            .then(|| quiet(*lane, admit))
            .transpose()
        {
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
            Lane::Deck => deck::gate_deck(record),
            Lane::Wasm if cli.options.contains_key("--size-only") => wasm::gate_wasm_size(),
            Lane::Wasm => wasm::gate_wasm(record),
            #[cfg(unix)]
            Lane::VteEngine => vte_engine::run(record),
            #[cfg(not(unix))]
            Lane::VteEngine => LaneRun::skip(
                "its payload capture runs vtebench's shell scripts under a unix PTY",
                "run it on macOS or Linux",
            ),
            #[cfg(unix)]
            Lane::VteMarks => vte_marks::run(record),
            #[cfg(not(unix))]
            Lane::VteMarks => LaneRun::skip(
                "its corpora live in the arena's cache and scratch, which are unix-only",
                "run it on macOS or Linux",
            ),
            #[cfg(target_os = "macos")]
            Lane::CliStart => cli_start::run(&cli_start_args, record),
            #[cfg(not(target_os = "macos"))]
            Lane::CliStart => LaneRun::skip(
                "it reads instructions retired from macOS's proc_pid_rusage",
                "run it on a Mac",
            ),
            #[cfg(unix)]
            Lane::FabricCounts => fabric_counts::run(record),
            #[cfg(not(unix))]
            Lane::FabricCounts => LaneRun::skip(
                "the fabric bridge is unix-only (aterm-link)",
                "run it on macOS or Linux",
            ),
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
            #[cfg(unix)]
            Lane::CommonsQuiet => commons::run(
                cli.options.get("--json-out").map(Path::new),
                cli.options.contains_key("--sustained"),
                cli.options.contains_key("--catalog"),
                cli.options.contains_key("--long-log"),
                cli.options.contains_key("--late-joiner"),
                cli.options.contains_key("--large-table"),
            ),
            #[cfg(not(unix))]
            Lane::CommonsQuiet => LaneRun::skip(
                "the Commons measurement uses unix process and loopback helpers",
                "run it on macOS or Linux",
            ),
            #[cfg(unix)]
            Lane::JournalNote => journal::run(cli.options.get("--json-out").map(Path::new)),
            #[cfg(not(unix))]
            Lane::JournalNote => LaneRun::skip(
                "the journal measurement runs through the unix build helpers",
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
    if cli.runs_trend(!samples.is_empty()) {
        let started = Instant::now();
        eprintln!("--- trend ---");
        let floors: Vec<&LaneRun> = ran
            .iter()
            .filter(|(l, _, _)| l.kind() == Kind::Floor)
            .map(|(_, r, _)| r)
            .collect();
        let lanes_ok = floors
            .iter()
            .all(|r| matches!(r.status, Status::Pass | Status::Recorded(_)));
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

/// Resource admission for one timing lane, alone among quiet claims once load and sampled CPU
/// are ready. Its build runs under this reservation; collectors sample again after compilation.
/// Immediate, forced, unknown and unavailable admission starts no acceptance callback.
///
/// # Errors
/// An interrupt ended the wait (WAIT-INTERRUPT): why, and the run ends COULD NOT RUN.
fn quiet(lane: Lane, mode: aterm_capacity::Admit) -> Result<Option<aterm_capacity::Claim>, String> {
    let need = aterm_capacity::Need::timing(format!("verify perf {}", lane.name()));
    match aterm_capacity::admit(need, mode) {
        Ok(claim) => {
            claim.timing_admission().map_err(|why| {
                format!(
                    "{}: unqualified admission: {why}; no acceptance callback",
                    lane.name()
                )
            })?;
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
        Err(e) => Err(format!(
            "{}: timing admission unavailable: {e}; no acceptance callback",
            lane.name()
        )),
    }
}

/// A new postcompile observation, never an outer lane's retained/precompile sample.
/// The returned guard stays alive through execution of the already built measurement artifact.
pub(crate) fn measurement_entry() -> Result<aterm_capacity::Claim, String> {
    let mut claim = aterm_capacity::admit(
        aterm_capacity::Need::timing("perf postcompile measurement entry"),
        aterm_capacity::Admit::Wait,
    )
    .map_err(|why| format!("measurement entry admission: {why}"))?;
    let window = claim
        .measurement_entry()
        .map_err(|why| format!("measurement entry not qualified: {why}"))?;
    let evidence = claim.record().timing;
    let expires = evidence
        .admitted_ms
        .zip(claim.record().need.patience)
        .and_then(|(at, p)| at.checked_add(u64::try_from(p.hold.as_millis()).ok()?));
    eprintln!(
        "  timing phase=postcompile_measurement_entry provider=sysprobe_core_ticks scope=sampled_global_cpu_window;not_continuous_quiet observed_ms={} interval_ns={} cores={} busy_ticks={} total_ticks={} entry_load1={:?} actual_admitted_ms={:?} expires_ms={expires:?}",
        window.observed_ms,
        window.interval_ns,
        window.cores,
        window.busy_ticks,
        window.total_ticks,
        evidence.entry_load1,
        evidence.admitted_ms
    );
    Ok(claim)
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
#[path = "mod_tests.rs"]
mod tests;
