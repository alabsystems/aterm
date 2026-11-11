// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rewind's cost on the release binary: parse throughput and resident size with recording on against recording off.

use std::path::{Path, PathBuf};

use super::Rig;
use super::instance::{
    Band, Budget, CouldNotRun, Instance, Scratch, World, could_not_run, field, unanswered,
};
use super::starvation::{Disposition, ShapeVerdict};

#[path = "rewind_cost_diagnostic.rs"]
mod diagnostic;
pub use diagnostic::collect as diagnose;
pub use diagnostic::collect_reference as diagnose_reference;

/// A take's wall-clock limit: every wait and request on its instances ends with it.
pub const BUDGET_SECS: u64 = 600;

/// Predeclared looks as `(blocks, rank)`: after `blocks` whole ABBA blocks the sign interval runs
/// from the `rank`th smallest block ratio to the `rank`th largest. A decided take never collects
/// more data. The whole plan reaches a wrong verdict with probability at most
/// [`PLAN_ERROR_PPM_PER_SIDE`] per side, by nested exact binomial counts. The 8/16/32/64 looks
/// this replaced booked 1.36% per side by a union bound (their exact nested error was 1.14%), and
/// their first look needed all eight ratios above the floor, so one slow block cost another look.
const THROUGHPUT_LOOKS: [(usize, usize); 3] = [(10, 2), (16, 4), (32, 10)];
/// The error budget each direction of a verdict may use over all looks: a nominal 95% procedure.
const PLAN_ERROR_PPM_PER_SIDE: u64 = 25_000;
/// What the plan may charge to the machine, in seconds: four fifths of [`BUDGET_SECS`], whose
/// expiry is a refusal, and the last fifth is for a block under way and for what recording itself
/// adds, which is not charged. It starts no block past this and, at a look that left it
/// undecided, no further blocks whose next look its mean block cost would put past this: it ends
/// undecided, stated.
const THROUGHPUT_SPEND_SECS: u64 = BUDGET_SECS * 4 / 5;
const CONTROL_TARGET_MILLI: u64 = 1000;
const CONTROL_WIDTH_LT_MILLI: u64 = 30;

const DRAIN_COMPLETE: &str = "ATERM_DRAIN_COMPLETE";

/// A new marker even if a caller drains the same private world again.
fn next_drain_marker() -> Result<String, CouldNotRun> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map(|n| format!("{DRAIN_COMPLETE}_{n}"))
        .map_err(|_| CouldNotRun::Refused("drain completion marker sequence exhausted".into()))
}

/// No literal marker can appear in the command echo, even when it wraps. The shell
/// executes this only after the foreground drain exits, outside its reported timing.
fn drain_completion_command(marker: &str) -> String {
    let octal: String = marker.bytes().map(|byte| format!("\\{byte:03o}")).collect();
    format!("printf '\\n{octal}\\n'")
}

fn drain_completed(text: &str, marker: &str) -> bool {
    text.lines().any(|line| line.trim_end() == marker)
}

fn drain_completion_reply(
    reply: &super::instance::Reply,
    marker: &str,
) -> Result<bool, CouldNotRun> {
    if !reply.ok() {
        return could_not_run(format!(
            "the drain completion text read failed: {} {}",
            reply.stdout.trim(),
            reply.stderr.trim()
        ));
    }
    Ok(drain_completed(&reply.stdout, marker))
}

/// One parse-throughput row under the predeclared confidence-spending plan.
pub const THROUGHPUT: CostRow = CostRow::Throughput {
    bytes: 64 << 20,
    min_milli: 970,
};

/// ATERM_DESIGN.md §7.2's idle target for the whole app, in bytes. [`CostRow::IdleRss`] reports it on every bundle and judges it only on one that records by default: it measures the app, not recording (an idle instance with recording off misses it too), and design §14.5 R4 holds the default off until an idle recording pane meets it.
pub const APP_IDLE_TARGET_BYTES: u64 = 25_000_000;

/// What a cost row measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CostRow {
    /// Fresh-instance ABBA blocks (off, on, on, off) of one pane draining `bytes`,
    /// judged on whole-block ratios ([`block_ratios_milli`]) at [`THROUGHPUT_LOOKS`].
    /// The first decided interval stops the take; only an unproved interval collects more
    /// blocks, up to the plan's last look and [`THROUGHPUT_SPEND_SECS`]. Dropped input
    /// fails at once.
    Throughput { bytes: usize, min_milli: u64 },
    /// One idle pane, at most `pairs` ABBA blocks, each arm a fresh instance. After at least
    /// two blocks (or the full cap if smaller), stop only when every possible completion of
    /// the capped sample gives the same [`idle_verdict`]. Reports the observed medians and
    /// the completion bounds separately. App memory judges only a default-recording bundle.
    IdleRss { pairs: u32, max_extra_bytes: u64 },
    /// `panes` panes each draining `bytes` at once: passes when recording on adds at most `max_extra_bytes` of resident size over recording off and the pool holds no more than its limit.
    BusyRss {
        panes: u32,
        bytes: usize,
        max_extra_bytes: u64,
    },
}

/// One row's outcome: its report line and disposition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CostReport {
    pub line: String,
    pub disposition: Disposition,
}

/// A row judged pass or fail, never unproved.
const fn judged(verdict: ShapeVerdict) -> Disposition {
    match verdict {
        ShapeVerdict::Pass => Disposition::Pass,
        ShapeVerdict::Fail => Disposition::Fail,
    }
}

