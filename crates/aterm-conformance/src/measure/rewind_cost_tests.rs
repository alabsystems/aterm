// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn idle_sampling_stops_only_when_every_remaining_reading_is_harmless() {
    use super::*;
    let mut calls = 0;
    let pass = run_idle_plan(3, 8, true, |_, on| {
        calls += 1;
        Ok((100 + u64::from(on), 10))
    })
    .unwrap();
    assert_eq!((calls, pass.verdict), (8, ShapeVerdict::Pass));
    calls = 0;
    let fail = run_idle_plan(3, 8, true, |_, on| {
        calls += 1;
        Ok((100 + 9 * u64::from(on), 10))
    })
    .unwrap();
    assert_eq!((calls, fail.verdict), (8, ShapeVerdict::Fail));
    calls = 0;
    let borderline = run_idle_plan(3, 8, false, |block, on| {
        calls += 1;
        Ok((
            if on {
                9
            } else if block == 0 {
                0
            } else {
                100
            },
            10,
        ))
    })
    .unwrap();
    assert_eq!((calls, borderline.verdict), (12, ShapeVerdict::Pass));
    // Replacing completion bounds with the prefix's median would fail this at block two.
    let off = [0, 0, 100, 100];
    let on = [9; 4];
    assert_eq!(
        idle_verdict(&off, &on, &[10; 4], 8, false),
        ShapeVerdict::Fail
    );
    assert_eq!(idle_decision(&off, &on, &[10; 4], 6, 8, false), None);
    let mut full_off = off.to_vec();
    full_off.extend([100, 100]);
    assert_eq!(
        idle_verdict(&full_off, &[9; 6], &[10; 6], 8, false),
        ShapeVerdict::Pass
    );
    assert_eq!(
        idle_decision(&[100; 2], &[101; 2], &[10; 2], 6, 8, false),
        None
    );
    assert!(run_idle_plan(0, 8, false, |_, _| panic!("invalid cap sampled")).is_err());
    assert!(run_idle_plan(3, 8, false, |_, _| could_not_run("missing reading")).is_err());
    assert_eq!(
        idle_decision(
            &[100; 4],
            &[101; 4],
            &[APP_IDLE_TARGET_BYTES + 1; 4],
            6,
            8,
            true
        ),
        Some(ShapeVerdict::Fail)
    );
    assert_eq!(
        idle_decision(
            &[100; 4],
            &[101; 4],
            &[APP_IDLE_TARGET_BYTES + 1; 4],
            6,
            8,
            false
        ),
        Some(ShapeVerdict::Pass)
    );
}

#[test]
fn idle_decisions_agree_with_every_binary_completion_including_a_late_regression() {
    use super::*;
    // Enumerate all 4,096 paired six-reading samples over {0, 9}.
    for off_bits in 0..64 {
        for on_bits in 0..64 {
            let values = |bits| {
                (0..6)
                    .map(|i| if bits & (1 << i) == 0 { 0 } else { 9 })
                    .collect::<Vec<_>>()
            };
            let off = values(off_bits);
            let on = values(on_bits);
            let at_cap = idle_verdict(&off, &on, &[10; 6], 8, false);
            if let Some(early) = idle_decision(&off[..4], &on[..4], &[10; 4], 6, 8, false) {
                assert_eq!(early, at_cap);
            }
            assert_eq!(
                idle_decision(&off, &on, &[10; 6], 6, 8, false),
                Some(at_cap)
            );
        }
    }
    // The last two blocks reverse a favorable first block; minimum and bounds both matter.
    assert_eq!(
        idle_decision(&[100; 2], &[100; 2], &[10; 2], 6, 8, false),
        None
    );
    assert_eq!(
        idle_decision(
            &[100; 6],
            &[100, 100, 109, 109, 109, 109],
            &[10; 6],
            6,
            8,
            false
        ),
        Some(ShapeVerdict::Fail)
    );
}

use super::*;

