// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn counter(ns: u64) -> OwnCpuCounter {
    OwnCpuCounter {
        ns,
        precision_ns: 0,
    }
}

fn thread(id: u64, name: Option<&str>, ns: u64) -> OwnCpuThread {
    OwnCpuThread {
        id,
        name: name.map(str::to_owned),
        cpu: Some(counter(ns)),
    }
}

fn cut(begin: u64, before: u64, after: u64, threads: Vec<OwnCpuThread>) -> OwnCpuCut {
    OwnCpuCut {
        process: OwnCpuProcess {
            pid: 42,
            start: 123,
            version: 7,
        },
        begin_ns: begin,
        end_ns: begin + 10,
        process_before: counter(before),
        process_after: counter(after),
        coverage: OwnCpuCoverage {
            listed: threads.len(),
            inspected: threads.len(),
            ..OwnCpuCoverage::default()
        },
        threads,
    }
}

fn pair(before: &OwnCpuCut, after: &OwnCpuCut) -> OwnCpuView {
    let mut state = OwnCpuState::default();
    assert_eq!(state.absorb(Some(Ok(before))), OwnCpuView::Warming);
    state.absorb(Some(Ok(after)))
}

fn observed(view: OwnCpuView) -> OwnCpuChanges {
    match view {
        OwnCpuView::Observed(changes) => changes,
        other => panic!("expected counter observations, got {other:?}"),
    }
}

#[test]
fn named_self_cpu_default_is_not_a_claim_that_a_cut_was_read() {
    assert_eq!(
        OwnCpuView::default(),
        OwnCpuView::Unavailable("not_sampled")
    );
    assert_eq!(
        crate::Snapshot::default().self_cpu,
        OwnCpuView::Unavailable("not_sampled")
    );
}

#[test]
fn named_self_cpu_quantization_bounds_both_ends_of_counter_growth() {
    let mut before = cut(0, 1000, 1000, vec![thread(1, Some("aterm-vitals"), 1000)]);
    before.process_after.precision_ns = 10;
    before.threads[0].cpu.as_mut().unwrap().precision_ns = 20;
    let mut after = cut(100, 2000, 2000, vec![thread(1, Some("aterm-vitals"), 1800)]);
    after.process_after.precision_ns = 40;
    after.threads[0].cpu.as_mut().unwrap().precision_ns = 30;
    let changes = observed(pair(&before, &after));
    assert_eq!(
        changes.process_growth,
        OwnCpuGrowth {
            lower_ns: 990,
            upper_ns: 1040
        }
    );
    assert_eq!(
        changes.groups[0].growth,
        OwnCpuGrowth {
            lower_ns: 780,
            upper_ns: 830
        }
    );
}

#[test]
fn named_self_cpu_preserves_both_read_brackets_and_process_observation_skew() {
    let mut before = cut(0, 1000, 1100, vec![thread(1, Some("aterm-vitals"), 1000)]);
    before.end_ns = 100;
    let mut after = cut(
        1000,
        2000,
        2200,
        vec![thread(1, Some("aterm-vitals"), 1800)],
    );
    after.end_ns = 1200;
    let changes = observed(pair(&before, &after));
    assert_eq!((changes.before_begin_ns, changes.before_end_ns), (0, 100));
    assert_eq!((changes.after_begin_ns, changes.after_end_ns), (1000, 1200));
    assert_eq!(
        changes.process_growth,
        OwnCpuGrowth {
            lower_ns: 900,
            upper_ns: 1200
        }
    );
    assert_eq!(
        changes.groups[0].growth,
        OwnCpuGrowth {
            lower_ns: 800,
            upper_ns: 800
        }
    );
}

#[test]
fn named_self_cpu_partial_lists_never_rescale_observed_counter_growth() {
    let mut before = cut(0, 0, 0, vec![thread(1, Some("aterm-vitals"), 0)]);
    let mut after = cut(100, 1000, 1000, vec![thread(1, Some("aterm-vitals"), 499)]);
    let complete = observed(pair(&before, &after));
    before.coverage.listed = 20;
    before.coverage.capped = true;
    after.coverage = before.coverage;
    let partial = observed(pair(&before, &after));
    assert_eq!(partial.groups, complete.groups);
    assert_eq!(partial.process_growth, complete.process_growth);
    assert!(partial.coverage.before.capped && partial.coverage.after.capped);
}