/// An instance in its own scratch world, recording or not.
struct Take {
    inst: Instance,
    home: PathBuf,
    _scratch: Scratch,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RssScene {
    Idle,
    Busy,
}

/// Idle RSS reads at its quiet fence; busy RSS retains its post-drain delay. Callbacks let the
/// test hold the quiet fence and advance a logical clock without a timing-dependent test.
fn sample_settled<T>(
    scene: RssScene,
    quiet: impl FnOnce() -> Result<(), CouldNotRun>,
    pause: impl FnOnce(std::time::Duration),
    read: impl FnOnce() -> Result<T, CouldNotRun>,
) -> Result<T, CouldNotRun> {
    quiet()?;
    if scene == RssScene::Busy {
        pause(std::time::Duration::from_secs(2));
    }
    read()
}

impl Take {
    fn launch(rig: &Rig, record: bool, budget: Budget) -> Result<Self, CouldNotRun> {
        let (take, recording) =
            Self::start(rig, &format!("temporal_recording = {record}"), budget)?;
        if recording != record {
            return could_not_run(format!("the instance's recording is not {record}"));
        }
        Ok(take)
    }

    /// Whether the bundle records a new pane when its config has no `temporal_recording` key.
    fn records_by_default(rig: &Rig, budget: Budget) -> Result<bool, CouldNotRun> {
        Ok(Self::start(rig, "", budget)?.1)
    }

    /// An instance whose config starts with `config_head`, once its first pane is quiet, and whether that pane records.
    fn start(rig: &Rig, config_head: &str, budget: Budget) -> Result<(Self, bool), CouldNotRun> {
        Self::start_with_diagnostic(rig, config_head, budget, None)
    }

    fn start_with_diagnostic(
        rig: &Rig,
        config_head: &str,
        budget: Budget,
        probe: Option<bool>,
    ) -> Result<(Self, bool), CouldNotRun> {
        let mut scratch = Scratch::new("atcost")
            .map_err(|e| CouldNotRun::Refused(format!("cannot make a scratch world: {e}")))?;
        if probe.is_some() || std::env::var_os("ATERM_PAINT_KEEP").is_some() {
            scratch.keep();
        }
        let world = World::prepare(scratch.path(), config_head)
            .map_err(|e| CouldNotRun::Refused(format!("cannot lay out the scratch world: {e}")))?;
        let inst = match probe {
            Some(on) => Instance::launch_recording_cost_diagnostic(&rig.aterm, &world, budget, on)?,
            None => Instance::launch(&rig.aterm, &world, budget)?,
        };
        inst.ctl.ok(&["await", "idle", "500", "timeout=60000"])?;
        let status = inst.ctl.ok(&["temporal", "status"])?;
        let recording = match field(&status, "enabled") {
            Some("true") => true,
            Some("false") => false,
            _ => return unanswered(format!("temporal status named no enabled= ({status})")),
        };
        Ok((
            Self {
                inst,
                home: world.home,
                _scratch: scratch,
            },
            recording,
        ))
    }

    /// Start `fake-tui --drain` in the pane `target` names (`None`: the first), reporting into `out`.
    fn start_drain(
        &self,
        rig: &Rig,
        target: Option<&str>,
        bytes: usize,
        out: &Path,
    ) -> Result<(), CouldNotRun> {
        let line = format!(
            "{} --drain {bytes} {}",
            rig.fake_tui.display(),
            out.display()
        );
        let mut send = target.map(|t| vec![t]).unwrap_or_default();
        send.extend(["send", line.as_str()]);
        self.inst.ctl.ok(&send)?;
        let mut key = target.map(|t| vec![t]).unwrap_or_default();
        key.extend(["key", "enter"]);
        self.inst.ctl.ok(&key)?;
        Ok(())
    }

    /// The microseconds a drain reported into `out`.
    fn drained_us(&self, out: &Path) -> Result<u64, CouldNotRun> {
        self.inst.ctl.await_event("the drain's report", || {
            crate::measure::read_regular_text(out)
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
        })
    }

    /// Validate loss after the drain's ordered PTY marker and all recorded work complete.
    /// Called after its report: the foreground producer has finished its timed writes. The
    /// shell runs the marker after that producer exits; `text` reads it under the terminal
    /// lock, after the reader's parse-and-record-enqueue critical section. Queue emptiness
    /// below therefore cannot precede unparsed drain input. The fence is outside both the
    /// producer's drain timing and the diagnostic CPU bracket.
    fn completed_status(&self, recording: bool) -> Result<String, CouldNotRun> {
        let ctl = &self.inst.ctl;
        let marker = next_drain_marker()?;
        ctl.ok(&["send", &drain_completion_command(&marker)])?;
        ctl.ok(&["key", "enter"])?;
        ctl.await_event("the drain's ordered completion marker", || {
            // `Ctl::ok` keeps only the header; the marker is in the body.
            match drain_completion_reply(&ctl.call(&["text", "trim"]), &marker) {
                Ok(true) => Some(Ok(())),
                Ok(false) => None,
                Err(why) => Some(Err(why)),
            }
        })??;
        if !recording {
            return self.inst.ctl.ok(&["temporal", "status"]);
        }
        self.inst
            .ctl
            .await_event("the recording writer's completion", || {
                let status = match self.inst.ctl.ok(&["temporal", "status"]) {
                    Ok(status) => status,
                    Err(error) => return Some(Err(error)),
                };
                match writer_finished(&status) {
                    Ok(true) => Some(Ok(status)),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                }
            })?
    }

    /// The instance's resident size in bytes, once its panes are quiet.
    fn settled_rss(&self) -> Result<u64, CouldNotRun> {
        Ok(self.settled(RssScene::Busy)?.rss)
    }