#[test]
fn pane_fences_overlap_and_a_refusal_joins_every_observer() {
    let (started, starts) = std::sync::mpsc::channel();
    let channels: Vec<_> = (0..3).map(|_| std::sync::mpsc::channel::<()>()).collect();
    let (releases, panes): (Vec<_>, Vec<_>) = channels
        .into_iter()
        .map(|(tx, rx)| (tx, std::sync::Mutex::new(rx)))
        .unzip();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            await_panes(&panes, |pane| {
                started.send(()).expect("report fence entry");
                pane.lock()
                    .expect("one observer")
                    .recv()
                    .expect("release fence");
                could_not_run("planted refusal")
            })
        });
        let all_entered = (0..panes.len()).all(|_| {
            starts
                .recv_timeout(super::super::instance::MUST_HAPPEN)
                .is_ok()
        });
        // Release even a sequential mutant, so the negative control fails without hanging.
        for release in releases {
            let _ = release.send(());
        }
        let result = worker.join().expect("observer coordinator");
        assert!(all_entered, "all fences must start before any is released");
        assert_eq!(result, could_not_run("planted refusal"));
    });
    assert_eq!(await_panes(&[0, 1, 2], |_| Ok(())), Ok(()));
    assert_eq!(
        await_panes(&[0, 1, 2], |pane| {
            assert_ne!(*pane, 1, "planted observer panic");
            Ok(())
        }),
        could_not_run("a pane readiness worker panicked")
    );
}

#[test]
fn idle_rss_reads_at_the_quiet_fence_and_busy_rss_keeps_its_delay() {
    let observed = |scene| {
        let clock = std::cell::Cell::new(std::time::Duration::ZERO);
        sample_settled(
            scene,
            || {
                clock.set(std::time::Duration::from_secs(2));
                Ok(())
            },
            |delay| clock.set(clock.get() + delay),
            || Ok(clock.get()),
        )
        .unwrap()
    };
    assert_eq!(observed(RssScene::Idle), std::time::Duration::from_secs(2));
    assert_eq!(
        observed(RssScene::Busy),
        std::time::Duration::from_secs(4),
        "negative control: restoring the busy delay to idle must fail the idle assertion"
    );
    let unready = sample_settled::<()>(
        RssScene::Idle,
        || could_not_run("the quiet fence did not complete"),
        |_| panic!("a refused quiet fence must not pause"),
        || panic!("a refused quiet fence must not produce a memory sample"),
    );
    assert!(unready.is_err());
}

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
fn the_interval_is_the_exact_sign_tests() {
    assert_eq!(median(&[5, 1, 3]), 3);
    assert_eq!(median(&[4, 1, 3, 2]), 2);
    let ten: Vec<u64> = (1..=10).rev().collect();
    assert_eq!(
        interval_at(&ten, 2),
        Some(MedianInterval {
            median: 5,
            lo: 2,
            hi: 9,
            coverage_ppm: 978_515,
        }),
        "P(B(10, 1/2) <= 1) = 11/1024"
    );
    let twenty: Vec<u64> = (1..=20).collect();
    let i = interval_at(&twenty, 6).expect("twenty values");
    assert_eq!(
        (i.lo, i.hi, i.coverage_ppm),
        (6, 15, 958_610),
        "P(B(20, 1/2) <= 5) = 21700/2^20"
    );
    let sixty_four: Vec<u64> = (1..=64).collect();
    let i = interval_at(&sixty_four, 24).expect("the most values an interval takes");
    assert_eq!((i.lo, i.hi, i.coverage_ppm), (24, 41, 967_234));
    assert_eq!(interval_at(&[1; 65], 24), None, "past the exact sums");
    assert_eq!(interval_at(&[1; 10], 0), None, "rank 0 names no value");
    assert_eq!(interval_at(&[1; 10], 6), None, "past the middle");
    assert!(interval_at(&[1; 10], 5).is_some());
}

