// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ARENA lanes: real-window measurements of the one shipped `aterm`, and
//! head-to-heads against the pinned ghostty tip.
//!
//! # The arena — head-to-head measurement protocol
//!
//! The measurement arm of the faster-than-ghostty plan's §2
//! (`docs/FASTER_THAN_GHOSTTY_PLAN.md`, which `5fb7267b9cc` removed). One protocol, every
//! dimension, every competitor, one append-only ledger (see Ledger below). The runners are the
//! arena lanes of `verify perf` (this module and its children); `targo --unverified verify perf
//! --list` names them and what each needs.
//!
//! ## Rule zero — a run is INVALID unless all of these hold
//!
//! 1. **Build provenance.** Competitors run official release/nightly artifacts
//!    wherever they exist (ghostty: the `tip` GitHub release — the exact thing
//!    mitchellh benchmarks). The June onglass alacritty/kitty numbers came from
//!    source builds whose commands went unrecorded: never reuse them. Source
//!    builds record the exact command in the ledger (`zig build
//!    -Doptimize=ReleaseFast` — Zig defaults to Debug and runs ~2x slow; cargo
//!    profile + LTO flags for Rust peers). Every binary under test gets a sha256
//!    in the ledger. `crates/aterm-verify/perf/arena/manifest.toml`
//!    is the single source of pins.
//! 2. **Hardware + thermal.** Publishable head-to-heads run on the **M4 Max
//!    only** (mitchellh's hardware class). Anywhere else is dev-smoke: `cat`,
//!    `scroll` and `vte` SKIP there unless given `--dev-smoke`, and label every
//!    number from it `DEV-SMOKE, NON-PUBLISHABLE`; `start` and `compare` run
//!    anywhere and record the run class (and `compare` each automated rule-zero
//!    input) beside their numbers. Per trial: check thermal pressure (`pmset -g
//!    therm`), cool down between competitors, **interleave A/B ordering with
//!    rotation** (never all-of-A then all-of-B), record thermal state.
//! 3. **GUI protocol.** Window frontmost + focused + fully visible; same display,
//!    refresh rate recorded (ProMotion pinned on/off); Do Not Disturb on; App Nap
//!    verified inactive; no other apps running. (Ghostty stops its display link
//!    occluded; aterm suppresses effects unfocused — unfairness cuts both ways.)
//! 4. **Config parity.** All terminals on **shipping defaults**; any deviation is
//!    a ledger field. aterm publishes both default (effects as shipped) and
//!    effects-off numbers wherever effects plausibly cost.
//! 5. **Harness pinning.** Third-party harnesses (vtebench, DOOM-fire-zig,
//!    typometer) are pinned by commit in the manifest; corpora are pre-generated
//!    files, never piped from a generator mid-run (ghostty's own
//!    `src/benchmark/AGENTS.md` discipline: warmups + medians, identical
//!    dims/flags, never parallel).
//!
//! ## The lanes
//!
//! Every lane launches the ONE `aterm` (never a crate's dev-only thin binary)
//! with inherited `ATERM_*` overrides removed, stops it by its own process group
//! (never by a name a person's own windows could share), and caches what it
//! pre-generates — corpora, the sha-verified ghostty copy, the pinned vtebench —
//! under `$TMPDIR/aterm-arena/`, outside the checkout (cargo would claim the
//! vtebench checkout for this workspace). Control and report sockets live under
//! `/tmp/at.<pid>.<n>/`, short of the 103-byte socket-path limit whatever
//! `$TMPDIR` is. A terminal under test runs the in-terminal half, `aterm-verify
//! --arena-inner`, which answers on a report socket the runner waits on. Ctrl-C,
//! SIGTERM or SIGHUP retires every terminal the run launched and removes its
//! scratch directories before the run exits. An arena lane has no floor, so the
//! closing verdict reports it MEASURED (and where the numbers went), never a
//! pass.
//!
//! | Lane | Dimension | Status |
//! |---|---|---|
//! | `start` | ARENA-START: fresh-process real-window open time (GUI entry and Rust `main` → startup-metrics publication inside the first successful-present finalizer) | ready for local before/after runs; publishable claims still require rule zero + a ledger entry |
//! | `compare` | ARENA-START paired A/B: frozen artifacts, AB/BA rotation folded into order-balanced ABBA rounds, cooldown, raw distributions, per-metric resolution | ready; 20 pairs (10 rounds) and a five-second cooldown by default, the manual rule-zero controls left explicit |
//! | `cat` | D1: ARENA-CAT (cat 150 MB ascii + unicode, PTY drain) vs ghostty tip | ready — run on the M4 Max |
//! | `scroll` | ARENA-SCROLL: 100k+-line scrub + jump-to-top, windowed frame pacing (`max_frame_gap_ms`) | ready; only a dev-smoke M5-Max entry exists (2026-07-22). *Needed from the bench machine and the owner:* a rule-zero run plus the owner's manual ghostty screen recording (`docs/measured/arena/ISSUE-arena-scroll-m4max-publishable-run.md`) |
//! | `vte` | ARENA-VTE: vtebench's default suite, both terminals at one grid (default 120x40), interleaved A/B vs ghostty tip | **not yet run on the M4 Max**. The stock scripts' tty lookup is trimmed for macOS `ps` padding — untrimmed, `cursor_motion` and `light_cells` come out empty and vtebench drops them silently — so the lane refuses a trial that ran fewer than every benchmark, or at another grid, and keeps its file out of the medians. On macOS every payload is 80x24-shaped whatever the grid: vtebench pipes the scripts' stdout and stderr, which are the only places ncurses' `tput` reads the size from (`docs/measured/vte-engine-2026-10-29.md`, which `87da174f249` removed) |
//! | — | ARENA-FIRE (DOOM-fire-zig), ARENA-LAT (typometer), ARENA-CAST (asciinema corpus) | **no runner**, each for its own reason: ARENA-LAT is a manual GUI tool, ARENA-CAST's corpus is EXTERNAL (its `obtain` field in the manifest), and **ARENA-FIRE's method is not designed yet**: DOOM-fire-zig stops at a "Press return to continue..." prompt, loops until Ctrl-C, and prints its fps only into the frames it draws inside the terminal under test, which ghostty gives no control socket to read |
//!
//! The `cat` lane times `/bin/cat` alone, from inside the terminal under test.
//! The retired runner stamped the end of each drain from a freshly started
//! python, so the D1 entries before the port (2026-07-19/20) each include one
//! interpreter start, tens of milliseconds on a ~1 s drain: compare post-port
//! `cat` MB/s with those entries only in relative terms (each run's aterm
//! against its own ghostty), never in absolute ones.
//!
//! The engine-level, headless, floor-checked half of ARENA-SCROLL is the
//! `scroll-scrub` floor lane (baseline `crates/aterm-verify/perf/golden/perf-baseline-scroll.json`);
//! the `scroll` arena lane is the windowed half (frame pacing is observable only
//! in a real window). The historical PTY-drain and idle-memory sweep was
//! `docs/measured/onglass/README.md`, which `87da174f249` removed.
//!
//! ### ARENA-START
//!
//! Build the ONE `aterm` with the release or profiling profile, then:
//!
//! ```sh
//! targo --unverified verify perf start --samples 9 --backend cpu --binary target/profiling/aterm
//! targo --unverified verify perf compare \
//!   --baseline /abs/baseline-aterm --candidate /abs/candidate-aterm \
//!   --pairs 20 --backend gpu --json-out /tmp/aterm-start-ab.json
//! ```
//!
//! `--json-out` writes `aterm-start-v3` (`start`) or `aterm-start-compare-v2`
//! (`compare`). The `-v1` files in `docs/measured/arena/` came from the retired bash
//! runners and are shaped differently: v1 repeated each eligibility input as a
//! top-level flag and carried per-record hashes and the per-sample validity
//! fields; v2 keeps eligibility only under `automated_rule_zero_inputs` and the
//! run's RFC 3339 timestamps, harness and artifacts under `provenance`. v3 reads
//! GPU drill-down schema 2 (`startup_gpu_context_ms`, `startup_gpu_font_join_ms`,
//! `startup_gpu_assemble_ms`, `startup_gpu_tail_ms`) and drops v2's wgpu legs
//! (`startup_gpu_instance_ms`, `_adapter_ms`, `_device_ms`, `_context_tail_ms`,
//! `_pipelines_ms`), its `startup_gpu_pipe_*` keys and
//! `startup_gpu_cell_pipeline_ms`, which no binary has filled since GW.6.
//! Compare numbers within one schema.
//!
//! Every sample is a fresh process and a real macOS window, in fresh private
//! `HOME`/XDG directories with session restore and automatic updates off. The
//! observer waits for the socket without a request, settles 1.5 s, then makes
//! ONE `metrics --json` read. A sample is refused for `frames < 1`, a zero
//! `first_present_ms`, a present stamp not safely below the settle, a forced
//! backend that negotiated otherwise, a grid other than 110x32, a drop ledger
//! that disagrees with itself, or a partition that does not reconcile (the
//! eight startup phases with both totals, the eight attach subphases with their
//! parent, and — when the binary marks them valid — the worker and GPU
//! drill-downs, all within the 0.06 ms wire-rounding allowance). The binary is
//! copied to an immutable path and hashed before trial 1, so a concurrent
//! rebuild cannot mix revisions; installed `.app/Contents/MacOS/*` inputs are
//! refused.
//!
//! Two independent totals: first-present (time-to-CONTENT, from GUI entry and
//! from Rust `main`) and `rust_main_to_first_visible_ms` (time-to-VISIBLE, the
//! first window on glass). An early reveal moves the second and no phase of the
//! first, and may legally sit below it; `compare` compares it beside
//! first-present on the same round fold, so a reveal-ordering change is never
//! scored on the wrong axis. An arm that predates the reveal stamp drops that one
//! comparison; an arm missing the router, phase or attach schema needs
//! `--allow-legacy-metric` and leaves only the shared totals compared.
//!
//! `--warm` is a REACHABILITY switch: a cold launch's home has never held a
//! previous launch, so state-gated code (the L1 early reveal and its
//! cell-metrics hit) is absent from the cold lane. Warm primes each trial's
//! private home with a discarded launch, requires the cell-metrics cache it
//! leaves, and measures the second launch. Never compare a warm arm with a cold
//! one; the JSON records which.
//!
//! `compare` reports BY ROUND. Pairs 2k-1 and 2k are the quad A B B A, so each
//! round delta gives both arms mean slot position 2.5 and one repeat and one
//! switch launch: any offset that depends on slot position cancels inside the
//! unit. Each metric prints the 95% t interval on the round mean and the
//! resolution the run earned (this lane's identical-binary controls read -1.36
//! and +1.54 ms — what a ±2 ms resolution looks like when a bare median is
//! quoted). Pair deltas stay in the JSON as `paired_candidate_faster_ms`,
//! flagged `order_contaminated`. The automated rule-zero inputs (exact `Apple M4
//! Max`, ≥20 pairs, ≥5 s cooldown, every thermal reading the clean triple, not a
//! negative control) are recorded separately; display, focus, App Nap, DND and
//! the ledger stay manual, so no run is labelled publishable on automation
//! alone.
//!
//! ## Ledger
//!
//! One file per run: `<YYYY-MM-DD>-<dim>.md` from [`TEMPLATE`], in the verify
//! state directory (`<target>/verify/.ubd/perf.noindex/arena/`, [`crate::perf::ledger_dir`]);
//! the `cat`, `scroll` and `vte` lanes write the entry and leave the manual
//! fields for you. Append-only; a superseding run gets a new file, never an
//! edit. Every entry carries: date, machine, macOS version, display + refresh,
//! thermal states, competitor versions + sha256s + build provenance, config
//! deviations, harness commits, raw numbers, and the medians table. No entry,
//! no claim; an entry is local state, and the number a claim rests on goes in the
//! lab tracker with the command that produced it (`goals.py measure`).
//!
//! ## Standing triggers (plan §6)
//!
//! - Every ghostty **stable** release → full arena sweep.
//! - **Monthly** → ARENA-CAT (`cat`) vs ghostty tip; ARENA-FIRE joins it as the
//!   cheap pair once its runner exists.
//! - **mitchellh posts a benchmark chart → D1-style re-measure within a week.**
//! - Before every aterm release → full sweep.
//! - A dimension that flips against us is a P0 by definition.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::exec::check_command as command;
use aterm_uds::spawnfd::Command;