    /// The instance's resident size and app memory, once its panes are quiet.
    fn settled(&self, scene: RssScene) -> Result<Settled, CouldNotRun> {
        let started = std::time::Instant::now();
        let quiet_s = std::cell::Cell::new(0.0);
        let delay_s = std::cell::Cell::new(0.0);
        let result = sample_settled(
            scene,
            || {
                self.inst
                    .ctl
                    .ok(&["await", "idle", "2000", "timeout=120000"])?;
                quiet_s.set(started.elapsed().as_secs_f64());
                Ok(())
            },
            |duration| {
                let started = std::time::Instant::now();
                std::thread::sleep(duration);
                delay_s.set(started.elapsed().as_secs_f64());
            },
            || Settled::read(self.inst.pid(), self.inst.ctl.budget()),
        );
        if scene == RssScene::Idle {
            eprintln!(
                "measure setup: idle_rss quiet_s={:.6} post_quiet_s={:.6}",
                quiet_s.get(),
                delay_s.get()
            );
        }
        result
    }
}

/// One settled take's two sizes, in bytes.
struct Settled {
    /// Its pages in physical memory, including clean file-backed pages.
    rss: u64,
    /// Its physical footprint on macOS, or the resident size elsewhere.
    app: u64,
}

impl Settled {
    /// Read both sizes from one kernel usage result, without launching a subprocess.
    /// The footprint counts dirty private pages, including compressed ones, and excludes
    /// the clean file-backed pages the resident size counts.
    #[cfg(target_os = "macos")]
    fn read(pid: u32, _budget: Budget) -> Result<Self, CouldNotRun> {
        let usage = super::usage(pid)?;
        Ok(Self {
            rss: usage.resident,
            app: usage.footprint,
        })
    }

    /// Where no footprint is available, reuse one budgeted resident-size reading.
    #[cfg(not(target_os = "macos"))]
    fn read(pid: u32, budget: Budget) -> Result<Self, CouldNotRun> {
        let rss = rss_from_ps(pid, budget)?;
        Ok(Self { rss, app: rss })
    }
}

/// The portable resident-size collector, also exercised on macOS.
#[cfg(any(test, not(target_os = "macos")))]
fn rss_from_ps(pid: u32, budget: Budget) -> Result<u64, CouldNotRun> {
    let mut command = aterm_uds::spawnfd::command(Path::new("/bin/ps"));
    command
        .env_clear()
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .stdin(aterm_uds::spawnfd::Stdio::null());
    static GATE: aterm_uds::deadline::Gate = aterm_uds::deadline::Gate::new("measure-rss");
    let out = budget
        .output(command, &GATE)
        .map_err(|e| e.map(|why| format!("cannot run ps: {why}")))?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .map(|kib| kib.saturating_mul(1024))
        .map_err(|_| CouldNotRun::Refused(format!("ps named no resident size for {pid}")))
}

/// CPU accounting is diagnostic and never substitutes for the throughput verdict.
#[cfg(target_os = "macos")]
fn cpu_ns(pid: u32) -> Option<u64> {
    super::usage(pid).ok().map(|usage| usage.cpu_ns)
}

#[cfg(not(target_os = "macos"))]
fn cpu_ns(_: u32) -> Option<u64> {
    None
}

/// The median of `v` (the lower of the middle two).
#[must_use]
pub fn median(v: &[u64]) -> u64 {
    let mut s = v.to_vec();
    s.sort_unstable();
    s.get((s.len().saturating_sub(1)) / 2).copied().unwrap_or(0)
}

/// One observation per ABBA block: `sqrt(A1*A2/(B1*B2))`, in thousandths, rounded down.
/// `off` and `on` hold their arms in take order. Averaging the two log ratios cancels a linear
/// log-time drift over equally spaced takes; the two adjacent ratios need not be independent
/// observations. The sign interval assumes independent blocks, not independent arms within one.
/// Missing, zero or overflowing timings invalidate the sample rather than dropping observations.
#[must_use]
pub fn block_ratios_milli(off: &[u64], on: &[u64]) -> Option<Vec<u64>> {
    if off.is_empty() || off.len() != on.len() || off.len() % 2 != 0 {
        return None;
    }
    off.as_chunks::<2>()
        .0
        .iter()
        .zip(on.as_chunks::<2>().0)
        .map(|(a, b)| {
            if a.contains(&0) || b.contains(&0) {
                return None;
            }
            let numerator = u128::from(a[0])
                .checked_mul(u128::from(a[1]))?
                .checked_mul(1_000_000)?;
            let denominator = u128::from(b[0]) * u128::from(b[1]);
            u64::try_from((numerator / denominator).isqrt()).ok()
        })
        .collect()
}

/// An interval holding the median of the population `values` were drawn from with at least
/// [`MedianInterval::coverage_ppm`] probability, from the sign test (distribution-free: it
/// assumes independent draws with a common population median).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MedianInterval {
    /// The sample median (the lower of the middle two).
    pub median: u64,
    /// The interval's ends: the sorted values' `rank`th and `n + 1 - rank`th.
    pub lo: u64,
    pub hi: u64,
    /// The least probability, in millionths rounded down, that this one interval, taken alone,
    /// holds the population's median.
    pub coverage_ppm: u64,
}

/// The most values [`interval_at`] takes: its binomial sums are exact in `u128` up to here.
const MAX_INTERVAL_VALUES: usize = 64;