#[test]
fn the_plan_spends_at_most_its_error_budget_over_the_nested_looks() {
    for (&(n, rank), (lo, hi, coverage)) in
        THROUGHPUT_LOOKS
            .iter()
            .zip([(2, 9, 978_515), (4, 13, 978_729), (10, 23, 979_938)])
    {
        let values: Vec<_> = (1..=n as u64).collect();
        let i = interval_at(&values, rank).unwrap();
        assert_eq!(
            (i.lo, i.hi, i.coverage_ppm),
            (lo, hi, coverage),
            "{n} blocks"
        );
    }
    assert_eq!(plan_error_ppm(&THROUGHPUT_LOOKS), Some(24_129));
    assert!(plan_error_ppm(&THROUGHPUT_LOOKS).unwrap() <= PLAN_ERROR_PPM_PER_SIDE);
    for looser in [
        [(10, 3), (16, 4), (32, 10)],
        [(10, 2), (16, 5), (32, 10)],
        [(10, 2), (16, 4), (32, 11)],
    ] {
        assert!(
            plan_error_ppm(&looser).unwrap() > PLAN_ERROR_PPM_PER_SIDE,
            "negative control: one rank looser at any look breaks the budget: {looser:?}"
        );
    }
    for invalid in [
        &[][..],
        &[(10, 2), (10, 3)][..],
        &[(16, 4), (10, 2)][..],
        &[(10, 0)][..],
        &[(10, 6)][..],
        &[(65, 24)][..],
    ] {
        assert_eq!(plan_error_ppm(invalid), None, "{invalid:?}");
    }
}

#[test]
fn the_nested_error_counts_each_sequence_once() {
    // An independent oracle: every 10-block below/above sequence, against the nested counts.
    let looks = [(6, 1), (10, 2)];
    let passing = (0_u32..1 << 10)
        .filter(|bits| (bits & 0b11_1111).count_ones() == 0 || bits.count_ones() <= 1)
        .count() as u128;
    assert_eq!(
        plan_error_ppm(&looks),
        u64::try_from((passing * 1_000_000).div_ceil(1 << 10)).ok()
    );
    let one_look = |n, rank| plan_error_ppm(&[(n, rank)]).unwrap();
    assert!(
        plan_error_ppm(&looks).unwrap() < one_look(6, 1) + one_look(10, 2),
        "negative control: a union bound double-counts a sequence passing at both looks"
    );
}

const SPEND: std::time::Duration = std::time::Duration::from_secs(THROUGHPUT_SPEND_SECS);

/// The plan over `values`, each block charging `cost(block)`, and how many blocks it sampled.
fn charged_plan(
    values: &[u64],
    control: bool,
    loss_at: Option<usize>,
    cost: impl Fn(usize) -> std::time::Duration,
) -> (ThroughputAssessment, usize) {
    let CostRow::Throughput { min_milli, .. } = THROUGHPUT else {
        panic!("the throughput plan measures throughput");
    };
    let mut calls = 0;
    let result = run_throughput_plan(min_milli, control, SPEND, |pair| {
        calls += 1;
        Ok(ThroughputObservation {
            ratios: Some(values[..=pair].to_vec()),
            dropped_input: u64::from(loss_at == Some(pair)),
            machine_cost: cost(pair),
        })
    })
    .unwrap();
    (result, calls)
}

fn simulated_plan(
    values: &[u64],
    control: bool,
    loss_at: Option<usize>,
) -> (ThroughputAssessment, usize) {
    charged_plan(values, control, loss_at, |_| std::time::Duration::ZERO)
}