use crate::perf::LaneRun;
use crate::perf::workspace_root;

pub(crate) mod cat;
pub(crate) mod compare;
pub(crate) mod inner;
pub(crate) mod live;
pub(crate) mod scroll;
pub(crate) mod start;
pub(crate) mod vte;

pub(crate) use live::Group;
pub(crate) use start::BackendRequest;

/// Why an arena lane stopped short: a fact about this box it cannot measure
/// past (a SKIP, with the fix), or a failure.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Halt {
    Skip { why: String, fix: String },
    Timing(String),
    Fail(String),
}

impl Halt {
    pub(crate) fn skip(why: impl Into<String>, fix: impl Into<String>) -> Halt {
        Halt::Skip {
            why: why.into(),
            fix: fix.into(),
        }
    }
}

impl From<String> for Halt {
    fn from(e: String) -> Halt {
        Halt::Fail(e)
    }
}

impl From<&str> for Halt {
    fn from(e: &str) -> Halt {
        Halt::Fail(e.to_string())
    }
}

/// An arena lane's outcome.
pub(crate) fn outcome(result: Result<String, Halt>) -> LaneRun {
    match result {
        Ok(at) => LaneRun::measured(at),
        Err(Halt::Skip { why, fix }) => LaneRun::skip(why, fix),
        Err(Halt::Fail(e)) => LaneRun::fail(e),
        Err(Halt::Timing(why)) => {
            LaneRun::could_not_run(why, "obtain fresh measured timing admission")
        }
    }
}

