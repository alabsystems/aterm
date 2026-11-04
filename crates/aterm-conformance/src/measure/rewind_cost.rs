// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rewind's cost on the release binary: parse throughput and resident size with recording on against recording off.

use std::path::{Path, PathBuf};

use super::Rig;
use super::instance::{
    Budget, CouldNotRun, Instance, Scratch, World, could_not_run, field, unanswered,
};
use super::starvation::{Disposition, ShapeVerdict};

#[path = "rewind_cost_diagnostic.rs"]
mod diagnostic;
pub use diagnostic::collect as diagnose;
pub use diagnostic::collect_reference as diagnose_reference;

/// A take's wall-clock limit: every wait and request on its instances ends with it.
pub const BUDGET_SECS: u64 = 600;

/// ATERM_DESIGN.md §7.2's idle target for the whole app, in bytes. [`CostRow::IdleRss`] reports it on every bundle and judges it only on one that records by default: it measures the app, not recording (an idle instance with recording off misses it too), and design §14.5 R4 holds the default off until an idle recording pane meets it.
pub const APP_IDLE_TARGET_BYTES: u64 = 25_000_000;

/// What a cost row measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CostRow {
    /// `pairs` ABBA blocks (off, on, on, off) of one pane draining `bytes`, each take a fresh instance, judged on the adjacent takes' off/on drain ratios ([`pair_ratios_milli`]) by [`throughput_disposition`]: passes when their median's interval ([`median_interval`]) lies at or above `min_milli` thousandths and no recording input was dropped, fails when it lies below or input was dropped, and is unproved between.
    Throughput {
        bytes: usize,
        pairs: u32,
        min_milli: u64,
    },
    /// One idle pane, `pairs` ABBA pairs (off, on, on, off), each take a fresh instance: passes when the on arm's median resident size is at most `max_extra_bytes` over the off arm's and, on a bundle that records by default (one more instance with no `temporal_recording` key says), at most [`APP_IDLE_TARGET_BYTES`] ([`idle_verdict`]). Each take's app memory ([`Settled::read`]) is reported beside it and judges nothing.
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

    /// Validate loss after the PTY is quiet and all recorded work is complete. This wait is
    /// outside both the producer's drain timing and the diagnostic CPU bracket.
    fn completed_status(&self, recording: bool) -> Result<String, CouldNotRun> {
        self.inst
            .ctl
            .ok(&["await", "idle", "500", "timeout=60000"])?;
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
        Ok(self.settled()?.rss)
    }

    /// The instance's resident size and app memory, once its panes are quiet.
    fn settled(&self) -> Result<Settled, CouldNotRun> {
        self.inst
            .ctl
            .ok(&["await", "idle", "2000", "timeout=120000"])?;
        std::thread::sleep(std::time::Duration::from_secs(2));
        Settled::read(self.inst.pid(), self.inst.ctl.budget())
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

/// The off take's drain time over the on take's, in thousandths (1000: recording cost nothing),
/// for each pair of adjacent takes in an ABBA block: its arms 0 and 1, then its arms 3 and 2.
/// `off` and `on` hold each arm's takes in take order, so block `p`'s arms 0 and 3 are `off`'s
/// entries `2p` and `2p + 1`, and its arms 1 and 2 are `on`'s. Adjacent takes, a couple of seconds
/// apart, share the machine's state: a load that comes and goes between them is in both sides of
/// a pair, where pooling each arm's takes would put it in one arm's median and not the other's.
/// A pair with an on take of 0 µs (none was measured) gives no ratio.
#[must_use]
pub fn pair_ratios_milli(off: &[u64], on: &[u64]) -> Vec<u64> {
    off.iter()
        .zip(on)
        .filter(|(_, on)| **on != 0)
        .map(|(off, on)| off.saturating_mul(1000) / on)
        .collect()
}

/// An interval holding the median of the population `values` were drawn from with at least
/// [`MedianInterval::coverage_ppm`] probability, from the sign test (distribution-free: it
/// assumes only independent draws).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MedianInterval {
    /// The sample median (the lower of the middle two).
    pub median: u64,
    /// The interval's ends: the sorted values' `k`th and `n + 1 - k`th.
    pub lo: u64,
    pub hi: u64,
    /// The least probability, in millionths rounded down, that the interval holds the population's
    /// median.
    pub coverage_ppm: u64,
}