/// `values`' exact sign-test interval between its `rank`th smallest and `rank`th largest values.
/// `None` for no values, a rank of 0 or past the middle, or past [`MAX_INTERVAL_VALUES`].
#[must_use]
pub fn interval_at(values: &[u64], rank: usize) -> Option<MedianInterval> {
    let n = values.len();
    if n > MAX_INTERVAL_VALUES || rank == 0 || rank * 2 > n {
        return None;
    }
    let whole = 1_u128 << n;
    // `tail` is P(B <= rank - 1) * 2^n. All products fit u128 at n <= 64.
    let (mut tail, mut choose) = (0_u128, 1_u128);
    for i in 0..rank {
        tail += choose;
        choose = choose * (n - i) as u128 / (i + 1) as u128;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    Some(MedianInterval {
        median: median(&sorted),
        lo: sorted[rank - 1],
        hi: sorted[n - rank],
        coverage_ppm: u64::try_from(1_000_000 - (tail * 2_000_000).div_ceil(whole)).unwrap_or(0),
    })
}

/// The probability, in millionths rounded up, that a population whose median sits exactly on the
/// floor passes at any of `looks` (a FAIL is its mirror image, so this is each side's error).
/// Counts the block-sign sequences reaching a PASS at some look over the nested looks exactly,
/// as if a FAIL never stopped the take, which only overstates it. `None` for looks that are not
/// strictly increasing, take more than [`MAX_INTERVAL_VALUES`] blocks, or hold a rank
/// [`interval_at`] refuses.
fn plan_error_ppm(looks: &[(usize, usize)]) -> Option<u64> {
    let &(last, _) = looks.last()?;
    if last > MAX_INTERVAL_VALUES
        || !looks.windows(2).all(|w| w[0].0 < w[1].0)
        || looks.iter().any(|&(n, r)| r == 0 || r * 2 > n)
    {
        return None;
    }
    // `alive[b]`: sequences of the blocks so far with `b` ratios below the floor and no PASS yet.
    let (mut alive, mut done, mut passed) = (vec![0_u128; last + 1], 0, 0_u128);
    alive[0] = 1;
    for &(blocks, rank) in looks {
        for _ in done..blocks {
            for b in (1..=last).rev() {
                alive[b] += alive[b - 1];
            }
        }
        done = blocks;
        for count in &mut alive[..rank] {
            passed += *count << (last - blocks);
            *count = 0;
        }
    }
    u64::try_from((passed * 1_000_000).div_ceil(1_u128 << last)).ok()
}

/// Whether `blocks_ahead` more blocks, at the mean cost of the `blocks_done` so far, stay within
/// `limit`. With none ahead this is the limit itself; with none done there is no cost to project.
fn fits_spend(
    charged: std::time::Duration,
    blocks_done: u32,
    blocks_ahead: u32,
    limit: std::time::Duration,
) -> bool {
    let block = charged
        .checked_div(blocks_done)
        .unwrap_or(std::time::Duration::ZERO);
    charged.saturating_add(block.saturating_mul(blocks_ahead)) <= limit
}

/// A block's cost to the machine from its arms as `(candidate, wall)`: the recording-off arms'
/// wall time, doubled. The on arms never count, so a regression that slows them still reaches a
/// look and fails.
fn block_machine_cost(arms: &[(bool, std::time::Duration)]) -> std::time::Duration {
    arms.iter()
        .filter(|&&(candidate, _)| !candidate)
        .fold(std::time::Duration::ZERO, |sum, &(_, wall)| {
            sum.saturating_add(wall)
        })
        .saturating_mul(2)
}

/// Take a block's arms in ABBA order and return each as `(candidate, wall)` for
/// [`block_machine_cost`]. `arm` gets the arm's index and whether it records, and returns what
/// must outlive the timing together with whether to go on. A wall runs from before `arm` starts
/// to after it returns, so launch counts and the returned value's teardown does not.
fn take_block_arms<T>(
    clock: impl Fn() -> std::time::Duration,
    mut arm: impl FnMut(usize, bool) -> Result<(T, bool), CouldNotRun>,
) -> Result<Vec<(bool, std::time::Duration)>, CouldNotRun> {
    let mut arms = Vec::with_capacity(4);
    for (index, candidate) in [false, true, true, false].into_iter().enumerate() {
        let started = clock();
        let (teardown, go_on) = arm(index, candidate)?;
        arms.push((candidate, clock().saturating_sub(started)));
        drop(teardown);
        if !go_on {
            break;
        }
    }
    Ok(arms)
}

struct ThroughputObservation {
    ratios: Option<Vec<u64>>,
    dropped_input: u64,
    /// This block's [`block_machine_cost`].
    machine_cost: std::time::Duration,
}

struct ThroughputAssessment {
    interval: Option<MedianInterval>,
    interval_blocks: usize,
    disposition: Disposition,
    /// The spend limit ended the plan before its next look.
    spend_limited: bool,
    /// What the blocks taken charged to the machine.
    charged: std::time::Duration,
}

/// A/A is an instrument control: an interval excluding 1.000 fails the control;
/// one containing it passes only below the predeclared 0.030 width.
fn control_disposition(interval: Option<MedianInterval>) -> Disposition {
    match interval {
        Some(i) if i.lo > CONTROL_TARGET_MILLI || i.hi < CONTROL_TARGET_MILLI => Disposition::Fail,
        Some(i) if i.hi.saturating_sub(i.lo) < CONTROL_WIDTH_LT_MILLI => Disposition::Pass,
        _ => Disposition::Unproved,
    }
}

/// Acquire only until the first proof, under the plan fixed before any observation.
/// A FAIL returns immediately; later favorable blocks can never replace it. Before a block the
/// plan checks what it has charged against `limit`, and at a look that left it undecided what the
/// next look would cost at the mean block so far: past the limit, the take ends with the last
/// look's interval and no verdict.
fn run_throughput_plan(
    min_milli: u64,
    same_recording: bool,
    limit: std::time::Duration,
    mut sample: impl FnMut(usize) -> Result<ThroughputObservation, CouldNotRun>,
) -> Result<ThroughputAssessment, CouldNotRun> {
    let error_ppm = plan_error_ppm(&THROUGHPUT_LOOKS)
        .filter(|&ppm| ppm <= PLAN_ERROR_PPM_PER_SIDE)
        .ok_or_else(|| CouldNotRun::Refused("invalid throughput confidence plan".into()))?;
    let mut assessment = ThroughputAssessment {
        interval: None,
        interval_blocks: 0,
        disposition: Disposition::Unproved,
        spend_limited: false,
        charged: std::time::Duration::ZERO,
    };
    for pair in 0..THROUGHPUT_LOOKS[THROUGHPUT_LOOKS.len() - 1].0 {
        let next_look = THROUGHPUT_LOOKS
            .iter()
            .map(|&(blocks, _)| blocks)
            .find(|&blocks| blocks > pair)
            .expect("a block below the last look has a next look");
        let at_look = THROUGHPUT_LOOKS.iter().any(|&(blocks, _)| blocks == pair);
        let ahead = if at_look { next_look - pair } else { 0 };
        let (done, ahead) = (
            u32::try_from(pair).expect("a plan is a few blocks long"),
            u32::try_from(ahead).expect("a look is a few blocks away"),
        );
        if !fits_spend(assessment.charged, done, ahead, limit) {
            assessment.spend_limited = true;
            return Ok(assessment);
        }
        let observation = sample(pair)?;
        assessment.charged = assessment.charged.saturating_add(observation.machine_cost);
        if observation.dropped_input != 0 {
            assessment.disposition = Disposition::Fail;
            return Ok(assessment);
        }
        let ratios = observation.ratios.ok_or_else(|| {
            CouldNotRun::Refused(
                "throughput supplied missing, zero or overflowing block timings".into(),
            )
        })?;
        if ratios.len() != pair + 1 {
            return could_not_run(
                "throughput must supply exactly one observation per completed ABBA block",
            );
        }
        let Some(&(_, rank)) = THROUGHPUT_LOOKS.iter().find(|&&(n, _)| n == ratios.len()) else {
            continue;
        };
        let interval = interval_at(&ratios, rank);
        let disposition = if same_recording {
            control_disposition(interval)
        } else {
            throughput_disposition(interval, min_milli, 0)
        };
        assessment.interval = interval;
        assessment.interval_blocks = ratios.len();
        assessment.disposition = disposition;
        eprintln!(
            "measure-look: {}",
            aterm_json::json!({
                "measurement": "recording-throughput", "blocks": ratios.len(), "rank": rank,
                "same_recording": same_recording, "plan_error_ppm_per_side": error_ppm,
                "look_coverage_ppm": interval.map(|i| i.coverage_ppm),
                "lo_milli": interval.map(|i| i.lo), "hi_milli": interval.map(|i| i.hi),
                "disposition": format!("{disposition:?}"),
            })
        );
        if disposition != Disposition::Unproved {
            return Ok(assessment);
        }
    }
    Ok(assessment)
}

/// What recording adds to an idle instance's resident size: the on arm's median less the off arm's, 0 when the on arm is the smaller.
#[must_use]
pub fn added_bytes(off: &[u64], on: &[u64]) -> u64 {
    median(on).saturating_sub(median(off))
}

/// An idle row's verdict: passes when both arms were taken, recording adds at most `max_extra` resident bytes ([`added_bytes`]), and, when the bundle records by default, the on arm's median app footprint is at most [`APP_IDLE_TARGET_BYTES`]. A bundle that records only when asked is not judged on the app's own size.
#[must_use]
pub fn idle_verdict(
    off: &[u64],
    on: &[u64],
    on_app: &[u64],
    max_extra: u64,
    records_by_default: bool,
) -> ShapeVerdict {
    verdict(
        !off.is_empty()
            && !on.is_empty()
            && added_bytes(off, on) <= max_extra
            && (!records_by_default
                || (!on_app.is_empty() && median(on_app) <= APP_IDLE_TARGET_BYTES)),
    )
}

/// An exact decision about the capped idle-RSS medians. The lower median has one-based rank
/// `cap.div_ceil(2)`. No independence or distribution assumption is needed: every unobserved
/// reading may be anywhere in `0..=u64::MAX`. At least four readings per arm (two whole ABBA
/// blocks) are required, or the whole sample when its cap is smaller. `None` means keep taking
/// complete blocks; invalid and incomplete prefixes cannot pass.
#[must_use]
pub fn idle_decision(
    off: &[u64],
    on: &[u64],
    on_app: &[u64],
    cap: usize,
    max_extra: u64,
    records_by_default: bool,
) -> Option<ShapeVerdict> {
    if cap == 0
        || !cap.is_multiple_of(2)
        || off.len() != on.len()
        || on.len() != on_app.len()
        || !on.len().is_multiple_of(2)
        || on.len() < cap.min(4)
    {
        return None;
    }
    let rank = cap.div_ceil(2);
    let (off_lo, off_hi) = super::decision::completion(off, cap, rank)?;
    let (on_lo, on_hi) = super::decision::completion(on, cap, rank)?;
    let (app_lo, app_hi) = super::decision::completion(on_app, cap, rank)?;
    if on_hi.saturating_sub(off_lo) <= max_extra
        && (!records_by_default || app_hi <= APP_IDLE_TARGET_BYTES)
    {
        Some(ShapeVerdict::Pass)
    } else if on_lo.saturating_sub(off_hi) > max_extra
        || (records_by_default && app_lo > APP_IDLE_TARGET_BYTES)
    {
        Some(ShapeVerdict::Fail)
    } else {
        None
    }
}

struct IdleReadings {
    off: Vec<u64>,
    on: Vec<u64>,
    off_app: Vec<u64>,
    on_app: Vec<u64>,
    verdict: ShapeVerdict,
}

/// This acquisition loop is shared by the live instrument and callback-driven negative controls.
fn run_idle_plan(
    pairs: u32,
    max_extra: u64,
    by_default: bool,
    mut sample: impl FnMut(usize, bool) -> Result<(u64, u64), CouldNotRun>,
) -> Result<IdleReadings, CouldNotRun> {
    let cap = usize::try_from(pairs)
        .ok()
        .and_then(|p| p.checked_mul(2))
        .filter(|&n| n > 0)
        .ok_or_else(|| CouldNotRun::Refused("idle RSS requires a positive sample cap".into()))?;
    let mut readings = IdleReadings {
        off: Vec::new(),
        on: Vec::new(),
        off_app: Vec::new(),
        on_app: Vec::new(),
        verdict: ShapeVerdict::Fail,
    };
    for block in 0..pairs as usize {
        for record in [false, true, true, false] {
            let (rss, app) = sample(block, record)?;
            if record {
                &mut readings.on
            } else {
                &mut readings.off
            }
            .push(rss);
            if record {
                &mut readings.on_app
            } else {
                &mut readings.off_app
            }
            .push(app);
        }
        if let Some(verdict) = idle_decision(
            &readings.off,
            &readings.on,
            &readings.on_app,
            cap,
            max_extra,
            by_default,
        ) {
            readings.verdict = verdict;
            return Ok(readings);
        }
    }
    could_not_run("idle RSS reached its cap without a completion decision")
}

/// Measure one row.
/// # Errors
/// [`CouldNotRun`] when an instance did not start, a request failed, a drain never reported,
/// the recording writer ended, required status counters are unavailable, or an idle-RSS
/// sample cap is zero or overflows.
pub fn measure(rig: &Rig, row: &CostRow) -> Result<CostReport, CouldNotRun> {
    measure_with_control(rig, row, false)
}

/// Run the exact throughput plan with recording off in both arms. This is an instrument noise
/// control, never evidence that recording meets its floor.
/// # Errors
/// The same instance, drain and status failures as [`measure`].
pub fn throughput_control(rig: &Rig) -> Result<CostReport, CouldNotRun> {
    measure_with_control(rig, &THROUGHPUT, true)
}

fn measure_with_control(
    rig: &Rig,
    row: &CostRow,
    same_recording: bool,
) -> Result<CostReport, CouldNotRun> {
    let _one = super::take(matches!(row, CostRow::Throughput { .. }));
    let budget = Budget::from_now(BUDGET_SECS);
    match *row {
        CostRow::Throughput { bytes, min_milli } => {
            let (mut off, mut on) = (Vec::new(), Vec::new());
            let mut dropped_input = 0_u64;
            let assessment = run_throughput_plan(
                min_milli,
                same_recording,
                std::time::Duration::from_secs(THROUGHPUT_SPEND_SECS),
                |pair| {
                    let block_started = std::time::Instant::now();
                    let arms = take_block_arms(
                        || block_started.elapsed(),
                        |arm, candidate| {
                            let record = candidate && !same_recording;
                            let setup_started = std::time::Instant::now();
                            let take = Take::launch(rig, record, budget)?;
                            let setup_wall_us = setup_started.elapsed().as_micros();
                            let out = take.home.join("drain.us");
                            // Observe both endpoints without changing scheduling or timing the probes.
                            let subject_role_before = Band::observed_role(take.inst.pid());
                            let driver_role_before = Band::observed_role(0);
                            let before = cpu_ns(take.inst.pid());
                            let started = std::time::Instant::now();
                            take.start_drain(rig, None, bytes, &out)?;
                            let us = take.drained_us(&out)?;
                            let sample_wall_us = started.elapsed().as_micros();
                            let used = before
                                .zip(cpu_ns(take.inst.pid()))
                                .and_then(|(before, after)| after.checked_sub(before));
                            // Diagnostics bracket the command and report polling as well as the drain.
                            // The verdict still uses only the producer's original drain measurement.
                            let status = take.inst.ctl.ok(&["temporal", "status"])?;
                            let completion_started = std::time::Instant::now();
                            let completed = take.completed_status(record)?;
                            let completion_wall_us = completion_started.elapsed().as_micros();
                            let subject_role_after = Band::observed_role(take.inst.pid());
                            let driver_role_after = Band::observed_role(0);
                            eprintln!(
                                "measure-sample: {}",
                                aterm_json::json!({
                                    "schema": 1, "measurement": "recording-throughput", "pair": pair,
                                    "arm": arm, "recording": record, "same_recording": same_recording, "drain_us": us,
                                    "sample_wall_us": sample_wall_us, "sample_cpu_ns": used,
                                    "setup_wall_us": setup_wall_us,
                                    "completion_wall_us": completion_wall_us,
                                    "completion_fence": "ordered_shell_marker",
                                    "role_observation": "read_only_endpoints;not_continuous",
                                    "subject_role_before": {
                                        "darwin_role": subject_role_before.0, "errno": subject_role_before.1,
                                    },
                                    "driver_role_before": {
                                        "darwin_role": driver_role_before.0, "errno": driver_role_before.1,
                                    },
                                    "subject_role_after": {
                                        "darwin_role": subject_role_after.0, "errno": subject_role_after.1,
                                    },
                                    "driver_role_after": {
                                        "darwin_role": driver_role_after.0, "errno": driver_role_after.1,
                                    },
                                    "dropped_input": field(&completed, "dropped_input"),
                                    "dropped_input_at_receipt": field(&status, "dropped_input"),
                                    "completion_writer_in_flight": field(&completed, "writer_in_flight"),
                                    "completion_writer_queued_bytes": field(&completed, "writer_queued_bytes"),
                                    "writer_in_flight": field(&status, "writer_in_flight"),
                                    "writer_queued_bytes": field(&status, "writer_queued_bytes"),
                                    "writer_role_requested": field(&status, "writer_role_requested"),
                                    "writer_qos_observed": field(&status, "writer_qos_observed"),
                                    "writer_urgent_pressure": field(&status, "writer_urgent_pressure"),
                                    "writer_priority_override": field(&status, "writer_priority_override"),
                                    "pool_used": field(&status, "pool_used"),
                                })
                            );
                            if record {
                                dropped_input =
                                    dropped_input.saturating_add(recording_drops(&completed)?);
                            }
                            if candidate { &mut on } else { &mut off }.push(us);
                            Ok((take, dropped_input == 0))
                        },
                    )?;
                    Ok(ThroughputObservation {
                        ratios: block_ratios_milli(&off, &on),
                        dropped_input,
                        machine_cost: block_machine_cost(&arms),
                    })
                },
            )?;
            let ratios = block_ratios_milli(&off, &on);
            let interval = assessment.interval;
            let blocks = off.len().min(on.len()) / 2;
            let interval_blocks = assessment.interval_blocks;
            let error_ppm =
                plan_error_ppm(&THROUGHPUT_LOOKS).expect("plan validated before sampling");
            let spend_limited = assessment.spend_limited;
            let charged_secs = assessment.charged.as_secs();
            let (median_milli, lo, hi, look_coverage) = interval.map_or_else(
                || {
                    (
                        "-".to_owned(),
                        "-".to_owned(),
                        "-".to_owned(),
                        "-".to_owned(),
                    )
                },
                |i| {
                    (
                        i.median.to_string(),
                        i.lo.to_string(),
                        i.hi.to_string(),
                        i.coverage_ppm.to_string(),
                    )
                },
            );
            Ok(CostReport {
                line: format!(
                    "COST throughput bytes={bytes} blocks={blocks} interval_blocks={interval_blocks} \
                     planned_looks={THROUGHPUT_LOOKS:?} same_recording={same_recording} \
                     control_target_milli={CONTROL_TARGET_MILLI} control_width_lt_milli={CONTROL_WIDTH_LT_MILLI} off_us={off:?} on_us={on:?} \
                     off_median_us={} on_median_us={} block_ratios_milli={ratios:?} \
                     ratio_milli={median_milli} interval_milli={lo}..{hi} \
                     plan_error_ppm_per_side={error_ppm} look_coverage_ppm={look_coverage} min_milli={min_milli} \
                     spend_limit_secs={THROUGHPUT_SPEND_SECS} spend_charged_secs={charged_secs} spend_limited={spend_limited} dropped_input={dropped_input}",
                    median(&off),
                    median(&on)
                ),
                disposition: assessment.disposition,
            })
        }
        CostRow::IdleRss {
            pairs,
            max_extra_bytes,
        } => {
            let by_default = Take::records_by_default(rig, budget)?;
            let mut sample = 0;
            let IdleReadings {
                off,
                on,
                off_app,
                on_app,
                verdict,
            } = run_idle_plan(pairs, max_extra_bytes, by_default, |block, record| {
                sample += 1;
                let mut phases = super::instance::Phases::new(format!(
                    "row=idle_rss block={block} record={record} sample={sample}"
                ));
                let started = std::time::Instant::now();
                let take = Take::launch(rig, record, budget)?;
                phases.end("launch_first_quiet");
                eprintln!(
                    "measure setup: idle_rss record={record} launch_s={:.6}",
                    started.elapsed().as_secs_f64()
                );
                let took = take.settled(RssScene::Idle)?;
                phases.end("quiet_sample");
                eprintln!(
                    "measure-sample: {}",
                    aterm_json::json!({
                        "schema": 1, "measurement": "idle-rss", "block": block,
                        "recording": record, "rss_bytes": took.rss, "app_bytes": took.app,
                        "seconds": started.elapsed().as_secs_f64(),
                    })
                );
                drop(take);
                phases.end("stop");
                Ok((took.rss, took.app))
            })?;
            let on_median = median(&on);
            let on_app_median = median(&on_app);
            Ok(CostReport {
                line: format!(
                    "COST idle_rss pairs={} planned_pairs={pairs} stopping_samples_per_arm={} \
                     decision=all_completions completion_extra_bytes={:?} completion_app_bytes={:?} \
                     off_bytes={off:?} on_bytes={on:?} \
                     off_median_bytes={} on_median_bytes={on_median} extra_bytes={} \
                     max_extra_bytes={max_extra_bytes} off_app_bytes={off_app:?} \
                     on_app_bytes={on_app:?} off_app_median_bytes={} \
                     on_app_median_bytes={} records_by_default={by_default} \
                     app_target_bytes={APP_IDLE_TARGET_BYTES} app_target_met={}",
                    on.len() / 2,
                    on.len(),
                    {
                        let cap = pairs as usize * 2;
                        let (off_lo, off_hi) =
                            super::decision::completion(&off, cap, cap.div_ceil(2))
                                .expect("validated cap");
                        let (on_lo, on_hi) = super::decision::completion(&on, cap, cap.div_ceil(2))
                            .expect("validated cap");
                        (on_lo.saturating_sub(off_hi), on_hi.saturating_sub(off_lo))
                    },
                    super::decision::completion(&on_app, pairs as usize * 2, pairs as usize),
                    median(&off),
                    added_bytes(&off, &on),
                    median(&off_app),
                    on_app_median,
                    on_app_median <= APP_IDLE_TARGET_BYTES,
                ),
                disposition: judged(verdict),
            })
        }
        CostRow::BusyRss {
            panes,
            bytes,
            max_extra_bytes,
        } => {
            let (off, _) = busy(rig, false, panes, bytes, budget)?;
            let (on, pool) = busy(rig, true, panes, bytes, budget)?;
            let (used, limit) = pool;
            let extra = on.saturating_sub(off);
            Ok(CostReport {
                line: format!(
                    "COST busy_rss panes={panes} bytes={bytes} off_bytes={off} on_bytes={on} \
                     extra_bytes={extra} max_extra_bytes={max_extra_bytes} pool_used={used} \
                     pool_limit={limit}"
                ),
                disposition: judged(verdict(extra <= max_extra_bytes && used <= limit)),
            })
        }
    }
}

/// A missing loss counter cannot establish that the recording kept its input.
fn recording_drops(status: &str) -> Result<u64, CouldNotRun> {
    field(status, "dropped_input")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| {
            CouldNotRun::Unanswered(format!(
                "temporal status named no numeric dropped_input= ({status})"
            ))
        })
}