/// The arena lanes' options, parsed and range-checked.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Args {
    pub binary: Option<PathBuf>,
    pub baseline: Option<PathBuf>,
    pub candidate: Option<PathBuf>,
    pub samples: Option<u32>,
    pub pairs: Option<u32>,
    pub backend: BackendRequest,
    pub settle_ms: Option<u64>,
    pub timeout_s: Option<u64>,
    pub cooldown_s: Option<f64>,
    pub json_out: Option<PathBuf>,
    pub allow_identical: bool,
    pub allow_legacy_metric: bool,
    pub warm: bool,
    pub dev_smoke: bool,
    pub dmg: Option<PathBuf>,
    pub trials: Option<u32>,
    pub fill_lines: Option<u64>,
    pub scrub_steps: Option<u32>,
    pub columns: Option<u32>,
    pub rows: Option<u32>,
    pub max_secs: Option<u32>,
}

fn positive<T: std::str::FromStr + PartialOrd + Default>(
    flag: &str,
    raw: &str,
) -> Result<T, String> {
    match raw.parse::<T>() {
        Ok(v) if v > T::default() => Ok(v),
        _ => Err(format!("`{flag}` needs a positive integer, got `{raw}`")),
    }
}

impl Args {
    /// Parse the options the arena lanes read.
    pub(crate) fn from_options(options: &BTreeMap<&'static str, String>) -> Result<Args, String> {
        let mut a = Args::default();
        for (flag, raw) in options {
            let path = || Some(PathBuf::from(raw));
            match *flag {
                "--binary" => a.binary = path(),
                "--baseline" => a.baseline = path(),
                "--candidate" => a.candidate = path(),
                "--json-out" => a.json_out = path(),
                "--dmg" => a.dmg = path(),
                "--samples" => a.samples = Some(positive(flag, raw)?),
                "--pairs" => a.pairs = Some(positive(flag, raw)?),
                "--timeout-s" => a.timeout_s = Some(positive(flag, raw)?),
                "--trials" => a.trials = Some(positive(flag, raw)?),
                "--fill-lines" => a.fill_lines = Some(positive(flag, raw)?),
                "--scrub-steps" => a.scrub_steps = Some(positive(flag, raw)?),
                "--columns" => a.columns = Some(positive(flag, raw)?),
                "--rows" => a.rows = Some(positive(flag, raw)?),
                "--max-secs" => a.max_secs = Some(positive(flag, raw)?),
                "--settle-ms" => {
                    a.settle_ms = Some(raw.parse().map_err(|_| {
                        format!("`--settle-ms` needs a non-negative integer, got `{raw}`")
                    })?);
                }
                "--cooldown-s" => {
                    let v: f64 = raw.parse().map_err(|_| {
                        format!("`--cooldown-s` needs a non-negative number, got `{raw}`")
                    })?;
                    if !(v.is_finite() && v >= 0.0) {
                        return Err(format!(
                            "`--cooldown-s` needs a non-negative number, got `{raw}`"
                        ));
                    }
                    a.cooldown_s = Some(v);
                }
                "--backend" => {
                    a.backend = match raw.as_str() {
                        "auto" => BackendRequest::Auto,
                        "cpu" => BackendRequest::Cpu,
                        "gpu" => BackendRequest::Gpu,
                        _ => return Err(format!("`--backend` is auto, cpu or gpu, got `{raw}`")),
                    };
                }
                "--allow-identical" => a.allow_identical = true,
                "--allow-legacy-metric" => a.allow_legacy_metric = true,
                "--warm" => a.warm = true,
                "--dev-smoke" => a.dev_smoke = true,
                "--no-windows" | "--admit" => {}
                other => return Err(format!("internal: `{other}` has no parser")),
            }
        }
        Ok(a)
    }
}