/// The most values [`median_interval`] takes: its binomial sums are exact in `u128` up to here.
const MAX_INTERVAL_VALUES: usize = 64;

/// `values`' median and its sign-test interval at 95% or more: with `n` values, `k` is the largest
/// with `P(Binomial(n, 1/2) <= k - 1) <= 2.5%`, so the median lies below the `k`th smallest or
/// above the `k`th largest with at most 2.5% probability each. `None` for fewer than 6 values (no
/// such `k`) or more than [`MAX_INTERVAL_VALUES`].
#[must_use]
pub fn median_interval(values: &[u64]) -> Option<MedianInterval> {
    let n = values.len();
    if n > MAX_INTERVAL_VALUES {
        return None;
    }
    let whole = 1_u128 << n;
    let choose =
        |i: usize| -> u128 { (0..i).fold(1_u128, |c, j| c * (n - j) as u128 / (j + 1) as u128) };
    // `tail` is P(B <= k - 1) * 2^n for the current `k`; the last `k` whose tail is at most 2.5%.
    let (mut k, mut tail, mut kept) = (0, 0_u128, None);
    while k < n / 2 {
        tail += choose(k);
        k += 1;
        if tail * 40 > whole {
            break;
        }
        kept = Some((k, tail));
    }
    let (k, tail) = kept?;
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    Some(MedianInterval {
        median: median(&sorted),
        lo: sorted[k - 1],
        hi: sorted[n - k],
        coverage_ppm: u64::try_from(1_000_000 - (tail * 2_000_000).div_ceil(whole)).unwrap_or(0),
    })
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

/// Measure one row.
/// # Errors
/// [`CouldNotRun`] when an instance did not start, a request failed, a drain never reported,
/// the recording writer ended, or required status counters are unavailable.
pub fn measure(rig: &Rig, row: &CostRow) -> Result<CostReport, CouldNotRun> {
    let _one = super::ONE_TAKE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let budget = Budget::from_now(BUDGET_SECS);
    match *row {
        CostRow::Throughput {
            bytes,
            pairs,
            min_milli,
        } => {
            let (mut off, mut on) = (Vec::new(), Vec::new());
            let mut dropped_input = 0_u64;
            for pair in 0..pairs {
                for (arm, record) in [false, true, true, false].into_iter().enumerate() {
                    let take = Take::launch(rig, record, budget)?;
                    let out = take.home.join("drain.us");
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
                    let completed = take.completed_status(record)?;
                    eprintln!(
                        "measure-sample: {}",
                        aterm_json::json!({
                            "schema": 1, "measurement": "recording-throughput", "pair": pair,
                            "arm": arm, "recording": record, "drain_us": us,
                            "sample_wall_us": sample_wall_us, "sample_cpu_ns": used,
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
                        dropped_input = dropped_input.saturating_add(recording_drops(&completed)?);
                    }
                    if record { &mut on } else { &mut off }.push(us);
                }
            }
            let ratios = pair_ratios_milli(&off, &on);
            let interval = median_interval(&ratios);
            let (median_milli, lo, hi, coverage) = interval.map_or_else(
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
                    "COST throughput bytes={bytes} pairs={pairs} off_us={off:?} on_us={on:?} \
                     off_median_us={} on_median_us={} pair_ratios_milli={ratios:?} \
                     ratio_milli={median_milli} interval_milli={lo}..{hi} \
                     coverage_ppm={coverage} min_milli={min_milli} dropped_input={dropped_input}",
                    median(&off),
                    median(&on)
                ),
                disposition: throughput_disposition(interval, min_milli, dropped_input),
            })
        }
        CostRow::IdleRss {
            pairs,
            max_extra_bytes,
        } => {
            let by_default = Take::records_by_default(rig, budget)?;
            let (mut off, mut on) = (Vec::new(), Vec::new());
            let (mut off_app, mut on_app) = (Vec::new(), Vec::new());
            for _ in 0..pairs {
                for record in [false, true, true, false] {
                    let took = Take::launch(rig, record, budget)?.settled()?;
                    if record { &mut on } else { &mut off }.push(took.rss);
                    if record { &mut on_app } else { &mut off_app }.push(took.app);
                }
            }
            let on_median = median(&on);
            let on_app_median = median(&on_app);
            Ok(CostReport {
                line: format!(
                    "COST idle_rss pairs={pairs} off_bytes={off:?} on_bytes={on:?} \
                     off_median_bytes={} on_median_bytes={on_median} extra_bytes={} \
                     max_extra_bytes={max_extra_bytes} off_app_bytes={off_app:?} \
                     on_app_bytes={on_app:?} off_app_median_bytes={} \
                     on_app_median_bytes={} records_by_default={by_default} \
                     app_target_bytes={APP_IDLE_TARGET_BYTES} app_target_met={}",
                    median(&off),
                    added_bytes(&off, &on),
                    median(&off_app),
                    on_app_median,
                    on_app_median <= APP_IDLE_TARGET_BYTES,
                ),
                disposition: judged(idle_verdict(
                    &off,
                    &on,
                    &on_app,
                    max_extra_bytes,
                    by_default,
                )),
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

/// The throughput row's disposition from the adjacent takes' ratios' [`MedianInterval`]: a
/// recording that dropped input fails, whatever its speed; an interval at or above `min_milli`
/// passes, and one wholly below it fails (recording cost more than the bound, under whatever load
/// the takes ran in: ATERM-KI-1102-7's cost is real). An interval that holds `min_milli`, or none
/// (too few pairs), is unproved: the takes varied more than the bound the row judges, so they
/// are no evidence either way.
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
    let take = Take::launch(rig, record, budget)?;
    let mut targets = vec![None];
    for _ in 1..panes {
        let sid = take.inst.ctl.ok(&["spawn", "window=0", "raise=false"])?;
        let sid = sid
            .strip_prefix("OK ")
            .and_then(|s| s.split_whitespace().next())
            .ok_or_else(|| CouldNotRun::Refused(format!("spawn named no session ({sid})")))?;
        targets.push(Some(format!("@{sid}")));
    }
    for target in targets.iter().flatten() {
        take.inst
            .ctl
            .ok(&[target, "await", "idle", "500", "timeout=60000"])?;
    }
    let outs: Vec<PathBuf> = (0..targets.len())
        .map(|n| take.home.join(format!("drain-{n}.us")))
        .collect();
    for (target, out) in targets.iter().zip(&outs) {
        take.start_drain(rig, target.as_deref(), bytes, out)?;
    }
    for out in &outs {
        take.drained_us(out)?;
    }
    let rss = take.settled_rss()?;
    let status = take.inst.ctl.ok(&["temporal", "status"])?;
    let pool = |key| field(&status, key).and_then(|v| v.parse::<u64>().ok());
    let pool = match (pool("pool_used"), pool("pool_limit")) {
        (Some(used), Some(limit)) => (used, limit),
        _ if !record => (0, 0),
        _ => return unanswered(format!("temporal status named no pool ({status})")),
    };
    Ok((rss, pool))
}

const fn verdict(pass: bool) -> ShapeVerdict {
    if pass {
        ShapeVerdict::Pass
    } else {
        ShapeVerdict::Fail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_portable_resident_probe_obeys_the_take_budget() {
        let pid = std::process::id();
        assert!(rss_from_ps(pid, Budget::from_now(60)).expect("resident size") > 0);
        assert!(matches!(
            rss_from_ps(pid, Budget::from_now(0)),
            Err(CouldNotRun::Refused(_))
        ));
    }

    #[test]
    fn the_ratios_pair_adjacent_takes_and_their_interval_is_the_sign_tests() {
        assert_eq!(median(&[5, 1, 3]), 3);
        assert_eq!(median(&[4, 1, 3, 2]), 2);
        assert_eq!(
            pair_ratios_milli(&[970, 1000, 5], &[1000, 1010, 0]),
            [970, 990],
            "an unmeasured on take gives no ratio"
        );
        let ten: Vec<u64> = (1..=10).rev().collect();
        assert_eq!(
            median_interval(&ten),
            Some(MedianInterval {
                median: 5,
                lo: 2,
                hi: 9,
                coverage_ppm: 978_515,
            }),
            "P(B(10, 1/2) <= 1) = 11/1024"
        );
        let twenty: Vec<u64> = (1..=20).collect();
        let i = median_interval(&twenty).expect("twenty values");
        assert_eq!(
            (i.lo, i.hi, i.coverage_ppm),
            (6, 15, 958_610),
            "P(B(20, 1/2) <= 5) = 21700/2^20"
        );
        assert_eq!(median_interval(&[1, 2, 3, 4, 5]), None, "too few for 95%");
        assert!(median_interval(&[1; 6]).is_some());
        assert_eq!(median_interval(&[1; 65]), None, "past the exact sums");
    }

    /// Retained takes (`off_us`, `on_us` in take order) of three five-block runs.
    const LOADED_POOLED_955: ([u64; 10], [u64; 10]) = (
        [
            1_808_126, 1_844_560, 2_086_444, 1_879_753, 2_001_503, 1_938_782, 1_887_837, 2_636_332,
            2_758_945, 2_782_664,
        ],
        [
            1_934_805, 1_824_336, 1_952_736, 1_888_877, 2_029_006, 2_081_219, 2_499_644, 2_511_547,
            2_948_381, 2_884_309,
        ],
    );
    const LOADED_POOLED_1025: ([u64; 10], [u64; 10]) = (
        [
            304_384, 291_178, 311_930, 307_422, 309_872, 292_797, 293_213, 786_952, 861_320,
            726_545,
        ],
        [
            299_894, 291_672, 297_713, 297_672, 305_655, 298_078, 771_557, 821_118, 725_792,
            813_669,
        ],
    );
    const QUIET_POOLED_999: ([u64; 10], [u64; 10]) = (
        [
            1_605_535, 1_614_247, 1_609_860, 1_607_189, 1_596_909, 1_609_144, 1_586_673, 1_607_772,
            1_593_787, 1_616_058,
        ],
        [
            1_596_980, 1_597_657, 1_655_941, 1_618_460, 1_604_724, 1_642_396, 1_608_202, 1_613_086,
            1_602_880, 1_610_144,
        ],
    );

    fn disposition_of((off, on): ([u64; 10], [u64; 10])) -> Disposition {
        throughput_disposition(median_interval(&pair_ratios_milli(&off, &on)), 970, 0)
    }

    /// A take drains for 0.2 to 3 s in a fresh instance (these runs' takes), so a load that
    /// comes and goes slows whole takes (ATERM-KI-1102-7) and the arms' pooled medians follow
    /// the Mac: retained loaded runs pooled 955, a fail, and 1025, a pass that had one block run
    /// 2.5 times slow in both arms. Judged on the adjacent takes' ratios, each reads unproved:
    /// their intervals, 931..1049 and 892..1047, hold the bound. A quiet run's, 979..1005, clears
    /// it, and takes that cost recording 6% in every pair fail however they spread.
    /// NEGATIVE CONTROL: a verdict on the ratios' median alone (`median >= min_milli`) fails the
    /// 955 run (median 964) and passes the 1025 one (998).
    #[test]
    fn the_row_judges_the_takes_not_the_load_they_ran_in() {
        assert_eq!(disposition_of(LOADED_POOLED_955), Disposition::Unproved);
        assert_eq!(disposition_of(LOADED_POOLED_1025), Disposition::Unproved);
        assert_eq!(disposition_of(QUIET_POOLED_999), Disposition::Pass);
        let on: [u64; 10] = [
            300_000, 320_000, 900_000, 310_000, 305_000, 700_000, 301_000, 330_000, 299_000,
            480_000,
        ];
        let off = on.map(|us| us * 94 / 100);
        assert_eq!(disposition_of((off, on)), Disposition::Fail);
        assert_eq!(
            throughput_disposition(None, 970, 0),
            Disposition::Unproved,
            "too few pairs prove nothing"
        );
    }

    #[test]
    fn a_fast_recording_cannot_pass_by_losing_input() {
        let fast = median_interval(&[1000; 10]);
        assert_eq!(throughput_disposition(fast, 970, 0), Disposition::Pass);
        assert_eq!(throughput_disposition(fast, 970, 1), Disposition::Fail);
        assert_eq!(throughput_disposition(None, 970, 1), Disposition::Fail);
        assert_eq!(recording_drops("OK dropped_input=0").unwrap(), 0);
        assert_eq!(recording_drops("OK dropped_input=2").unwrap(), 2);
        for status in ["OK", "OK dropped_input=-", "OK dropped_input=bad"] {
            assert!(matches!(
                recording_drops(status),
                Err(CouldNotRun::Unanswered(_))
            ));
        }
    }

    #[test]
    fn loss_is_checked_only_after_live_writer_completion() {
        let receipt =
            "OK writer_gone=false writer_in_flight=1 writer_queued_bytes=8 dropped_input=0";
        let completed =
            "OK writer_gone=false writer_in_flight=0 writer_queued_bytes=0 dropped_input=1";
        assert!(!writer_finished(receipt).unwrap());
        assert!(writer_finished(completed).unwrap());
        assert_eq!(
            throughput_disposition(
                median_interval(&[1000; 10]),
                970,
                recording_drops(completed).unwrap()
            ),
            Disposition::Fail
        );
        assert!(
            writer_finished("OK writer_gone=false writer_in_flight=0 writer_queued_bytes=0")
                .unwrap()
        );
        assert!(
            !writer_finished("OK writer_gone=false writer_in_flight=1 writer_queued_bytes=0")
                .unwrap()
        );
        assert!(
            !writer_finished("OK writer_gone=false writer_in_flight=0 writer_queued_bytes=8")
                .unwrap()
        );
        assert!(matches!(
            writer_finished("OK writer_gone=true"),
            Err(CouldNotRun::Refused(_))
        ));
        for status in [
            "OK",
            "OK writer_gone=false",
            "OK writer_gone=false writer_in_flight=- writer_queued_bytes=0",
        ] {
            assert!(matches!(
                writer_finished(status),
                Err(CouldNotRun::Unanswered(_))
            ));
        }
    }

    /// One pane's Rewind budget, the idle row's bound.
    const PANE: u64 = 8 << 20;

    #[test]
    fn the_idle_row_judges_what_recording_adds_and_the_app_only_once_recording_is_the_default() {
        // e11bf220e1's take: the app is twice §7.2's target with recording on and off.
        let (off, on) = ([50_266_112], [50_806_784]);
        assert_eq!(added_bytes(&off, &on), 540_672);
        assert_eq!(
            idle_verdict(&off, &on, &[], PANE, false),
            ShapeVerdict::Pass
        );
        assert_eq!(
            idle_verdict(&off, &on, &on, PANE, true),
            ShapeVerdict::Fail,
            "a bundle that records by default is held to the 25 MB idle target"
        );
        assert_eq!(
            idle_verdict(&[20_000_000], &[24_000_000], &[24_000_000], PANE, true),
            ShapeVerdict::Pass,
            "one that meets it passes"
        );
        assert_eq!(
            idle_verdict(&off, &[59_000_000], &[], PANE, false),
            ShapeVerdict::Fail,
            "8.7 MB added is past one pane's budget"
        );
        assert_eq!(
            idle_verdict(&[50_000_000], &[49_000_000], &[], 0, false),
            ShapeVerdict::Pass,
            "a smaller on arm adds nothing"
        );
        assert_eq!(
            idle_verdict(&[], &[1], &[], PANE, false),
            ShapeVerdict::Fail
        );
        assert_eq!(
            idle_verdict(&[1], &[], &[], PANE, false),
            ShapeVerdict::Fail,
            "no take passes nothing"
        );
        assert_eq!(
            idle_verdict(&[35_000_000], &[35_100_000], &[11_000_000], PANE, true),
            ShapeVerdict::Pass,
            "clean file-backed pages count toward resident size, not the app footprint"
        );
    }

    /// The app reading is the footprint, not the resident size: a 64 MiB file this process
    /// maps and reads is resident and clean, so `ps` counts it and the footprint does not;
    /// 64 MiB of memory it writes is dirty, and the footprint counts it. The readings run in a
    /// child of this test binary that runs this test alone: in the shared test process, the
    /// other tests' own allocations moved the footprint between two readings by more than the
    /// bound.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_app_reading_counts_dirty_pages_and_not_clean_file_pages() {
        const CHILD: &str = "ATERM_TEST_APP_READING_CHILD";
        const HANG: std::time::Duration = std::time::Duration::from_secs(60);
        let test = "measure::rewind_cost::tests::the_app_reading_counts_dirty_pages_and_not_clean_file_pages";
        if std::env::var_os(CHILD).is_none() {
            let mut cmd =
                aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
            cmd.args([test, "--exact", "--nocapture", "--test-threads=1"])
                .env(CHILD, "1");
            let out = aterm_uds::deadline::output_within(&mut cmd, HANG).expect("the child ends");
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert_eq!(
                out.status.code(),
                Some(0),
                "{stdout}{}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                stdout
                    .lines()
                    .any(|line| line.starts_with("test result: ok. 1 passed;")),
                "the child ran no test:\n{stdout}"
            );
            return;
        }
        const LEN: usize = 64 << 20;
        const PAGE: usize = 16 << 10;
        let pid = std::process::id();
        let budget = Budget::from_now(60);
        let dir = aterm_tempfile::TempDir::new().expect("a scratch directory");
        let path = dir.path().join("clean.bin");
        {
            use std::io::Write as _;
            let mut out = std::fs::File::create(&path).expect("the file");
            let chunk = vec![7u8; 1 << 20];
            for _ in 0..LEN >> 20 {
                out.write_all(&chunk).expect("the file's bytes");
            }
        }
        let file = std::fs::File::open(&path).expect("the file");
        let Settled {
            rss: rss_0,
            app: app_0,
        } = Settled::read(pid, budget).expect("memory");
        // SAFETY: a fresh read-only private mapping of `LEN` bytes of an open file of that
        // length; it is unmapped below and nothing else holds it.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                LEN,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                std::os::fd::AsRawFd::as_raw_fd(&file),
                0,
            )
        };
        assert_ne!(map, libc::MAP_FAILED, "the mapping");
        let mut sum = 0u64;
        for off in (0..LEN).step_by(PAGE) {
            // SAFETY: `off < LEN`, inside the live mapping.
            sum += u64::from(unsafe { std::ptr::read_volatile(map.cast::<u8>().add(off)) });
        }
        assert_eq!(sum, 7 * (LEN / PAGE) as u64, "fixture: every page was read");
        let Settled {
            rss: rss_1,
            app: app_1,
        } = Settled::read(pid, budget).expect("memory");
        // SAFETY: the mapping made above, `LEN` long, unmapped once.
        assert_eq!(unsafe { libc::munmap(map, LEN) }, 0, "the unmap");
        // Fresh anonymous pages (a heap buffer could reuse pages the file's write dirtied).
        // SAFETY: a new private anonymous read-write mapping of `LEN` bytes, unmapped below.
        let anon = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                LEN,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        assert_ne!(anon, libc::MAP_FAILED, "the anonymous mapping");
        for off in (0..LEN).step_by(PAGE) {
            // SAFETY: `off < LEN`, inside the live writable mapping.
            unsafe { std::ptr::write_volatile(anon.cast::<u8>().add(off), 1) };
        }
        let app_2 = Settled::read(pid, budget).expect("memory").app;
        // SAFETY: the anonymous mapping made above, `LEN` long, unmapped once.
        assert_eq!(unsafe { libc::munmap(anon, LEN) }, 0, "the anonymous unmap");
        assert!(
            rss_1 >= rss_0 + (48 << 20),
            "fixture: the read file pages are resident ({rss_0} -> {rss_1})"
        );
        assert!(
            app_1 < app_0 + (16 << 20),
            "the app reading counted clean file pages ({app_0} -> {app_1})"
        );
        assert!(
            app_2 >= app_1 + (48 << 20),
            "the app reading missed 64 MiB of written memory ({app_1} -> {app_2})"
        );
    }

    #[test]
    fn a_low_take_in_either_arm_does_not_decide_the_row() {
        // Design §14.5 As built (R4): six runs' takes, each arm with one low instance (41.45 MB on, 37.81 MB off).
        let on = [
            48_910_000, 48_710_000, 41_450_000, 48_450_000, 48_530_000, 48_610_000,
        ];
        let off = [
            48_170_000, 48_120_000, 37_810_000, 48_840_000, 48_460_000, 48_910_000,
        ];
        assert_eq!(added_bytes(&off, &on), 360_000);
        assert_eq!(
            idle_verdict(&off, &on, &[], PANE, false),
            ShapeVerdict::Pass
        );
        // fd2a2d37fc's one pair put its on take 12 MB under its off take (26.9 against 38.9 MB); the same spread with the off take low fails a row judged on one pair.
        assert_eq!(
            idle_verdict(&[26_900_000], &[38_900_000], &[], PANE, false),
            ShapeVerdict::Fail
        );
    }
}