#[test]
fn a_decided_look_stops_acquisition_and_cannot_be_rerolled() {
    let (below, calls) = simulated_plan(&[969; 64], false, None);
    assert_eq!((below.disposition, calls), (Disposition::Fail, 10));
    let (at_floor, calls) = simulated_plan(&[970; 64], false, None);
    assert_eq!((at_floor.disposition, calls), (Disposition::Pass, 10));
    let mut values = [1000; 64];
    values[..9].fill(940);
    let (failed, calls) = simulated_plan(&values, false, None);
    assert_eq!(
        (failed.disposition, failed.interval_blocks, calls),
        (Disposition::Fail, 10, 10)
    );
    assert_eq!(
        throughput_disposition(interval_at(&values[..32], 10), 970, 0),
        Disposition::Pass,
        "negative control: the plan's last look over these blocks would erase a proven FAIL"
    );

    values.fill(940);
    values[..10].fill(1000);
    let (passed, calls) = simulated_plan(&values, false, None);
    assert_eq!((passed.disposition, calls), (Disposition::Pass, 10));
    let (lost, calls) = simulated_plan(&[1000; 64], false, Some(1));
    assert_eq!(
        (lost.disposition, calls),
        (Disposition::Fail, 2),
        "loss stops before the first statistical look"
    );
    let mut calls = 0;
    assert!(
        run_throughput_plan(970, false, SPEND, |_| {
            calls += 1;
            Ok(ThroughputObservation {
                ratios: None,
                dropped_input: 0,
                machine_cost: std::time::Duration::ZERO,
            })
        })
        .is_err()
    );
    assert_eq!(
        calls, 1,
        "invalid timings cannot be repaired by collecting later samples"
    );
    assert!(
        run_throughput_plan(970, false, SPEND, |_| Ok(ThroughputObservation {
            ratios: Some(vec![1000; 8]),
            dropped_input: 0,
            machine_cost: std::time::Duration::ZERO,
        }))
        .is_err(),
        "one physical block cannot masquerade as eight observations"
    );
}

#[test]
fn one_slow_block_does_not_extend_the_first_look() {
    let mut values = [1000; 64];
    values[0] = 900;
    let (result, calls) = simulated_plan(&values, false, None);
    assert_eq!((result.disposition, calls), (Disposition::Pass, 10));
    values[1] = 900;
    let (result, calls) = simulated_plan(&values, false, None);
    assert_eq!(
        (result.disposition, calls),
        (Disposition::Pass, 16),
        "negative control: a second slow block is past what the first look tolerates"
    );
}

#[test]
fn only_undecided_looks_extend_and_the_control_requires_precision() {
    let mut values = [1000; 64];
    values[..3].fill(940);
    let (result, calls) = simulated_plan(&values, false, None);
    assert_eq!((result.disposition, calls), (Disposition::Pass, 16));
    values[..5].fill(940);
    let (result, calls) = simulated_plan(&values, false, None);
    assert_eq!((result.disposition, calls), (Disposition::Pass, 32));

    let mut values = [1000; 64];
    values[..8].copy_from_slice(&[980, 985, 990, 995, 1005, 1010, 1015, 1020]);
    let (recording, calls) = simulated_plan(&values, false, None);
    assert_eq!((recording.disposition, calls), (Disposition::Pass, 10));
    let (control, calls) = simulated_plan(&values, true, None);
    assert_eq!(
        (control.disposition, calls),
        (Disposition::Pass, 16),
        "negative control: a wide interval cannot pass A/A by clearing the recording floor"
    );
    let (biased, calls) = simulated_plan(&[1001; 64], true, None);
    assert_eq!(
        (biased.disposition, calls),
        (Disposition::Fail, 10),
        "an A/A interval excluding 1.000 stops as a failed control"
    );
    assert_eq!(
        control_disposition(interval_at(
            &[985, 985, 985, 985, 1015, 1015, 1015, 1015],
            1
        )),
        Disposition::Unproved,
        "width must be strictly below the 0.030 margin"
    );
    let on_floor: Vec<_> = (0..64)
        .map(|i| if i % 2 == 0 { 940 } else { 1000 })
        .collect();
    let (unproved, calls) = simulated_plan(&on_floor, false, None);
    assert_eq!(
        (unproved.disposition, unproved.spend_limited, calls),
        (Disposition::Unproved, false, 32),
        "the last look bounds the blocks: 32, not 64"
    );
}