#[test]
fn named_self_cpu_deferred_publication_is_growth_not_cpu_confined_to_read_brackets() {
    const MS: u64 = 1_000_000;
    // A has 4 ms of CPU already executed but unpublished before the first bracket. It runs
    // another 1 ms after that bracket begins, then publishes all 5 ms. Other threads run 1 ms.
    // These observations are compatible with only 1 ms of A's CPU inside the read brackets.
    let before = cut(
        10 * MS,
        50 * MS,
        50 * MS,
        vec![thread(1, Some("A"), 10 * MS), thread(2, Some("B"), 20 * MS)],
    );
    let after = cut(
        1010 * MS,
        56 * MS,
        56 * MS,
        vec![thread(1, Some("A"), 15 * MS), thread(2, Some("B"), 21 * MS)],
    );
    let actual_a_inside_brackets = MS;
    let actual_all_inside_brackets = 2 * MS;
    let changes = observed(pair(&before, &after));
    assert_eq!(changes.groups[0].name, "A");
    assert_eq!(
        changes.groups[0].growth,
        OwnCpuGrowth {
            lower_ns: 5 * MS,
            upper_ns: 5 * MS
        }
    );
    assert_eq!(
        changes.process_growth,
        OwnCpuGrowth {
            lower_ns: 6 * MS,
            upper_ns: 6 * MS
        }
    );
    assert!(changes.groups[0].growth.lower_ns > actual_a_inside_brackets);
    assert!(changes.process_growth.lower_ns > actual_all_inside_brackets);
    // No actual-window CPU bound or share is produced: both brackets describe read times only.
    assert_eq!(
        (changes.before_begin_ns, changes.after_begin_ns),
        (10 * MS, 1010 * MS)
    );
}

#[test]
fn named_self_cpu_asynchronous_groups_are_not_a_partition_of_process_growth() {
    let before = cut(
        0,
        0,
        0,
        vec![thread(1, Some("a"), 0), thread(2, Some("b"), 0)],
    );
    let after = cut(
        100,
        1000,
        1000,
        vec![thread(1, Some("a"), 600), thread(2, Some("b"), 600)],
    );
    let changes = observed(pair(&before, &after));
    assert_eq!(changes.groups.len(), 2);
    assert_eq!(
        changes.process_growth,
        OwnCpuGrowth {
            lower_ns: 1000,
            upper_ns: 1000
        }
    );
    for group in changes.groups {
        assert_eq!(
            group.growth,
            OwnCpuGrowth {
                lower_ns: 600,
                upper_ns: 600
            }
        );
    }
}

#[test]
fn named_self_cpu_groups_only_exact_stable_names_and_sorts_independent_growth() {
    let before = cut(
        0,
        0,
        0,
        vec![
            thread(1, Some("aterm-net read"), 0),
            thread(2, Some("aterm-net read"), 0),
            thread(3, Some("aterm-net write"), 0),
            thread(4, Some("aterm-aaa"), 0),
        ],
    );
    let mut after = cut(
        100,
        1200,
        1200,
        vec![
            thread(1, Some("aterm-net read"), 400),
            thread(2, Some("aterm-net read"), 400),
            thread(3, Some("aterm-net write"), 200),
            thread(4, Some("aterm-aaa"), 200),
        ],
    );
    after.threads[0].cpu.as_mut().unwrap().precision_ns = 3;
    after.threads[1].cpu.as_mut().unwrap().precision_ns = 7;
    let changes = observed(pair(&before, &after));
    assert_eq!(changes.groups.len(), 3);
    assert_eq!(
        (changes.groups[0].name.as_str(), changes.groups[0].threads),
        ("aterm-net read", 2)
    );
    assert_eq!(
        changes.groups[0].growth,
        OwnCpuGrowth {
            lower_ns: 800,
            upper_ns: 810
        }
    );
    assert_eq!(
        (
            changes.groups[1].name.as_str(),
            changes.groups[2].name.as_str()
        ),
        ("aterm-aaa", "aterm-net write")
    );
}

#[test]
fn named_self_cpu_changed_missing_and_truncated_names_have_no_named_group() {
    let before = cut(0, 0, 0, vec![thread(1, Some("aterm-vitals"), 0)]);
    for name in [Some("aterm-render"), None, Some("")] {
        let after = cut(100, 1000, 1000, vec![thread(1, name, 1000)]);
        let changes = observed(pair(&before, &after));
        assert!(changes.groups.is_empty());
        assert_eq!(changes.coverage.unnamed, 1);
        assert_eq!(
            changes.coverage.name_changes,
            usize::from(name.is_some_and(|n| n != "aterm-vitals"))
        );
    }
}

