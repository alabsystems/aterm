// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Counter boundaries enclose timed batches, including the small clock-read overhead.
//! Process counters cover all threads; thread CPU time has its own boundary. Neither is
//! substituted for the wall time used by the existing throughput metrics.

use aterm_bench::measurement_clock as clock;
use aterm_json::{Value, json};

use super::{N_ITERS, PER_SAMPLE};

// A selected sample represents the median itself, rather than averaging two batches.
const _: () = assert!(N_ITERS > 0 && N_ITERS % 2 == 1);

#[derive(Clone, Copy, Default)]
pub(super) struct Snapshot {
    work: Option<clock::Counters>,
    thread_cpu_ns: Option<u64>,
}

impl Snapshot {
    pub(super) fn read(enabled: bool) -> Self {
        if !enabled {
            return Self::default();
        }
        Self {
            work: clock::counters(),
            // The non-Unix fallback is a wall clock, not a CPU observation.
            thread_cpu_ns: (clock::NAME == "thread_cpu")
                .then(clock::thread_cpu_ns)
                .flatten(),
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct Batch {
    seconds: f64,
    work: Option<clock::Counters>,
    thread_cpu_ns: Option<u64>,
}

impl Batch {
    pub(super) fn between(seconds: f64, before: Snapshot, after: Snapshot) -> Self {
        Self {
            seconds,
            work: before.work.zip(after.work).map(|(a, b)| b.since(a)),
            thread_cpu_ns: before
                .thread_cpu_ns
                .zip(after.thread_cpu_ns)
                .and_then(|(a, b)| b.checked_sub(a)),
        }
    }

    fn per_op_seconds(self) -> f64 {
        self.seconds / PER_SAMPLE as f64
    }

    fn record(self, case: &str, phase: &str, sample: usize, selected: bool) -> Value {
        let work = self.work.map(|c| {
            json!({
                "cpu_ns": c.cpu_ns, "p_cpu_ns": c.pcpu_ns,
                "instructions": c.instructions, "p_instructions": c.pinstructions,
                "cycles": c.cycles, "p_cycles": c.pcycles,
            })
        });
        let p_ghz = self
            .work
            .filter(|c| c.pcycles > 0)
            .and_then(|c| ratio(c.pcycles as f64, c.pcpu_ns));
        let p_instruction_share = self
            .work
            .and_then(|c| ratio(c.pinstructions as f64, c.instructions as f64));
        json!({
            "schema": 1, "case": case, "phase": phase,
            "sample": sample, "selected_median": selected, "operations": PER_SAMPLE,
            "wall_seconds": self.seconds, "per_op_seconds": self.per_op_seconds(),
            "thread_clock": clock::NAME, "thread_cpu_ns": self.thread_cpu_ns,
            "counter_scope": "process", "work": work,
            "p_ghz": p_ghz, "p_instruction_share": p_instruction_share,
        })
    }
}

fn ratio(numerator: f64, denominator: f64) -> Option<f64> {
    (numerator.is_finite() && numerator >= 0.0 && denominator.is_finite() && denominator > 0.0)
        .then_some(numerator / denominator)
        .filter(|value| value.is_finite())
}

#[derive(Clone, Copy, Default)]
pub(super) struct Pass {
    pub(super) whole: Batch,
    pub(super) capture: Batch,
}

pub(super) struct Keyframes {
    passes: [Pass; N_ITERS],
    whole_median: usize,
    capture_median: usize,
}

impl Keyframes {
    pub(super) fn new(passes: [Pass; N_ITERS]) -> Self {
        let select = |phase: fn(Pass) -> Batch| {
            let mut order: [usize; N_ITERS] = std::array::from_fn(|i| i);
            order.sort_by(|&a, &b| {
                phase(passes[a])
                    .seconds
                    .total_cmp(&phase(passes[b]).seconds)
                    .then(a.cmp(&b))
            });
            order[N_ITERS / 2]
        };
        Self {
            whole_median: select(|p| p.whole),
            capture_median: select(|p| p.capture),
            passes,
        }
    }

    pub(super) fn medians(&self) -> (f64, f64) {
        (
            self.passes[self.whole_median].whole.per_op_seconds(),
            self.passes[self.capture_median].capture.per_op_seconds(),
        )
    }

    pub(super) fn emit(&self, case: &str) {
        for (sample, pass) in self.passes.iter().enumerate() {
            for (phase, batch, selected) in [
                ("keyframe", pass.whole, self.whole_median),
                ("capture", pass.capture, self.capture_median),
            ] {
                let record = batch.record(case, phase, sample, sample == selected);
                eprintln!(
                    "crash_restore_harness: sample {}",
                    aterm_json::to_string(&record)
                        .expect("finite kernel and wall-clock observations")
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_median_keeps_its_own_sample_and_work_identity_including_ties() {
        let whole = [9., 1., 8., 2., 7., 3., 6., 4., 5.];
        let capture = [5., 5., 5., 5., 5., 5., 5., 5., 5.];
        let passes = std::array::from_fn(|i| Pass {
            whole: Batch {
                seconds: whole[i],
                work: Some(clock::Counters {
                    instructions: (i as u64 + 1) * 100,
                    ..Default::default()
                }),
                ..Default::default()
            },
            capture: Batch {
                seconds: capture[i],
                ..Default::default()
            },
        });
        let result = Keyframes::new(passes);
        assert_eq!(result.whole_median, 8);
        assert_eq!(result.capture_median, 4);
        assert_eq!(
            result.medians(),
            (5. / PER_SAMPLE as f64, 5. / PER_SAMPLE as f64)
        );
        let selected = result.passes[result.whole_median]
            .whole
            .record("shell", "keyframe", 8, true);
        assert_eq!(selected["work"]["instructions"].as_u64(), Some(900));
        assert_eq!(selected["selected_median"].as_bool(), Some(true));
    }

    #[test]
    fn counter_deltas_and_frequency_are_bound_to_the_same_batch() {
        let before = Snapshot {
            thread_cpu_ns: Some(100),
            work: Some(clock::Counters {
                cpu_ns: 100.,
                pcpu_ns: 50.,
                instructions: 100,
                pinstructions: 60,
                cycles: 500,
                pcycles: 200,
            }),
        };
        let after = Snapshot {
            thread_cpu_ns: Some(180),
            work: Some(clock::Counters {
                cpu_ns: 200.,
                pcpu_ns: 100.,
                instructions: 300,
                pinstructions: 210,
                cycles: 1000,
                pcycles: 400,
            }),
        };
        let record = Batch::between(0.01, before, after).record("shell", "keyframe", 0, true);
        assert_eq!(record["thread_cpu_ns"].as_u64(), Some(80));
        assert_eq!(record["work"]["instructions"].as_u64(), Some(200));
        assert_eq!(record["work"]["p_cycles"].as_u64(), Some(200));
        assert_eq!(record["p_ghz"].as_f64(), Some(4.));
        assert_eq!(record["p_instruction_share"].as_f64(), Some(0.75));
    }

    #[test]
    fn unavailable_or_empty_counters_stay_null_and_labels_round_trip() {
        for work in [None, Some(clock::Counters::default())] {
            let batch = Batch {
                work,
                ..Default::default()
            };
            let record = batch.record("quoted\"\ncase", "capture", 0, false);
            let text = aterm_json::to_string(&record).unwrap();
            let back: Value = aterm_json::from_str(&text).unwrap();
            assert_eq!(back["case"].as_str(), Some("quoted\"\ncase"));
            assert!(back["p_ghz"].is_null());
            assert!(back["p_instruction_share"].is_null());
            assert!(back["thread_cpu_ns"].is_null());
        }
        let absent = Batch::between(1., Snapshot::default(), Snapshot::default());
        assert!(absent.record("shell", "keyframe", 0, false)["work"].is_null());
    }
}
