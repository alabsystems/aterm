// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::self_cpu::{
    OwnCpuChanges, OwnCpuGroup, OwnCpuGrowth, OwnCpuProcess, OwnCpuWindowCoverage,
};

fn observed(thread: u64, process: u64) -> OwnCpuView {
    OwnCpuView::Observed(OwnCpuChanges {
        process: OwnCpuProcess {
            pid: 12,
            start: 100,
            version: 3,
        },
        before_begin_ns: 1,
        before_end_ns: 100,
        after_begin_ns: 1_999_999_999,
        after_end_ns: 2_000_000_000,
        process_growth: OwnCpuGrowth {
            lower_ns: process,
            upper_ns: process,
        },
        groups: vec![OwnCpuGroup {
            name: "aterm-vitals".into(),
            growth: OwnCpuGrowth {
                lower_ns: thread,
                upper_ns: thread,
            },
            threads: 1,
        }],
        coverage: OwnCpuWindowCoverage::default(),
    })
}

#[test]
fn named_self_cpu_deferred_growth_never_becomes_a_live_cpu_share() {
    // A publishes 4 ms from before the reads and 1 ms inside; others use 1 ms inside.
    // The reported 5/6 fraction cannot establish A's actual 1/2 share in the read window.
    let value = json(&observed(5_000_000, 6_000_000)).unwrap();
    assert_eq!(
        value.get("process_counter_growth_ns").unwrap().get("lower"),
        Some(&Jv::Int(6_000_000))
    );
    assert_eq!(
        value.get("cpu_window_attribution"),
        Some(&Jv::why("unavailable:deferred_accounting"))
    );
    assert_eq!(
        value.get("name_basis"),
        Some(&Jv::s("observed_at_both_cuts"))
    );
    assert!(value.get("split_mc").is_none());
}

#[test]
fn named_self_cpu_json_large_counter_growth_keeps_its_exact_value() {
    let value = json(&observed(0, u64::MAX)).unwrap();
    assert_eq!(
        value
            .get("process_counter_growth_ns")
            .unwrap()
            .get("upper")
            .unwrap()
            .get("decimal"),
        Some(&Jv::s(u64::MAX.to_string()))
    );
}

#[test]
fn named_self_cpu_unread_and_warming_are_explicit_without_inventing_zero() {
    assert!(json(&OwnCpuView::default()).is_none());
    assert!(text(&OwnCpuView::default()).is_none());
    assert_eq!(json(&OwnCpuView::Warming), Some(Jv::why("warming")));
    assert_eq!(
        json(&OwnCpuView::Unavailable("process_generation_changed")),
        Some(Jv::why("unavailable:process_generation_changed"))
    );
}

#[test]
fn named_self_cpu_json_obeys_the_existing_unknown_contract() {
    for view in [
        observed(799, 1000),
        observed(800, 1000),
        OwnCpuView::Warming,
        OwnCpuView::Unavailable("counter_overflow"),
        OwnCpuView::Unavailable("thread_list_unread"),
        OwnCpuView::Unavailable("unsupported"),
        OwnCpuView::Unavailable("bad reason"),
    ] {
        let value = json(&view).unwrap();
        assert!(
            crate::sys::json::contract_violations(&value).is_empty(),
            "{value:?}"
        );
    }
}