#[test]
fn named_self_cpu_missing_counters_and_new_or_exited_threads_keep_coverage_unknown() {
    let before = cut(
        0,
        0,
        0,
        vec![thread(1, Some("known"), 0), thread(2, Some("exits"), 0)],
    );
    let mut after = cut(
        100,
        1000,
        1000,
        vec![thread(1, Some("known"), 500), thread(3, Some("new"), 500)],
    );
    after.threads[0].cpu = None;
    after.coverage.unread = 1;
    let changes = observed(pair(&before, &after));
    assert_eq!((changes.coverage.unpaired, changes.coverage.unread), (2, 1));
    assert!(changes.groups.is_empty());
    assert_eq!(
        changes.process_growth,
        OwnCpuGrowth {
            lower_ns: 1000,
            upper_ns: 1000
        }
    );
}

#[test]
fn named_self_cpu_missing_reads_and_reader_errors_clear_the_previous_cut() {
    let before = cut(0, 0, 0, vec![thread(1, Some("known"), 0)]);
    let after = cut(100, 1000, 1000, vec![thread(1, Some("known"), 1000)]);
    for missing in [None, Some(Err("reader_unavailable"))] {
        let mut state = OwnCpuState::default();
        state.absorb(Some(Ok(&before)));
        assert!(matches!(state.absorb(missing), OwnCpuView::Unavailable(_)));
        assert_eq!(state.absorb(Some(Ok(&after))), OwnCpuView::Warming);
    }
}

#[test]
fn named_self_cpu_pid_start_and_exec_generation_edges_never_share_observations() {
    let before = cut(0, 0, 0, vec![]);
    for process in [
        OwnCpuProcess {
            pid: 43,
            ..before.process
        },
        OwnCpuProcess {
            start: 124,
            ..before.process
        },
        OwnCpuProcess {
            version: 8,
            ..before.process
        },
    ] {
        let mut after = cut(100, 1000, 1000, vec![]);
        after.process = process;
        assert_eq!(
            pair(&before, &after),
            OwnCpuView::Unavailable("process_changed")
        );
    }
}

#[test]
fn named_self_cpu_process_and_thread_counter_resets_are_unavailable() {
    let before = cut(0, 1000, 1000, vec![thread(1, Some("known"), 1000)]);
    let process_reset = cut(100, 999, 1100, vec![thread(1, Some("known"), 1100)]);
    assert_eq!(
        pair(&before, &process_reset),
        OwnCpuView::Unavailable("process_counter_reset")
    );
    let thread_reset = cut(100, 1100, 1100, vec![thread(1, Some("known"), 999)]);
    assert_eq!(
        pair(&before, &thread_reset),
        OwnCpuView::Unavailable("thread_counter_reset")
    );
    let backwards_cut = cut(100, 1100, 1099, vec![]);
    assert_eq!(
        pair(&before, &backwards_cut),
        OwnCpuView::Unavailable("process_counter_reset")
    );
}

#[test]
fn named_self_cpu_invalid_duplicate_and_overlapping_reads_never_make_changes() {
    let before = cut(0, 0, 0, vec![]);
    for end in [99, 100] {
        let mut invalid = cut(100, 1000, 1000, vec![]);
        invalid.end_ns = end;
        assert_eq!(
            pair(&before, &invalid),
            OwnCpuView::Unavailable("invalid_read_interval")
        );
    }
    let overlapping = cut(10, 1000, 1000, vec![]);
    assert_eq!(
        pair(&before, &overlapping),
        OwnCpuView::Unavailable("overlapping_read_intervals")
    );
    let duplicate = cut(
        100,
        1000,
        1000,
        vec![thread(1, Some("known"), 1), thread(1, Some("known"), 1)],
    );
    assert_eq!(
        pair(&before, &duplicate),
        OwnCpuView::Unavailable("invalid_thread_identity")
    );
    let mut invalid_coverage = cut(100, 1000, 1000, vec![]);
    invalid_coverage.coverage.inspected = 1;
    assert_eq!(
        pair(&before, &invalid_coverage),
        OwnCpuView::Unavailable("invalid_thread_coverage")
    );
}