#[test]
fn the_spend_limit_states_undecided_at_a_look_it_cannot_afford() {
    let secs = std::time::Duration::from_secs;
    let on_floor: Vec<_> = (0..64_u64)
        .map(|i| if i % 2 == 0 { 940 } else { 1000 })
        .collect();
    let shape = |(result, calls): (ThroughputAssessment, usize)| {
        (
            result.disposition,
            result.spend_limited,
            result.interval_blocks,
            calls,
        )
    };
    assert_eq!(
        shape(charged_plan(&on_floor, false, None, |_| secs(16))),
        (Disposition::Unproved, true, 16, 16),
        "at look 16: 256 s charged and 16 more blocks at 16 s is past the limit"
    );
    assert_eq!(
        shape(charged_plan(&on_floor, false, None, |_| secs(15))),
        (Disposition::Unproved, false, 32, 32),
        "negative control: 240 s and 16 more at 15 s is exactly the limit and runs on"
    );
    assert_eq!(
        shape(charged_plan(&on_floor, false, None, |block| {
            secs(if block == 0 { 40 } else { 6 })
        })),
        (Disposition::Unproved, false, 32, 32),
        "one slow block moves the mean, and does not cancel a look its peers can reach"
    );
    assert_eq!(
        shape(charged_plan(&[1000; 64], false, None, |_| secs(70))),
        (Disposition::Unproved, true, 0, 7),
        "the limit reached before any look: no interval and no verdict, never a refusal"
    );
}

#[test]
fn a_regression_fails_within_a_limit_only_its_recording_off_arms_are_charged_to() {
    let secs = std::time::Duration::from_secs;
    let (failed, calls) = charged_plan(&[250; 64], false, None, |_| secs(46));
    assert_eq!(
        (failed.disposition, failed.spend_limited, calls),
        (Disposition::Fail, false, 10),
        "the on arms' own slowness is the verdict's evidence and charges nothing"
    );
    let (spent, calls) = charged_plan(&[250; 64], false, None, |_| secs(60));
    assert_eq!(
        (spent.disposition, spent.spend_limited, calls),
        (Disposition::Unproved, true, 9),
        "negative control: a machine busy in the recording-off arms still ends undecided"
    );
}

#[test]
fn a_blocks_charge_is_its_recording_off_arms_doubled() {
    let secs = std::time::Duration::from_secs;
    let arms = |off: u64, on: u64| {
        [
            (false, secs(off)),
            (true, secs(on)),
            (true, secs(on)),
            (false, secs(3)),
        ]
    };
    assert_eq!(block_machine_cost(&arms(2, 2)), secs(10));
    assert_eq!(
        block_machine_cost(&arms(2, 200)),
        secs(10),
        "a recording arm's own slowness is the verdict's evidence and charges nothing"
    );
    assert_eq!(
        block_machine_cost(&arms(20, 2)),
        secs(46),
        "negative control: a slow recording-off arm is the machine's load and is charged"
    );
    assert_eq!(
        block_machine_cost(&[(false, std::time::Duration::MAX), (false, secs(1))]),
        std::time::Duration::MAX,
        "the charge saturates rather than wrapping under the limit"
    );
}

/// A clock the arms advance by hand, so a wall is an exact number rather than a timing.
struct HandClock(std::rc::Rc<std::cell::Cell<std::time::Duration>>);

impl HandClock {
    fn new() -> Self {
        Self(std::rc::Rc::default())
    }
    fn pass(&self, secs: u64) {
        self.0
            .set(self.0.get() + std::time::Duration::from_secs(secs));
    }
    fn now(&self) -> impl Fn() -> std::time::Duration {
        || self.0.get()
    }
}

/// Advances the clock when dropped, as a take's teardown takes time after its arm is timed.
struct TeardownTakes(HandClock, u64);

impl Drop for TeardownTakes {
    fn drop(&mut self) {
        self.0.pass(self.1);
    }
}