fn writer_finished(status: &str) -> Result<bool, CouldNotRun> {
    match field(status, "writer_gone") {
        Some("true") => return could_not_run("the recording writer ended before validation"),
        Some("false") => {}
        _ => return unanswered("temporal status named no writer_gone="),
    }
    let number = |key| {
        field(status, key)
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| {
                CouldNotRun::Unanswered(format!(
                    "temporal status named no numeric {key}= ({status})"
                ))
            })
    };
    Ok(number("writer_in_flight")? == 0 && number("writer_queued_bytes")? == 0)
}

/// The throughput row's disposition from the whole blocks' ratios' [`MedianInterval`]: a
/// recording that dropped input fails, whatever its speed; an interval at or above `min_milli`
/// passes, and one wholly below it fails (recording cost more than the bound, under whatever load
/// the takes ran in: the replay mint's cost was real,
/// `docs/measured/temporal-reader-rekey-2026-11-05.md`). An interval that holds `min_milli`, or
/// none (too few pairs), is unproved: the takes varied more than the bound the row judges, so
/// they are no evidence either way.
#[must_use]
pub const fn throughput_disposition(
    interval: Option<MedianInterval>,
    min_milli: u64,
    dropped_input: u64,
) -> Disposition {
    match interval {
        _ if dropped_input != 0 => Disposition::Fail,
        Some(MedianInterval { lo, .. }) if lo >= min_milli => Disposition::Pass,
        Some(MedianInterval { hi, .. }) if hi < min_milli => Disposition::Fail,
        _ => Disposition::Unproved,
    }
}

