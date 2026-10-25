// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rewind matrix: a Rewind step's time from the main thread taking it to its fold shown, over a full chain, and an open view's idle wakes, on the release binary.

use std::time::Duration;

use super::Rig;
use super::instance::{
    Budget, CouldNotRun, Instance, Scratch, World, could_not_run, field, uint_field,
};
use super::starvation::ShapeVerdict;

/// What a row does with the open view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RewindScene {
    /// `steps` steps across the chain, half back and half forward, each to another state.
    Steps { steps: u32, max_p99_us: u64 },
    /// The view left alone for `window` once its worker has settled.
    Idle { window: Duration },
}

/// One rewind row: the scene, over a chain of `chain_bytes` of output after the session's keyframe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RewindRow {
    pub scene: RewindScene,
    pub chain_bytes: usize,
}

/// A take's wall-clock limit: every wait and request on the instance ends with it.
pub const BUDGET_SECS: u64 = 300;

/// The chain file's line, newline included: a person's wide build log.
const LINE: usize = 80;

/// How long the worker is given to fill its fork cache after the view opens.
const SETTLE: Duration = Duration::from_secs(3);

/// One row's outcome.
#[derive(Clone, Debug, PartialEq)]
pub struct RewindReport {
    pub row: RewindRow,
    /// The recorder's keyframes after the output (one: the whole output is one chain).
    pub keyframes: u64,
    /// The recorded states the view steps among.
    pub states: u64,
    /// Steps taken, steps the metrics booked as shown, and their p99 in microseconds (a headless instance presents nothing, so a step is booked when its fold is shown and its redraw asked for).
    pub steps: u32,
    pub booked: u64,
    pub p99_us: u64,
    /// Over an idle window: the worker's wakes, frames, timer wakes, and every wake by variant.
    pub done_wakes: u64,
    pub frames: u64,
    pub timer_wakes: u64,
    pub wakes: String,
    pub verdict: ShapeVerdict,
}

impl std::fmt::Display for RewindReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.row.scene {
            RewindScene::Steps { max_p99_us, .. } => write!(
                f,
                "REWIND steps={} booked={} p99_us={} max_p99_us={max_p99_us} chain_bytes={} \
                 keyframes={} states={} verdict={:?}",
                self.steps,
                self.booked,
                self.p99_us,
                self.row.chain_bytes,
                self.keyframes,
                self.states,
                self.verdict
            ),
            RewindScene::Idle { window } => write!(
                f,
                "REWIND idle_ms={} rewind_wakes={} frames={} timer_wakes={} wakes_by_variant={} \
                 chain_bytes={} keyframes={} states={} verdict={:?}",
                window.as_millis(),
                self.done_wakes,
                self.frames,
                self.timer_wakes,
                self.wakes,
                self.row.chain_bytes,
                self.keyframes,
                self.states,
                self.verdict
            ),
        }
    }
}

/// The row's bound: a step row passes when every step was booked and the p99 is within its bound; an idle row when the view woke the app for nothing and drew no frame.
#[must_use]
pub fn judge(row: &RewindRow, report: &RewindReport) -> ShapeVerdict {
    let one_chain = report.keyframes == 1;
    let pass = match row.scene {
        RewindScene::Steps { steps, max_p99_us } => {
            one_chain
                && steps > 0
                && report.booked == u64::from(steps)
                && report.p99_us <= max_p99_us
        }
        RewindScene::Idle { .. } => one_chain && report.done_wakes == 0 && report.frames == 0,
    };
    if pass {
        ShapeVerdict::Pass
    } else {
        ShapeVerdict::Fail
    }
}

/// `key`'s count in a `wakes_by_variant=<name>:<n>,…` field (0 when absent).
#[must_use]
pub fn variant_wakes(line: &str, variant: &str) -> u64 {
    field(line, "wakes_by_variant")
        .into_iter()
        .flat_map(|v| v.split(','))
        .filter_map(|pair| pair.split_once([':', '=']))
        .filter(|(name, _)| *name == variant)
        .filter_map(|(_, n)| n.parse::<u64>().ok())
        .sum()
}

/// A `ms` decimal field as whole microseconds, rounded up.
#[must_use]
pub fn ms_field_us(line: &str, key: &str) -> Option<u64> {
    let ms = field(line, key)?.parse::<f64>().ok()?;
    (ms.is_finite() && ms >= 0.0).then(|| (ms * 1000.0).ceil() as u64)
}