/// The arena's cache directory reused across runs outside the checkout.
pub(crate) fn arena_dir() -> Result<PathBuf, String> {
    let dir = aterm_tempfile::quiet_in(&std::env::temp_dir()).join("aterm-arena.noindex");
    if dir.starts_with(workspace_root()) {
        return Err(format!(
            "the arena cache {} is inside the checkout ($TMPDIR points into it); cargo would \
             claim the vtebench checkout as a workspace member. Point TMPDIR elsewhere.",
            dir.display()
        ));
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// The longest unix-socket path the kernel takes (`sun_path` less its NUL).
pub(crate) const MAX_SUN_PATH: usize = 103;

/// Where a run's sockets live, whatever `$TMPDIR` is: macOS's per-user
/// `$TMPDIR` alone is 49 bytes before a run adds its HOME tree to it.
const SOCKET_BASE: &str = "/tmp";

/// `dir/name`, refused up front when it would not fit in `sun_path`.
pub(crate) fn socket_in(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let path = dir.join(name);
    let len = path.as_os_str().len();
    if len > MAX_SUN_PATH {
        return Err(format!(
            "the socket path {} is {len} bytes; a unix socket takes at most {MAX_SUN_PATH}",
            path.display()
        ));
    }
    Ok(path)
}

/// A private directory for one run, removed on drop and on interrupt.
pub(crate) struct Scratch(PathBuf);

impl Scratch {
    /// Under `$TMPDIR`: a run's private HOME trees, logs and staged binaries.
    pub(crate) fn new(tag: &str) -> Result<Scratch, String> {
        live::make_dir(&std::env::temp_dir(), &format!("aterm-{tag}-")).map(Scratch)
    }

    /// Under `/tmp`'s quiet parent, named `at.<pid>.<n>.noindex`: a run's unix sockets, short enough
    /// for every name a lane gives one ([`Scratch::sock`]).
    pub(crate) fn sockets() -> Result<Scratch, String> {
        live::make_dir(Path::new(SOCKET_BASE), "at.").map(Scratch)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    /// A socket path in this directory, checked against [`MAX_SUN_PATH`].
    pub(crate) fn sock(&self, name: &str) -> Result<PathBuf, String> {
        socket_in(&self.0, name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        live::remove_dir(&self.0);
    }
}

/// Owner-only permissions on a directory the run's state lives in.
pub(crate) fn private(dir: &Path) -> Result<(), String> {
    crate::set_dir_mode_nofollow(dir, 0o700)
        .map_err(|e| format!("could not chmod {}: {e}", dir.display()))
}

/// The lowercase hex SHA-256 of a file, streamed.
pub(crate) fn sha256_file(path: &Path) -> Result<String, String> {
    let file =
        crate::open_regular(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    sha256_opened(file, path)
}

/// The lowercase hex SHA-256 of an already-opened regular file handle, streamed.
pub(crate) fn sha256_opened(mut file: std::fs::File, path: &Path) -> Result<String, String> {
    let mut hasher = aterm_digest::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(aterm_codec::hex::encode_lower(&hasher.finalize()))
}

/// A program's trimmed stdout, or `None` when it cannot run or fails.
pub(crate) fn stdout_of(cmd: &mut Command) -> Option<String> {
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The CPU, as `sysctl` names it (the architecture where it cannot).
pub(crate) fn chip() -> String {
    stdout_of(command(aterm_uds::os_path!("sysctl")).args(["-n", "machdep.cpu.brand_string"]))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::env::consts::ARCH.to_string())
}

/// `pmset -g therm`, its non-empty lines joined by ` | `, or `unavailable`.
pub(crate) fn thermal() -> String {
    let text = stdout_of(command("pmset").args(["-g", "therm"])).unwrap_or_default();
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        "unavailable".to_string()
    } else {
        lines.join(" | ")
    }
}

/// Where a run sits under rule zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Machine {
    pub chip: String,
    /// Off the M4 Max bench box: every number is labelled non-publishable.
    pub dev_smoke: bool,
}

impl Machine {
    pub(crate) fn label(&self) -> String {
        if self.dev_smoke {
            format!("{} — DEV-SMOKE, NON-PUBLISHABLE", self.chip)
        } else {
            self.chip.clone()
        }
    }
}

/// Rule zero §2's hardware (this module's documentation): publishable
/// head-to-heads run on the M4 Max only, as `sysctl` names it exactly.
pub(crate) fn rule_zero_hardware(chip: &str) -> bool {
    chip.trim() == "Apple M4 Max"
}

/// The bench-box check: publishable head-to-heads run on [`rule_zero_hardware`]
/// only, and anywhere else only when the caller says `--dev-smoke`.
pub(crate) fn bench_box(chip: &str, dev_smoke: bool) -> Result<Machine, Halt> {
    let bench_class = rule_zero_hardware(chip);
    if !bench_class && !dev_smoke {
        return Err(Halt::skip(
            format!("this is '{chip}', not the M4 Max bench machine"),
            "re-run with --dev-smoke to measure here, every number labelled non-publishable",
        ));
    }
    Ok(Machine {
        chip: chip.to_string(),
        dev_smoke: !bench_class,
    })
}

/// [`bench_box`] for this machine and the ledger its entry goes to, with the
/// thermal reading and the manual rule-zero reminders printed.
pub(crate) fn preflight(dev_smoke: bool) -> Result<(Machine, LedgerBook), Halt> {
    if !cfg!(target_os = "macos") {
        return Err(Halt::skip(
            "the head-to-head lanes open macOS windows",
            "run them on the bench Mac",
        ));
    }
    let machine = bench_box(&chip(), dev_smoke)?;
    let ledger = LedgerBook::open()?;
    if machine.dev_smoke {
        eprintln!(
            "== DEV-SMOKE RUN on '{}' — NON-PUBLISHABLE ==",
            machine.chip
        );
    }
    eprintln!("thermal: {}", thermal());
    eprintln!(
        "REMINDERS (rule zero §3): Do Not Disturb ON, no other apps, each window frontmost \
         and visible — do not touch the machine during trials."
    );
    Ok((machine, ledger))
}

/// The pins every arena run names, from `crates/aterm-verify/perf/arena/manifest.toml`.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub(crate) struct Pins {
    pub ghostty: GhosttyPins,
    pub harness: HarnessPins,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub(crate) struct GhosttyPins {
    pub tip: GhosttyTip,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub(crate) struct GhosttyTip {
    pub artifact: String,
    pub artifact_sha256: String,
    /// What the tip build self-identifies as (`CFBundleShortVersionString`).
    pub bundle_version: String,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub(crate) struct HarnessPins {
    pub vtebench: RepoPin,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub(crate) struct RepoPin {
    pub repo: String,
    pub commit: String,
}

/// The arena's pins, in the running checkout ([`super::workspace_root`]).
pub(crate) fn manifest_path() -> PathBuf {
    aterm_tempfile::manifest_dir().join("perf/arena/manifest.toml")
}

pub(crate) fn pins() -> Result<Pins, String> {
    let path = manifest_path();
    let text = crate::read_regular_text(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    aterm_toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The pinned ghostty tip, sha-verified and installed under the arena cache.
#[derive(Debug, Clone)]
pub(crate) struct Ghostty {
    pub app: PathBuf,
    pub commit: String,
    pub sha256: String,
}

/// A Ghostty tip DMG is measured in tens of MiB. Bound a replacement before its pinned digest is
/// checked, without making an elapsed-time budget stand in for transfer progress.
const GHOSTTY_DMG_CAP: u64 = 1 << 30;

/// Fetch the pinned DMG into its quarantine path. The shared transport keeps its own resumable
/// `<quarantine>.part` on a transient failure; only the caller's digest check can move
/// `quarantine` onto the arena's cache path.
fn download_ghostty_dmg_with(
    url: &str,
    quarantine: &Path,
    fetch: impl FnOnce(&str, &Path, u64) -> Result<(), String>,
) -> Result<(), Halt> {
    fetch(url, quarantine, GHOSTTY_DMG_CAP).map_err(|why| {
        Halt::skip(
            format!("the pinned ghostty dmg could not be downloaded: {why}"),
            "check the network, or pass --dmg PATH",
        )
    })
}

fn download_ghostty_dmg(url: &str, quarantine: &Path) -> Result<(), Halt> {
    download_ghostty_dmg_with(
        url,
        quarantine,
        aterm_update_core::download_to_resumable_https_only,
    )
}

pub(crate) fn ghostty_tip(pins: &Pins, dmg: Option<&Path>) -> Result<Ghostty, Halt> {
    let tip = &pins.ghostty.tip;
    let arena = arena_dir()?;
    let mut verified = false;
    let dmg = match dmg {
        Some(d) => d.to_path_buf(),
        None => {
            let d = arena.join(format!("Ghostty-tip-{}.dmg", tip.bundle_version));
            if !d.is_file() {
                eprintln!("downloading {}", tip.artifact);
                let part = d.with_extension("part");
                download_ghostty_dmg(&tip.artifact, &part)?;
                if sha256_file(&part)? != tip.artifact_sha256 {
                    let _ = std::fs::remove_file(&part);
                    return Err(Halt::Fail(format!(
                        "{} served a dmg that does not hash to the pinned sha256, so it is not \
                         the pinned build, and nothing was cached; {} names it by its \
                         per-commit artifact, never the rolling `tip` release, which moves.",
                        tip.artifact,
                        manifest_path().display()
                    )));
                }
                std::fs::rename(&part, &d)
                    .map_err(|e| format!("could not place {}: {e}", d.display()))?;
                verified = true;
            }
            d
        }
    };
    if !verified && sha256_file(&dmg)? != tip.artifact_sha256 {
        return Err(Halt::Fail(format!(
            "{} does not hash to the pinned sha256, so it is not the pinned build{}; {} names \
             it by its per-commit artifact, never the rolling `tip` release, which moves.",
            dmg.display(),
            if dmg.starts_with(&arena) {
                " (a download an earlier run cached: remove it, and the next run downloads the \
                 pinned artifact)"
            } else {
                ""
            },
            manifest_path().display()
        )));
    }
    let app = arena.join(format!("Ghostty-tip-{}.app", tip.bundle_version));
    if !app.is_dir() {
        let part = app.with_extension("part");
        let _ = std::fs::remove_dir_all(&part);
        let mount = Scratch::new("ghostty-dmg")?;
        run_ok(
            command(aterm_uds::os_path!("hdiutil"))
                .args(["attach", "-nobrowse", "-quiet", "-readonly", "-mountpoint"])
                .arg(mount.path())
                .arg(&dmg),
            "hdiutil attach",
        )?;
        let copied = run_ok(
            command(aterm_uds::os_path!("cp"))
                .arg("-R")
                .arg(mount.path().join("Ghostty.app"))
                .arg(&part),
            "cp",
        );
        let _ = command(aterm_uds::os_path!("hdiutil"))
            .args(["detach", "-quiet"])
            .arg(mount.path())
            .status();
        copied?;
        std::fs::rename(&part, &app)
            .map_err(|e| format!("could not place {}: {e}", app.display()))?;
    }
    let plist = app.join("Contents/Info.plist");
    let version = stdout_of(
        command(aterm_uds::os_path!("defaults"))
            .arg("read")
            .arg(&plist)
            .arg("CFBundleShortVersionString"),
    )
    .unwrap_or_default();
    if version != tip.bundle_version {
        return Err(Halt::Fail(format!(
            "the installed ghostty self-identifies as '{version}', the pin is '{}'",
            tip.bundle_version
        )));
    }
    eprintln!("ghostty tip verified: {version}");
    Ok(Ghostty {
        app,
        commit: tip.bundle_version.clone(),
        sha256: tip.artifact_sha256.clone(),
    })
}

/// Run `cmd` and require success.
pub(crate) fn run_ok(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = cmd
        .output()
        .map_err(|e| format!("could not run {what}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{what} exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim_end()
        ))
    }
}

/// The one shipped executable under test, release-built, with provenance.
#[derive(Debug, Clone)]
pub(crate) struct Aterm {
    pub bin: PathBuf,
    pub sha256: String,
    /// `HEAD`'s short sha, `-dirty` when the tree has changes.
    pub git: String,
}

impl Aterm {
    pub(crate) fn provenance(&self, how: &str) -> String {
        format!("{} sha256={} ({how})", self.git, self.sha256)
    }
}

/// `targo --unverified build --release -p aterm`, then the binary's identity.
pub(crate) fn build_aterm() -> Result<Aterm, String> {
    let driver = crate::perf::driver::cargo_driver()?;
    let args = ["--release", "-q", "-p", "aterm"];
    eprintln!("$ {}", driver.display_workspace("build", &args));
    let status = driver
        .workspace_command("build")?
        .args(args)
        .current_dir(workspace_root())
        .status()
        .map_err(|e| format!("could not run targo: {e}"))?;
    if !status.success() {
        return Err(format!(
            "the aterm release build exited {:?}",
            status.code()
        ));
    }
    let bin = release_dir()?.join("aterm");
    Ok(Aterm {
        sha256: sha256_file(&bin)?,
        git: git_provenance(&workspace_root()),
        bin,
    })
}

/// An explicitly supplied product is the entire subject of this run. In particular, a VM
/// qualification must never replace a profile arm's bundle with a fresh default-profile build.
pub(crate) fn supplied_or_build(binary: Option<&Path>) -> Result<Aterm, String> {
    supplied_or(binary, build_aterm)
}

fn supplied_or(
    binary: Option<&Path>,
    build: impl FnOnce() -> Result<Aterm, String>,
) -> Result<Aterm, String> {
    let Some(binary) = binary else {
        return build();
    };
    let bin = std::fs::canonicalize(binary).map_err(|e| format!("{}: {e}", binary.display()))?;
    Ok(Aterm {
        sha256: sha256_file(&bin)?,
        git: "supplied binary".into(),
        bin,
    })
}

#[cfg(test)]
#[path = "mod_supplied_tests.rs"]
mod supplied_tests;

/// The checkout at `dir`: `HEAD`'s short sha, `-dirty` when its tracked files
/// have changes, `unknown` when git cannot say.
pub(crate) fn git_provenance(dir: &Path) -> String {
    let sha = stdout_of(
        command(aterm_uds::os_path!("git"))
            .args(["rev-parse", "--short=12", "HEAD"])
            .current_dir(dir),
    )
    .unwrap_or_else(|| "unknown".into());
    let clean = command(aterm_uds::os_path!("git"))
        .args(["diff", "--quiet"])
        .current_dir(dir)
        .status()
        .is_ok_and(|s| s.success());
    if clean { sha } else { format!("{sha}-dirty") }
}

/// Where the release build lands.
fn release_dir() -> Result<PathBuf, String> {
    Ok(target_dir()?.join("release"))
}

/// The target directory ([`crate::perf::driver::target_dir`]).
pub(crate) fn target_dir() -> Result<PathBuf, String> {
    crate::perf::driver::target_dir()
}

/// Strip every inherited `ATERM_*` override but the verify run's mark from a
/// terminal under test ([`aterm_uds::process::stripped_from_child`]): a caller's
/// terminal-specific settings must not change the run.
pub(crate) fn without_aterm_env(cmd: &mut Command) -> &mut Command {
    for (key, _) in std::env::vars_os() {
        if aterm_uds::process::stripped_from_child(&key) {
            cmd.env_remove(key);
        }
    }
    cmd
}

/// [`without_aterm_env`] for an aterm under test whose HOME is `home`, a scratch world: the
/// world's marker ([`aterm_types::world::MARKER`] at [`aterm_types::world::state_home`]), set
/// after the strip so it is the world's. A release build reads no development seam, and the
/// lanes that reach the local network (its `_alab._tcp` browse, its converge runner) read the
/// marker raw. The folder is where a release build keeps its state under that HOME anyway.
/// # Errors
/// This platform keeps no aterm state under a HOME.
pub(crate) fn in_scratch_world<'c>(
    cmd: &'c mut Command,
    home: &Path,
) -> Result<&'c mut Command, String> {
    let state = aterm_types::world::state_home(home)
        .ok_or_else(|| "this platform keeps no aterm state under a world's HOME".to_owned())?;
    Ok(without_aterm_env(cmd).env(aterm_types::world::MARKER, state))
}

/// A scratch world's `aterm.toml`: `head`'s root keys, then the tables that turn the world's
/// automatic lanes off ([`aterm_types::world::LANES_OFF`]). They act for the login's account
/// (update, packages, the keeper, the swarm's local-network uses), which no run under test is,
/// and are the same in every run and every lane, so no comparison turns on them.
pub(crate) fn scratch_config(head: &str) -> String {
    format!("{head}\n{}", aterm_types::world::LANES_OFF)
}

fn deadline_after(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait)
        .unwrap_or_else(|| now + Duration::from_secs(365 * 24 * 3600))
}

/// Wait for a launched instance's control socket: `Ok` once the path exists,
/// an error if the leader exits first or `timeout` passes.
pub(crate) fn await_socket(
    group: &mut Group,
    sock: &Path,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = deadline_after(timeout);
    loop {
        if let Some(status) = group.exited() {
            return Err(format!(
                "aterm exited before creating its socket ({status})"
            ));
        }
        if sock.exists() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "control socket {} did not appear within {timeout:?}",
                sock.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `<aterm> ctl --sock <sock> …`: the instance's own executable as its client.
pub(crate) fn ctl(bin: &Path, sock: &Path) -> Command {
    let mut cmd = command(bin);
    cmd.arg("ctl").arg("--sock").arg(sock);
    cmd
}

/// How long a `ctl` client may run past its verb's own `timeout=`: the hang
/// detector for a client that wedges, never a latency budget (`start`'s own).
const CTL_HUNG: Duration = Duration::from_secs(60);

/// The verb's own `timeout=<ms>`, which the instance bounds its answer by; none
/// when it names none.
fn verb_timeout(verb: &[&str]) -> Duration {
    verb.iter()
        .find_map(|a| a.strip_prefix("timeout=")?.parse::<u64>().ok())
        .map_or(Duration::ZERO, Duration::from_millis)
}

/// Run one `ctl` verb and return its stdout, or the failure.
pub(crate) fn ctl_out(bin: &Path, sock: &Path, verb: &[&str]) -> Result<String, String> {
    let deadline = deadline_after(verb_timeout(verb).saturating_add(CTL_HUNG));
    let out = aterm_uds::deadline::output_until(ctl(bin, sock).args(verb), deadline)
        .map_err(|e| format!("ctl {}: {e}", verb.join(" ")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "ctl {} exited {:?}: {}",
            verb.join(" "),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim_end()
        ))
    }
}

/// The socket an in-terminal half ([`inner`]) reports on: the runner waits on
/// the report itself — an event — with a hang ceiling, never on a file poll.
pub(crate) struct Inbox {
    path: PathBuf,
    rx: mpsc::Receiver<String>,
    _dir: Scratch,
}

impl Inbox {
    pub(crate) fn bind() -> Result<Inbox, String> {
        let dir = Scratch::sockets()?;
        let path = dir.sock("report.sock")?;
        let listener = UnixListener::bind(&path)
            .map_err(|e| format!("could not listen on {}: {e}", path.display()))?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            ) {
                let mut text = String::new();
                let _ = (&mut stream).take(1 << 20).read_to_string(&mut text);
                let _ = tx.send(text);
            }
        });
        Ok(Inbox {
            path,
            rx,
            _dir: dir,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The one report, or an error when nothing arrives within `ceiling`.
    pub(crate) fn wait(&self, ceiling: Duration) -> Result<String, String> {
        self.rx
            .recv_timeout(ceiling)
            .map(|t| t.trim().to_string())
            .map_err(|_| format!("no report on {} within {ceiling:?}", self.path.display()))
    }
}

impl Drop for Inbox {
    fn drop(&mut self) {
        let _ = UnixStream::connect(&self.path);
    }
}

/// Send one report line to the runner's [`Inbox`].
pub(crate) fn report(sock: &Path, line: &str) -> Result<(), String> {
    let mut stream = UnixStream::connect(sock)
        .map_err(|e| format!("could not reach the runner at {}: {e}", sock.display()))?;
    stream
        .write_all(line.as_bytes())
        .map_err(|e| format!("could not report: {e}"))
}

/// The cooldown between competitors (rule zero §2) — a protocol interval.
pub(crate) const COOLDOWN: Duration = Duration::from_secs(5);

/// An arena ledger entry's head, its `{{…}}` fields filled from a [`Ledger`] and the fields
/// marked TODO left for the person who ran it ([`fill_template`]).
pub(crate) const TEMPLATE: &str = r"<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Arena: {{DIM}} — {{DATE}}

Protocol: `crates/aterm-verify/src/perf/arena/mod.rs` rule zero. Append-only — a superseding
run gets a new file. No field left TODO in an entry a claim rests on.

- **Machine:** {{MACHINE}}
- **macOS:** {{MACOS}}
- **Display / refresh:** TODO (ProMotion on/off)
- **Thermal state per trial:** TODO (`pmset -g therm` before each)
- **aterm:** {{ATERM}}
- **Competitors:** {{COMPETITORS}}
- **Config deviations from shipping defaults:** none / TODO
- **Harness pins:** see `crates/aterm-verify/perf/arena/manifest.toml` at this commit
- **Effects:** aterm default (shipped effects on); effects-off delta: TODO if relevant

## Results (medians; raw trials inline)

| workload | aterm | competitor | raw |
|---|---|---|---|
";

/// The fields a ledger entry fills into [`TEMPLATE`].
pub(crate) struct Ledger<'a> {
    pub date: &'a str,
    pub dim: &'a str,
    pub machine: &'a str,
    pub macos: &'a str,
    pub aterm: &'a str,
    pub competitors: &'a str,
}

/// The template with its `{{…}}` fields filled.
pub(crate) fn fill_template(template: &str, l: &Ledger<'_>) -> String {
    template
        .replace("{{DATE}}", l.date)
        .replace("{{DIM}}", l.dim)
        .replace("{{MACHINE}}", l.machine)
        .replace("{{MACOS}}", l.macos)
        .replace("{{ATERM}}", l.aterm)
        .replace("{{COMPETITORS}}", l.competitors)
}

/// The append-only arena ledger, in the verify state directory
/// ([`crate::perf::ledger_dir`] `arena`).
pub(crate) struct LedgerBook {
    dir: PathBuf,
}

impl LedgerBook {
    /// The ledger folder, made before a lane runs a trial, so a run whose entry has nowhere to
    /// go fails before it spends its trials (as the fabric and restore lanes do).
    pub(crate) fn open() -> Result<LedgerBook, Halt> {
        let dir = crate::perf::ledger_dir("arena");
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not make {}: {e}", dir.display()))?;
        Ok(LedgerBook { dir })
    }

    /// Write a new entry `<date>-<slug>.md` (append-only: a second run on one
    /// day gets its own file), returning its path.
    pub(crate) fn write(
        &self,
        slug: &str,
        ledger: &Ledger<'_>,
        body: &str,
    ) -> Result<PathBuf, String> {
        let mut text = fill_template(TEMPLATE, ledger);
        text.push_str(body);
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("could not make {}: {e}", self.dir.display()))?;
        let mut path = self.dir.join(format!("{}-{slug}.md", ledger.date));
        let mut n = 2;
        while path.exists() {
            path = self.dir.join(format!("{}-{slug}-{n}.md", ledger.date));
            n += 1;
        }
        crate::write_fresh(&path, text)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        eprintln!(
            "ledger entry written: {} — fill in the display/thermal/config fields before a \
             claim rests on it.",
            path.display()
        );
        Ok(path)
    }
}

pub(crate) fn macos_version() -> String {
    stdout_of(command(aterm_uds::os_path!("sw_vers")).arg("-productVersion"))
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
