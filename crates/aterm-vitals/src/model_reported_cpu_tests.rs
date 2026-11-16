// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::published_cpu::{CpuAccounting as Reported, CpuExecutionWindow};

fn reading(second: u64, published_ns: u64, busy_ticks: u64) -> Sample {
    let mut p = proc(200, 100, 0, 1, "reported-worker");
    p.cpu_ns = Some(published_ns);
    p.child_cpu_ns = Some(0);
    p.reaps_unwaited = Some(false);
    p.uid = Some(501);
    p.version = Some(9);
    p.at_ns = Some(second * 1_000_000_000);
    let mut s = sample(second, &[busy_ticks], vec![p]);
    s.at_unix_ms = second as i64 * 1000;
    s.mono_ms = second * 1000;
    s.self_uid = Some(501);
    s.cadence_ms = 1000;
    s
}

#[test]
fn reported_cpu_deferred_four_plus_one_retains_growth_without_actual_window_assurance() {
    for incomplete in [false, true] {
        let mut v = Vitals::new();
        v.absorb(&reading(10, 0, 100));
        let mut after = reading(11, 5_000_000, 101);
        after.procs_truncated = incomplete;
        let snap = v.absorb(&after);
        let row = snap.procs.iter().find(|p| p.pid == 200).unwrap();
        assert!((row.cores.unwrap() - 0.005).abs() < 1e-7);
        // Four ms executed before the first read but remained unpublished; only one ms ran
        // inside. The same published increment could also describe five ms executed inside.
        let actual_inside_ns = 1_000_000_u64;
        assert!(f64::from(row.cores.unwrap()) > actual_inside_ns as f64 / 1e9);
        assert_eq!(row.cpu_accounting.published, Reported::PUBLISHED_COUNTERS);
        let tab = snap.resource(Resource::Cpu, &[7]).tab;
        assert_eq!(tab.cpu_accounting, Some(Reported::PUBLISHED_COUNTERS));
        assert_eq!(
            tab.cpu_accounting.unwrap().execution_window,
            CpuExecutionWindow::Unavailable
        );
        if incomplete {
            assert!(tab.unread > 0);
        }
        assert_eq!(snap.bar_shares(Resource::Cpu, &[7]), None);
    }
}

#[test]
fn reported_cpu_zero_publication_is_not_assurance_of_zero_execution() {
    let mut v = Vitals::new();
    v.absorb(&reading(10, 0, 100));
    let snap = v.absorb(&reading(11, 0, 101));
    let actual_inside_ns = 1_000_000_u64; // Still pending publication at the second read.
    assert!(actual_inside_ns > 0);
    let row = snap.procs.iter().find(|p| p.pid == 200).unwrap();
    assert_eq!(row.cores, Some(0.0));
    assert_eq!(row.cpu_read_span_ns, Some(1_000_000_000));
    let cpu = snap.resource(Resource::Cpu, &[7]);
    assert_eq!(cpu.tab.now, Some(0.0));
    assert_eq!(
        cpu.tab.cpu_accounting.unwrap().execution_window,
        CpuExecutionWindow::Unavailable
    );
    assert_eq!(
        snap.tabs[0].scope.cpu_accounting,
        Some(Reported::PUBLISHED_COUNTERS)
    );
}

#[test]
fn reported_cpu_rows_retain_their_selected_span_without_relabeling_scope_rates() {
    for process_marks in [false, true] {
        let mut before = reading(10, 0, 100);
        let mut after = reading(11, 5_000_000, 101);
        before.procs.as_mut().unwrap()[0].at_ns = process_marks.then_some(10_500_000_000);
        after.procs.as_mut().unwrap()[0].at_ns = process_marks.then_some(11_750_000_000);
        let mut vitals = Vitals::new();
        vitals.absorb(&before);
        let snap = vitals.absorb(&after);
        let row = snap.procs.iter().find(|row| row.pid == 200).unwrap();
        let expected_span = if process_marks {
            1_250_000_000
        } else {
            1_000_000_000
        };
        assert_eq!(row.cpu_read_span_ns, Some(expected_span));
        assert!((row.cores.unwrap() - 5_000_000.0 / expected_span as f32).abs() < 1e-7);
        assert_eq!(snap.procs_window_ns, Some(1_000_000_000));
        assert!((snap.resource(Resource::Cpu, &[7]).tab.now.unwrap() - 0.005).abs() < 1e-7);
    }
}