#[test]
fn each_arm_is_timed_in_abba_order_from_launch_to_before_its_teardown() {
    let secs = std::time::Duration::from_secs;
    let clock = HandClock::new();
    let mut order = Vec::new();
    let arms = take_block_arms(clock.now(), |index, candidate| {
        order.push((index, candidate));
        clock.pass(10 + 10 * u64::try_from(index).expect("four arms"));
        Ok((TeardownTakes(HandClock(clock.0.clone()), 1000), true))
    })
    .expect("no arm failed");
    assert_eq!(
        order,
        [(0, false), (1, true), (2, true), (3, false)],
        "the ABBA order is what cancels linear drift"
    );
    assert_eq!(
        arms,
        [
            (false, secs(10)),
            (true, secs(20)),
            (true, secs(30)),
            (false, secs(40)),
        ],
        "negative control: 1000 s of teardown after each arm is charged to none of them"
    );
    assert_eq!(
        block_machine_cost(&arms),
        secs(100),
        "the walls handed over are the ones the charge rule doubles"
    );
}

#[test]
fn a_block_stopped_by_dropped_input_keeps_the_arms_it_took() {
    let secs = std::time::Duration::from_secs;
    let clock = HandClock::new();
    let arms = take_block_arms(clock.now(), |index, _| {
        clock.pass(5);
        Ok(((), index != 1))
    })
    .expect("no arm failed");
    assert_eq!(
        arms,
        [(false, secs(5)), (true, secs(5))],
        "the arm that ended the block is still counted and no later arm starts"
    );
}

#[test]
fn the_spend_forecast_is_the_mean_block_to_the_next_look() {
    let secs = std::time::Duration::from_secs;
    assert!(
        fits_spend(secs(0), 0, 32, SPEND),
        "no block to project from"
    );
    assert!(fits_spend(secs(320), 16, 8, SPEND), "320 + 8 * 20 = 480");
    assert!(!fits_spend(secs(320), 16, 9, SPEND), "320 + 9 * 20 = 500");
    assert!(fits_spend(SPEND, 8, 0, SPEND), "at the limit, none ahead");
    assert!(!fits_spend(SPEND + secs(1), 8, 0, SPEND), "past the limit");
    assert!(
        !fits_spend(std::time::Duration::MAX, 1, u32::MAX, SPEND),
        "the projection saturates rather than wrapping under the limit"
    );
}

#[test]
fn a_block_cancels_log_drift_without_counting_correlated_arms_twice() {
    // A, B, B, A each takes 10% longer than its predecessor, with no treatment effect.
    assert_eq!(
        block_ratios_milli(&[10_000, 13_310], &[11_000, 12_100]),
        Some(vec![1000])
    );
    assert_ne!(
        10_000 * 1000 / 11_000,
        1000,
        "negative control: adjacent ratios retain drift"
    );
    assert_eq!(
        block_ratios_milli(&[10_000, 13_310], &[11_000, 12_100])
            .unwrap()
            .len(),
        1
    );
    for (a, b) in [
        (&[][..], &[][..]),
        (&[1][..], &[1][..]),
        (&[1, 1][..], &[1][..]),
        (&[1, 0][..], &[1, 1][..]),
        (&[1, 1][..], &[0, 1][..]),
        (&[u64::MAX; 2][..], &[1, 1][..]),
    ] {
        assert_eq!(
            block_ratios_milli(a, b),
            None,
            "invalid blocks cannot be silently omitted"
        );
    }
}

#[test]
fn fixed_blocks_preserve_a_real_cost_and_an_undecided_interval() {
    let a = [1_000_000; 64];
    let decide = |b: &[u64]| {
        throughput_disposition(
            block_ratios_milli(&a, b)
                .as_deref()
                .and_then(|values| interval_at(values, 10)),
            970,
            0,
        )
    };
    assert_eq!(decide(&a), Disposition::Pass);
    assert_eq!(
        decide(&[1_040_000; 64]),
        Disposition::Fail,
        "negative control: drift cancellation must retain treatment cost"
    );
    let b: Vec<_> = (0..64)
        .map(|n| if n < 32 { 1_000_000 } else { 1_060_000 })
        .collect();
    assert_eq!(
        decide(&b),
        Disposition::Unproved,
        "a straddling interval is never a pass"
    );
    assert_eq!(
        throughput_disposition(None, 970, 0),
        Disposition::Unproved,
        "too few pairs prove nothing"
    );
}

