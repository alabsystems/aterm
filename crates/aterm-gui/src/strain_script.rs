// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A scripted strain for measuring the stats deck in motion
//! (`docs/DESIGN-stats-deck-gpu-motion-2026-10-26.md` §4): the dev seam
//! `ATERM_DEV_STRAIN_SCRIPT` replaces each adopted reading's CPU now, load average and hour of
//! over-capacity buckets with a script's, so a measuring take swings the dial's needle and soak
//! through the same moves on every run, whatever the Mac is doing. Builds without debug
//! assertions or the `dev-seams` feature never read it.
//!
//! The script is comma-separated steps `LEVEL[/SOAK]:SECONDS`, cycled from the first reading
//! adopted: `LEVEL` is the strain in `0..=2` (up to 1 the share of the Mac busy, past 1 over
//! capacity by `LEVEL − 1`), `SOAK` the hour's soak in `0..=1` (0 where unsaid), and `SECONDS` how
//! long the step holds. `0.1:2,0.9:2,0.2:2,1.6:2,0.0:2` is the swing take's.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use aterm_vitals::model::{Resource, ResourceView, Snapshot};

/// One step of a script.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Step {
    /// The strain, `0..=2`.
    pub(crate) level: f32,
    /// The hour's soak, `0..=1`.
    pub(crate) soak: f32,
    /// How long the step holds.
    pub(crate) hold: Duration,
}

/// A parsed `ATERM_DEV_STRAIN_SCRIPT`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Script {
    steps: Vec<Step>,
}

impl Script {
    /// Parse `LEVEL[/SOAK]:SECONDS,...`.
    ///
    /// # Errors
    ///
    /// A script with no step, a step without its `:SECONDS`, a number that does not parse or is
    /// not finite, a level outside `0..=2`, a soak outside `0..=1`, or a hold that is not positive.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let mut steps = Vec::new();
        for raw in text.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (strain, secs) = raw
                .split_once(':')
                .ok_or_else(|| format!("step {raw:?} has no :SECONDS"))?;
            let (level, soak) = strain.split_once('/').unwrap_or((strain, "0"));
            let num = |s: &str| {
                s.trim()
                    .parse::<f32>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .ok_or_else(|| format!("step {raw:?}: {s:?} is not a number"))
            };
            let (level, soak, secs) = (num(level)?, num(soak)?, num(secs)?);
            if !(0.0..=2.0).contains(&level) {
                return Err(format!("step {raw:?}: level {level} is outside 0..=2"));
            }
            if !(0.0..=1.0).contains(&soak) {
                return Err(format!("step {raw:?}: soak {soak} is outside 0..=1"));
            }
            if secs <= 0.0 {
                return Err(format!("step {raw:?}: hold {secs} s is not positive"));
            }
            steps.push(Step {
                level,
                soak,
                hold: Duration::from_secs_f32(secs),
            });
        }
        if steps.is_empty() {
            return Err("the script has no step".into());
        }
        Ok(Self { steps })
    }

    /// The step `elapsed` after the script began, the script cycling.
    #[must_use]
    pub(crate) fn at(&self, elapsed: Duration) -> Step {
        let cycle: Duration = self.steps.iter().map(|s| s.hold).sum();
        let mut left = Duration::from_nanos(
            (elapsed.as_nanos() % cycle.as_nanos().max(1))
                .try_into()
                .unwrap_or(0),
        );
        for step in &self.steps {
            if left < step.hold {
                return *step;
            }
            left -= step.hold;
        }
        self.steps[self.steps.len() - 1]
    }
}

impl Step {
    /// `snap` with this step's strain: the CPU row busy at `min(level, 1)` of its capacity and
    /// the GPU row idle, the load average `2^(3·over)` times the cores past capacity (so
    /// [`aterm_vitals::strain::over`] reads `level − 1`), no swap, and the CPU row's hour over
    /// capacity for its newest buckets, as many as make [`aterm_vitals::strain::soak`] read this
    /// step's soak from calm. A row with no capacity read yet keeps its reading.
    #[must_use]
    pub(crate) fn apply(&self, snap: &Snapshot) -> Snapshot {
        let mut out = snap.clone();
        let cpu = &mut out.resources[Resource::Cpu.index()];
        let cores = cpu.capacity;
        if let Some(cap) = cores {
            cpu.now = Some(cap * f64::from(self.level.min(1.0)));
        }
        set_soak(cpu, self.soak);
        let gpu = &mut out.resources[Resource::Gpu.index()];
        if gpu.capacity.is_some() {
            gpu.now = Some(0.0);
        }
        let over = f64::from(self.level - 1.0).max(0.0);
        out.facts.load1 = match cores {
            Some(n) if over > 0.0 => {
                Some(n * (over * aterm_vitals::strain::OVER_LOAD_DOUBLINGS).exp2())
            }
            Some(n) => Some(n * 0.5),
            None => out.facts.load1,
        };
        out.facts.swap_rate = Some(0.0);
        out
    }
}