#[test]
fn reported_cpu_above_machine_ticks_survives_ranking_and_history() {
    let mut v = Vitals::new();
    v.absorb(&reading(10, 0, 100));
    let snap = v.absorb(&reading(11, 2_000_000_000, 101));
    let cpu = snap.resource(Resource::Cpu, &[7]);
    assert!(cpu.now.unwrap() < 0.02);
    assert!((cpu.tab.now.unwrap() - 2.0).abs() < 1e-7);
    assert!(cpu.tab.run.iter().any(|cell| cell.mean == Some(2.0)));
    assert!((snap.tabs[0].scope.cores.raw - 2.0).abs() < 1e-7);
    assert_eq!(snap.top[0].cores, 2.0);
    assert_eq!(snap.programs[0].cores, 2.0);
    assert_eq!(
        snap.global.cpu_accounting, None,
        "machine ticks keep their independent basis"
    );
    assert_eq!(snap.bar_shares(Resource::Cpu, &[7]), None);
}

#[test]
fn reported_cpu_unknown_identity_exec_and_reset_are_not_numeric_zero() {
    for defect in 0..3 {
        let mut v = Vitals::new();
        let mut before = reading(10, 10_000_000, 100);
        let mut after = reading(11, 15_000_000, 101);
        match defect {
            0 => {
                for s in [&mut before, &mut after] {
                    let p = &mut s.procs.as_mut().unwrap()[0];
                    p.start = None;
                    p.version = None;
                }
            }
            1 => after.procs.as_mut().unwrap()[0].version = Some(10),
            _ => after.procs.as_mut().unwrap()[0].cpu_ns = Some(9_000_000),
        }
        v.absorb(&before);
        let snap = v.absorb(&after);
        let row = snap.procs.iter().find(|p| p.pid == 200).unwrap();
        assert_eq!(row.cores, None);
        assert_eq!(row.cpu_read_span_ns, None);
    }
}

#[test]
fn reported_cpu_legacy_restore_preserves_ticks_and_memory_but_not_scope_cpu() {
    let mut v = Vitals::new();
    v.absorb(&reading(10, 0, 100));
    v.absorb(&reading(11, 5_000_000, 101));
    let current = v.saved_strip(true);
    assert!(!current.aterm[Resource::Cpu.index()].is_empty());
    let mut legacy = current.clone();
    legacy.scope_cpu_accounting = None;
    let raw = legacy.clone();
    let mut restored = Vitals::new();
    restored.restore_strip_history(legacy, 11_000);
    let supported = restored.saved_strip(true);
    assert!(supported.aterm[Resource::Cpu.index()].is_empty());
    assert!(supported.aterm_cpu_charge.is_empty());
    assert!(
        !raw.aterm[Resource::Cpu.index()].is_empty(),
        "the raw decoded input was not mutated"
    );
    let mut restored = Vitals::new();
    restored.restore_strip_history(current.clone(), 11_000);
    let control = restored.saved_strip(true);
    assert!(!control.aterm[Resource::Cpu.index()].is_empty());
    assert_eq!(
        supported.rings[Resource::Cpu.index()],
        control.rings[Resource::Cpu.index()]
    );
    assert_eq!(
        supported.aterm[Resource::Memory.index()],
        control.aterm[Resource::Memory.index()]
    );
}

#[test]
fn reported_cpu_current_tail_never_retags_legacy_cells_or_accepts_legacy_append() {
    let mut legacy = SavedStrip::default();
    let mut old = Agg::default();
    old.add(99.0, 1000, false);
    legacy.aterm[Resource::Cpu.index()].put_closed(1, old);
    let mut current = Agg::default();
    current.add(0.005, 1000, false);
    assert!(legacy.replay(StripLine {
        scope_cpu_accounting: Some(Reported::PUBLISHED_COUNTERS),
        k: 2,
        wall_ms: 20_000,
        rings: vec![(9, current)],
        ..StripLine::default()
    }));
    assert_eq!(legacy.aterm[0].buckets.len(), 1);
    assert_eq!(legacy.aterm[0].buckets[0].1.mean, 0.005);
    assert!(legacy.replay(StripLine {
        k: 3,
        wall_ms: 30_000,
        rings: vec![(9, old)],
        ..StripLine::default()
    }));
    assert_eq!(legacy.aterm[0].buckets.len(), 1);
}