#[test]
fn a_fast_recording_cannot_pass_by_losing_input() {
    let fast = interval_at(&[1000; 10], 2);
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
fn the_ordered_marker_needs_shell_output_not_command_echo_or_a_one_byte_mutant() {
    let marker = next_drain_marker().expect("a fresh marker");
    let next = next_drain_marker().expect("another marker");
    assert_ne!(marker, next);
    assert!(
        !drain_completed(&marker, &next),
        "a stale marker cannot complete a later drain"
    );
    let command = drain_completion_command(&marker);
    assert!(!command.contains(&marker));
    let mut shell = aterm_uds::spawnfd::command("/bin/sh");
    shell.args(["-c", &format!("printf 'prior drain output\\n'; {command}")]);
    let output =
        aterm_uds::deadline::output_within(&mut shell, super::super::instance::MUST_HAPPEN)
            .expect("the shell finishes its marker command");
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).expect("ASCII output");
    assert_eq!(text, format!("prior drain output\n\n{marker}\n"));
    let mutant = text.replacen(DRAIN_COMPLETE, "ATERM_DRAIN_COMPLETF", 1);
    for cols in [24, 40, 80, 120] {
        let mut screen = crate::Screen::new(24, cols);
        screen.feed(command.as_bytes()).feed(b"\r\n");
        assert!(
            !drain_completed(&screen.screen(), &marker),
            "echo at {cols} columns"
        );
        // A PTY's ONLCR translates the shell's LF to CRLF.
        screen.feed(mutant.replace('\n', "\r\n").as_bytes());
        assert!(
            !drain_completed(&screen.screen(), &marker),
            "negative control: one byte changed at {cols} columns"
        );
        screen.feed(text.replace('\n', "\r\n").as_bytes());
        assert!(
            drain_completed(&screen.screen(), &marker),
            "output at {cols} columns"
        );
    }
    assert!(!drain_completed(&format!("prefix {marker}\n"), &marker));
    assert!(!drain_completed(&format!("{marker} suffix\n"), &marker));
}

#[test]
fn the_completion_fence_reads_the_body_and_rejects_a_failed_text_request() {
    use super::super::instance::Reply;
    let marker = next_drain_marker().expect("marker");
    let mut reply = Reply {
        code: Some(0),
        stdout: format!("OK 2\n{marker}\n$ \n"),
        stderr: String::new(),
    };
    assert!(
        !drain_completed(reply.line(), &marker),
        "the header is no fence"
    );
    assert_eq!(drain_completion_reply(&reply, &marker), Ok(true));
    reply.code = Some(1);
    assert!(matches!(
        drain_completion_reply(&reply, &marker),
        Err(CouldNotRun::Refused(_))
    ));
    reply.code = Some(0);
    reply.stdout = "OK 1\n$ \n".into();
    assert_eq!(drain_completion_reply(&reply, &marker), Ok(false));
}

#[test]
fn loss_is_checked_only_after_live_writer_completion() {
    let receipt = "OK writer_gone=false writer_in_flight=1 writer_queued_bytes=8 dropped_input=0";
    let completed = "OK writer_gone=false writer_in_flight=0 writer_queued_bytes=0 dropped_input=1";
    assert!(!writer_finished(receipt).unwrap());
    assert!(writer_finished(completed).unwrap());
    assert_eq!(
        throughput_disposition(
            interval_at(&[1000; 10], 2),
            970,
            recording_drops(completed).unwrap()
        ),
        Disposition::Fail
    );
    assert!(
        writer_finished("OK writer_gone=false writer_in_flight=0 writer_queued_bytes=0").unwrap()
    );
    assert!(
        !writer_finished("OK writer_gone=false writer_in_flight=1 writer_queued_bytes=0").unwrap()
    );
    assert!(
        !writer_finished("OK writer_gone=false writer_in_flight=0 writer_queued_bytes=8").unwrap()
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
    let test =
        "measure::rewind_cost::tests::the_app_reading_counts_dirty_pages_and_not_clean_file_pages";
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