#[test]
fn named_self_cpu_large_counter_ranges_use_wide_arithmetic_without_wrapping() {
    let before = cut(0, 0, 0, vec![thread(1, Some("known"), 0)]);
    let mut after = cut(
        100,
        u64::MAX,
        u64::MAX,
        vec![thread(1, Some("known"), u64::MAX)],
    );
    let changes = observed(pair(&before, &after));
    assert_eq!(
        changes.groups[0].growth,
        OwnCpuGrowth {
            lower_ns: u64::MAX,
            upper_ns: u64::MAX
        }
    );
    after.process_after.precision_ns = 1;
    assert_eq!(
        pair(&before, &after),
        OwnCpuView::Unavailable("counter_overflow")
    );
    let mut before = cut(
        0,
        u64::MAX - 1,
        u64::MAX - 1,
        vec![thread(1, Some("known"), u64::MAX - 1)],
    );
    before.threads[0].cpu.as_mut().unwrap().precision_ns = u64::MAX;
    let after = cut(
        100,
        u64::MAX,
        u64::MAX,
        vec![thread(1, Some("known"), u64::MAX)],
    );
    assert_eq!(
        observed(pair(&before, &after)).groups[0].growth,
        OwnCpuGrowth {
            lower_ns: 0,
            upper_ns: 1
        }
    );
}

#[test]
fn named_self_cpu_group_growth_overflow_is_unavailable_instead_of_capped() {
    let before = cut(
        0,
        0,
        0,
        vec![thread(1, Some("same"), 0), thread(2, Some("same"), 0)],
    );
    let after = cut(
        100,
        u64::MAX,
        u64::MAX,
        vec![
            thread(1, Some("same"), u64::MAX),
            thread(2, Some("same"), 1),
        ],
    );
    assert_eq!(
        pair(&before, &after),
        OwnCpuView::Unavailable("counter_overflow")
    );
}

#[test]
fn named_self_cpu_growth_ranges_contain_each_monotone_quantization_choice() {
    for old_precision in [0, 2, 7] {
        for new_precision in [0, 3, 9] {
            for increment in [0, 1, 20] {
                let mut before = cut(0, 100, 100, vec![thread(1, Some("known"), 100)]);
                let mut after = cut(
                    100,
                    200,
                    200,
                    vec![thread(1, Some("known"), 100 + increment)],
                );
                before.threads[0].cpu.as_mut().unwrap().precision_ns = old_precision;
                after.threads[0].cpu.as_mut().unwrap().precision_ns = new_precision;
                let changes = observed(pair(&before, &after));
                let range = changes.groups[0].growth;
                assert!(range.lower_ns <= range.upper_ns);
                for old in 100..=100 + old_precision {
                    for new in 100 + increment..=100 + increment + new_precision {
                        if let Some(delta) = new.checked_sub(old) {
                            assert!(range.lower_ns <= delta && delta <= range.upper_ns);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn named_self_cpu_never_supplies_app_or_tab_cpu_charges() {
    let mut model = crate::Vitals::default();
    let mut sample = crate::Sample {
        self_cpu: Some(Ok(cut(0, 0, 0, vec![thread(1, Some("known"), 0)]))),
        ..crate::Sample::default()
    };
    assert_eq!(model.absorb(&sample).self_cpu, OwnCpuView::Warming);
    sample.at_ns = 100;
    sample.self_cpu = Some(Ok(cut(
        100,
        1000,
        1000,
        vec![thread(1, Some("known"), 1000)],
    )));
    let snap = model.absorb(&sample);
    assert!(matches!(snap.self_cpu, OwnCpuView::Observed(_)));
    assert!(snap.cpu.is_none() && snap.aterm.is_none() && snap.scoped_cpu.is_none());
}

#[test]
fn named_self_cpu_pid_version_is_an_opaque_signed_generation_token() {
    let mut before = cut(0, 0, 0, vec![thread(1, Some("known"), 0)]);
    let mut after = cut(100, 1000, 1000, vec![thread(1, Some("known"), 1000)]);
    before.process.version = i32::MIN;
    after.process.version = i32::MIN;
    assert_eq!(
        observed(pair(&before, &after)).groups[0].growth,
        OwnCpuGrowth {
            lower_ns: 1000,
            upper_ns: 1000
        }
    );
    after.process.version += 1;
    assert_eq!(
        pair(&before, &after),
        OwnCpuView::Unavailable("process_changed")
    );
}