/// The chain file: whole numbered lines, each [`LINE`] columns wide with its newline and its number coloured, just under `bytes`.
#[must_use]
pub fn chain_text(bytes: usize) -> String {
    let mut out = String::with_capacity(bytes);
    for n in 0.. {
        let digits = format!("{n:07} ");
        let fill = char::from(b'a' + (n % 26) as u8);
        let line = format!(
            "\x1b[{}m{digits}\x1b[m{}\n",
            31 + n % 6,
            String::from(fill).repeat(LINE - 1 - digits.len())
        );
        if out.len() + line.len() > bytes {
            break;
        }
        out.push_str(&line);
    }
    out
}

/// Measure one row: record `row.chain_bytes` of output in one chain, open the view on it, then step or idle.
/// # Errors
/// [`CouldNotRun`] when the instance did not start, a request failed, the output was not one chain, or a step's fold was never shown.
pub fn measure(rig: &Rig, row: &RewindRow) -> Result<RewindReport, CouldNotRun> {
    let _one = super::ONE_TAKE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let budget = Budget::from_now(BUDGET_SECS);
    let mut scratch = Scratch::new("atrewind")
        .map_err(|e| CouldNotRun(format!("cannot make a scratch world: {e}")))?;
    if std::env::var_os("ATERM_PAINT_KEEP").is_some() {
        scratch.keep();
    }
    let world = World::prepare(scratch.path(), "temporal_recording = true")
        .map_err(|e| CouldNotRun(format!("cannot lay out the scratch world: {e}")))?;
    std::fs::write(world.home.join("chain.txt"), chain_text(row.chain_bytes))
        .map_err(|e| CouldNotRun(format!("cannot write the chain file: {e}")))?;
    let inst = Instance::launch(&rig.aterm, &world, budget)?;
    let ctl = &inst.ctl;
    ctl.ok(&["await", "idle", "500", "timeout=60000"])?;
    ctl.ok(&["send", "cat chain.txt"])?;
    ctl.ok(&["key", "enter"])?;
    ctl.ok(&["await", "idle", "1000", "timeout=120000"])?;
    let status = ctl.ok(&["temporal", "status"])?;
    let keyframes = uint_field(&status, "keyframes")
        .ok_or_else(|| CouldNotRun(format!("temporal status named no keyframes= ({status})")))?;
    let open = ctl.ok(&["rewind", "open"])?;
    if field(&open, "view") != Some("open") {
        return could_not_run(format!("the Rewind view did not open ({open})"));
    }
    let states = field(&open, "state")
        .and_then(|s| s.split_once('/'))
        .and_then(|(_, n)| n.parse::<u64>().ok())
        .ok_or_else(|| CouldNotRun(format!("rewind open named no state=<n>/<N> ({open})")))?;
    std::thread::sleep(SETTLE);
    ctl.ok(&["metrics", "reset"])?;
    let mut report = RewindReport {
        row: *row,
        keyframes,
        states,
        steps: 0,
        booked: 0,
        p99_us: 0,
        done_wakes: 0,
        frames: 0,
        timer_wakes: 0,
        wakes: String::new(),
        verdict: ShapeVerdict::Fail,
    };
    match row.scene {
        RewindScene::Steps { steps, .. } => {
            let half = u64::from(steps / 2).max(1);
            let stride = (states.saturating_sub(1) / half).max(1);
            for i in 0..steps {
                let by = if u64::from(i) < half {
                    format!("-{stride}")
                } else {
                    format!("+{stride}")
                };
                ctl.ok(&["rewind", "step", &by])?;
                let want = u64::from(i) + 1;
                ctl.await_event("the step's fold shown", || {
                    let line = ctl.ok(&["metrics", "percentiles"]).ok()?;
                    (uint_field(&line, "n_rewind_shown")? >= want).then_some(())
                })?;
                report.steps += 1;
            }
            let metrics = ctl.ok(&["metrics", "percentiles"])?;
            report.booked = uint_field(&metrics, "n_rewind_shown").unwrap_or(0);
            report.p99_us = ms_field_us(&metrics, "rewind_shown_p99_ms").ok_or_else(|| {
                CouldNotRun(format!(
                    "ctl metrics percentiles named no rewind_shown_p99_ms= ({metrics})"
                ))
            })?;
        }
        RewindScene::Idle { window } => {
            std::thread::sleep(window);
            let metrics = ctl.ok(&["metrics"])?;
            report.done_wakes = variant_wakes(&metrics, "RewindDone");
            report.frames = uint_field(&metrics, "frames")
                .ok_or_else(|| CouldNotRun(format!("ctl metrics named no frames= ({metrics})")))?;
            report.timer_wakes = uint_field(&metrics, "timer_wakes").unwrap_or(0);
            report.wakes = field(&metrics, "wakes_by_variant")
                .unwrap_or("none")
                .to_string();
            let status = ctl.ok(&["rewind", "status"])?;
            if field(&status, "view") != Some("open") {
                return could_not_run(format!("the Rewind view closed in the window ({status})"));
            }
        }
    }
    let _ = ctl.ok(&["rewind", "close"]);
    report.verdict = judge(row, &report);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEPS: RewindRow = RewindRow {
        scene: RewindScene::Steps {
            steps: 4,
            max_p99_us: 8_000,
        },
        chain_bytes: 2_000_000,
    };

    const IDLE: RewindRow = RewindRow {
        scene: RewindScene::Idle {
            window: Duration::from_secs(10),
        },
        chain_bytes: 2_000_000,
    };

    fn report(row: RewindRow) -> RewindReport {
        RewindReport {
            row,
            keyframes: 1,
            states: 900,
            steps: 4,
            booked: 4,
            p99_us: 2_100,
            done_wakes: 0,
            frames: 0,
            timer_wakes: 0,
            wakes: "Control:2".to_string(),
            verdict: ShapeVerdict::Fail,
        }
    }

    #[test]
    fn a_fast_step_row_passes_and_each_witness_fails_it() {
        assert_eq!(judge(&STEPS, &report(STEPS)), ShapeVerdict::Pass);
        let slow = RewindReport {
            p99_us: 41_000,
            ..report(STEPS)
        };
        assert_eq!(
            judge(&STEPS, &slow),
            ShapeVerdict::Fail,
            "proven red: a worker that refolds from the keyframe on every step"
        );
        let unbooked = RewindReport {
            booked: 3,
            ..report(STEPS)
        };
        assert_eq!(
            judge(&STEPS, &unbooked),
            ShapeVerdict::Fail,
            "a step never shown"
        );
        let rekeyed = RewindReport {
            keyframes: 2,
            ..report(STEPS)
        };
        assert_eq!(
            judge(&STEPS, &rekeyed),
            ShapeVerdict::Fail,
            "a re-keyframe split the chain: the row measured a shorter one"
        );
    }

    #[test]
    fn an_idle_view_passes_only_with_no_rewind_wake_and_no_frame() {
        assert_eq!(judge(&IDLE, &report(IDLE)), ShapeVerdict::Pass);
        let woke = RewindReport {
            done_wakes: 1,
            ..report(IDLE)
        };
        assert_eq!(judge(&IDLE, &woke), ShapeVerdict::Fail);
        let drew = RewindReport {
            frames: 3,
            ..report(IDLE)
        };
        assert_eq!(judge(&IDLE, &drew), ShapeVerdict::Fail);
    }

    #[test]
    fn the_metrics_fields_read_as_counts_and_microseconds() {
        let line = "OK frames=2 wakes_by_variant=Control:4,RewindDone:3 rewind_step_p99_ms=7.25";
        assert_eq!(variant_wakes(line, "RewindDone"), 3);
        assert_eq!(variant_wakes(line, "Rewind"), 0);
        assert_eq!(variant_wakes("OK wakes_by_variant=none", "RewindDone"), 0);
        assert_eq!(ms_field_us(line, "rewind_step_p99_ms"), Some(7_250));
        assert_eq!(ms_field_us(line, "frames_ms"), None);
    }

    #[test]
    fn the_chain_file_is_whole_lines_just_under_its_size() {
        let text = chain_text(2_000_000);
        assert!(
            text.len() <= 2_000_000 && text.len() > 2_000_000 - 2 * LINE,
            "{}",
            text.len()
        );
        assert!(text.lines().all(|l| {
            let visible: usize = l
                .split('\x1b')
                .enumerate()
                .map(|(i, part)| {
                    if i == 0 {
                        part.len()
                    } else {
                        part.split_once('m').map_or(0, |(_, rest)| rest.len())
                    }
                })
                .sum();
            visible == LINE - 1
        }));
    }
}