/// Mark `row`'s newest buckets over capacity, as many as an exponential average with
/// [`aterm_vitals::strain::SOAK_TAU_S`] needs to read `soak` from calm (`1 − keep^k`), and the
/// rest not; the ring grows by hollow buckets where it holds too few.
fn set_soak(row: &mut ResourceView, soak: f32) {
    let keep = (-(aterm_vitals::ring::BUCKET_S as f64) / aterm_vitals::strain::SOAK_TAU_S).exp();
    let over = if soak <= 0.0 {
        0
    } else {
        let k = (1.0 - f64::from(soak)).max(1e-9).ln() / keep.ln();
        (k.round() as usize).clamp(1, aterm_vitals::ring::RING_BUCKETS)
    };
    let mut cells: Vec<aterm_vitals::ring::Cell> = row.ring.to_vec();
    if cells.len() < over {
        let mut grown = vec![aterm_vitals::ring::Cell::default(); over - cells.len()];
        grown.extend(cells);
        cells = grown;
    }
    let n = cells.len();
    for (i, cell) in cells.iter_mut().enumerate() {
        if i + over >= n {
            cell.n = cell.n.max(1.0);
            cell.over_n = cell.n;
        } else {
            cell.over_n = 0.0;
        }
    }
    row.ring = cells.into();
}

/// The seam's script, read once; `None` without the seam or with one that does not parse (which
/// is said once on stderr).
fn script() -> Option<&'static Script> {
    static SCRIPT: OnceLock<Option<Script>> = OnceLock::new();
    SCRIPT
        .get_or_init(|| {
            let raw = aterm_types::dev_seam!("ATERM_DEV_STRAIN_SCRIPT")?;
            match Script::parse(&raw.to_string_lossy()) {
                Ok(script) => Some(script),
                Err(why) => {
                    eprintln!("aterm: ATERM_DEV_STRAIN_SCRIPT ignored: {why}");
                    None
                }
            }
        })
        .as_ref()
}

/// The reading adopted at `now`, with the seam's scripted strain where the seam is set; the
/// script's clock starts at the first reading it rewrites.
#[must_use]
pub(crate) fn adopt(snap: Arc<Snapshot>, now: Instant) -> Arc<Snapshot> {
    static BEGAN: OnceLock<Instant> = OnceLock::new();
    let Some(script) = script() else {
        return snap;
    };
    let began = *BEGAN.get_or_init(|| now);
    Arc::new(script.at(now.saturating_duration_since(began)).apply(&snap))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_vitals::Strain;

    fn calm() -> Snapshot {
        let mut snap = Snapshot::default();
        let cpu = &mut snap.resources[Resource::Cpu.index()];
        cpu.capacity = Some(18.0);
        cpu.now = Some(3.0);
        cpu.ring_k = 400;
        cpu.ring = vec![aterm_vitals::ring::Cell::default(); 12].into();
        let gpu = &mut snap.resources[Resource::Gpu.index()];
        gpu.capacity = Some(1.0);
        gpu.now = Some(0.4);
        snap
    }

    #[test]
    fn a_script_parses_its_steps_and_refuses_what_it_cannot_hold() {
        let s = Script::parse("0.1:2, 0.9/0.5:1.5,1.6:2").expect("parses");
        assert_eq!(s.steps.len(), 3);
        assert_eq!(
            s.steps[1],
            Step {
                level: 0.9,
                soak: 0.5,
                hold: Duration::from_secs_f32(1.5)
            }
        );
        for bad in [
            "",
            "0.5",
            "x:2",
            "2.5:1",
            "0.5/1.5:1",
            "0.5:0",
            "0.5:-1",
            "nan:1",
        ] {
            assert!(Script::parse(bad).is_err(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn the_script_cycles_through_its_steps() {
        let s = Script::parse("0.1:2,0.9:1").expect("parses");
        let level = |ms: u64| s.at(Duration::from_millis(ms)).level;
        assert_eq!(level(0), 0.1);
        assert_eq!(level(1999), 0.1);
        assert_eq!(level(2000), 0.9);
        assert_eq!(level(2999), 0.9);
        assert_eq!(level(3000), 0.1);
        assert_eq!(level(5500), 0.9);
    }

    #[test]
    fn a_step_makes_the_strain_read_what_it_says() {
        for (level, soak) in [(0.0, 0.0), (0.35, 0.0), (1.0, 0.2), (1.6, 0.5), (2.0, 0.95)] {
            let step = Step {
                level,
                soak,
                hold: Duration::from_secs(1),
            };
            let strain = Strain::of(&step.apply(&calm()));
            assert!(
                (strain.level.expect("read") - level.min(1.0)).abs() < 1e-6,
                "{level}: {strain:?}"
            );
            let over = (level - 1.0).max(0.0);
            let want_over = if over > 0.0 {
                over.clamp(aterm_vitals::strain::OVER_FLOOR, 1.0)
            } else {
                0.0
            };
            assert!(
                (strain.over - want_over).abs() < 1e-4,
                "{level}: {strain:?}"
            );
            // The soak lands within one bucket's step of the asked value.
            assert!((strain.soak - soak).abs() < 0.02, "{soak}: {strain:?}");
        }
    }
}