/// `panes` panes of one instance each draining `bytes` at once: its settled resident size and the pool's `(used, limit)`.
fn busy(
    rig: &Rig,
    record: bool,
    panes: u32,
    bytes: usize,
    budget: Budget,
) -> Result<(u64, (u64, u64)), CouldNotRun> {
    let mut phases = super::instance::Phases::new(format!("row=busy_rss record={record}"));
    let take = Take::launch(rig, record, budget)?;
    phases.end("launch");
    let mut targets = vec![None];
    for _ in 1..panes {
        let sid = take.inst.ctl.ok(&["spawn", "window=0", "raise=false"])?;
        let sid = sid
            .strip_prefix("OK ")
            .and_then(|s| s.split_whitespace().next())
            .ok_or_else(|| CouldNotRun::Refused(format!("spawn named no session ({sid})")))?;
        targets.push(Some(format!("@{sid}")));
    }
    phases.end("spawn");
    let ready = |target: &String| {
        take.inst
            .ctl
            .ok(&[target, "await", "idle", "500", "timeout=60000"])
            .map(|_| ())
    };
    let panes: Vec<_> = targets.iter().flatten().cloned().collect();
    await_panes(&panes, ready)?;
    phases.end("ready");
    let outs: Vec<PathBuf> = (0..targets.len())
        .map(|n| take.home.join(format!("drain-{n}.us")))
        .collect();
    for (target, out) in targets.iter().zip(&outs) {
        take.start_drain(rig, target.as_deref(), bytes, out)?;
    }
    phases.end("send");
    for out in &outs {
        take.drained_us(out)?;
    }
    phases.end("drain");
    let rss = take.settled_rss()?;
    phases.end("settle_sample");
    let status = take.inst.ctl.ok(&["temporal", "status"])?;
    let pool = |key| field(&status, key).and_then(|v| v.parse::<u64>().ok());
    let pool = match (pool("pool_used"), pool("pool_limit")) {
        (Some(used), Some(limit)) => (used, limit),
        _ if !record => (0, 0),
        _ => return unanswered(format!("temporal status named no pool ({status})")),
    };
    phases.end("fences");
    drop(take);
    phases.end("stop");
    Ok((rss, pool))
}

/// Each pane must cross the same quiet fence before any producer starts. Independent
/// fences observe together; all workers are joined even when one pane refuses.
fn await_panes<T: Sync>(
    panes: &[T],
    ready: impl Fn(&T) -> Result<(), CouldNotRun> + Sync,
) -> Result<(), CouldNotRun> {
    std::thread::scope(|scope| {
        let ready = &ready;
        let waits: Vec<_> = panes
            .iter()
            .map(|pane| scope.spawn(move || ready(pane)))
            .collect();
        let mut result = Ok(());
        for wait in waits {
            let observed = wait
                .join()
                .unwrap_or_else(|_| could_not_run("a pane readiness worker panicked"));
            result = result.and(observed);
        }
        result
    })
}

const fn verdict(pass: bool) -> ShapeVerdict {
    if pass {
        ShapeVerdict::Pass
    } else {
        ShapeVerdict::Fail
    }
}

#[cfg(test)]
#[path = "rewind_cost_tests.rs"]
mod tests;
