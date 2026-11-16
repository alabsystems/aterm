// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::explain::detect::{ActivityId, Evidence};
use crate::explain::{CoalSplit, DeckCause, OpenCause};
use crate::model::{MemRates, MemSample, MemView, Pressure, Thermal, Throttle};
use crate::published_cpu::CpuAccounting;

fn observed(tab: i64, global: i64) -> Snapshot {
    Snapshot {
        aterm: Some(Default::default()),
        why: Some(Arc::new(WhyFacts {
            split: CoalSplit {
                cpu_accounting: CpuAccounting::PUBLISHED_COUNTERS,
                coverage_complete: true,
                global_mc: global,
                aterm_mc: tab,
                tabs: vec![(1, tab)],
                tab_reported: vec![(1, tab)],
                ..CoalSplit::default()
            },
            ..WhyFacts::default()
        })),
        mem: Some(MemView {
            sample: MemSample {
                total: 16 << 30,
                pressure: Pressure::Normal,
                ..MemSample::default()
            },
            rates: Some(MemRates::default()),
            ..MemView::default()
        }),
        thermal: Some(Thermal::default()),
        ..Snapshot::default()
    }
}

fn no_physical_cpu_claim(snapshot: &Snapshot, figure: i64) {
    let account = account(WhySubject::Cpu, Some(1), snapshot);
    assert_eq!(
        account.you.role,
        Role::Unavailable("execution_window_unknown")
    );
    assert_eq!(account.you.figure, Some(figure));
    assert_eq!(account.you.share_pm, None);
    assert_eq!(account.you.unattributed_mc, None);
    assert!(!account.you.calm && !account.nothing_needed);
    assert!(
        account
            .causes
            .iter()
            .all(|shown| !shown.cause.id.covering())
    );
    assert_ne!(bare_subject(snapshot), Bare::Calm);
    assert!(
        account
            .cannot
            .contains(&("cpu_execution_window", "execution_window_unknown"))
    );
}

#[test]
fn reported_cpu_deferred_four_plus_one_ms_is_not_an_execution_window_role() {
    // The first cut omits 4 ms already executed. The second publishes it plus the tab's
    // 1 ms inside the interval; another owner executed 1 ms there. Published 5/6 would
    // exceed 80%, while actual work inside that interval is only 1/2.
    let before_cut_ns = 4_000_000;
    let tab_inside_ns = 1_000_000;
    let other_inside_ns = 1_000_000;
    let published = before_cut_ns + tab_inside_ns;
    assert!(published * 5 >= (published + other_inside_ns) * 4);
    assert!(tab_inside_ns * 5 < (tab_inside_ns + other_inside_ns) * 4);
    let snapshot = observed(published / 1000, (tab_inside_ns + other_inside_ns) / 1000);
    no_physical_cpu_claim(&snapshot, 5000);
    assert!(snapshot.why.as_ref().unwrap().split.coverage_complete);
}

#[test]
fn reported_cpu_zero_growth_with_positive_unpublished_work_is_never_none_or_calm() {
    // The same observations permit a running owner to have executed CPU whose counter has
    // not published yet. A zero observation cannot prove no work or that no remedy is needed.
    let executed_ns = 1_000_000;
    let published_ns = 0;
    assert!(executed_ns > published_ns);
    no_physical_cpu_claim(&observed(published_ns, 0), 0);
}

#[test]
fn reported_cpu_own_counter_figure_stays_separate_from_reconciled_charge() {
    let mut snapshot = observed(5000, 6000);
    Arc::make_mut(snapshot.why.as_mut().unwrap()).split.tabs = vec![(1, 9000)];
    no_physical_cpu_claim(&snapshot, 5000);
    let diagnostic = account(WhySubject::Cpu, Some(1), &snapshot);
    assert_eq!(diagnostic.parts.map(|parts| parts.tab), Some(9000));
}

#[test]
fn reported_cpu_retained_causes_do_not_hide_independent_machine_contexts() {
    let mut snapshot = observed(5000, 6000);
    let facts = Arc::make_mut(snapshot.why.as_mut().unwrap());
    facts.causes = vec![Candidate {
        id: CauseId::TabWork(1),
        covers_mc: 5000,
        coalitions: vec![7],
        since_ms: Some(1),
        evidence: Evidence::Measured,
        contexts: vec![Context::Startup],
    }];
    facts.open = vec![OpenCause {
        id: CauseId::TabWork(1),
        opened_unix_ms: 1,
        since_ms: Some(1),
        holds: true,
        covers_mc: 5000,
        contexts: vec![Context::Wake],
    }];
    facts.clause = Some(DeckCause {
        activity: ActivityId::Spotlight,
        word: ActivityId::Spotlight.activity().word,
        context: Some(Context::Startup),
    });
    snapshot.thermal = Some(Thermal {
        level: Throttle::Light,
        ..Thermal::default()
    });
    snapshot.mem.as_mut().unwrap().sample.pressure = Pressure::Warn;
    no_physical_cpu_claim(&snapshot, 5000);
    let cpu = account(WhySubject::Cpu, Some(1), &snapshot);
    assert_eq!(
        cpu.causes
            .iter()
            .map(|shown| shown.cause.id)
            .collect::<Vec<_>>(),
        [
            CauseId::Context(Context::Throttled),
            CauseId::Context(Context::Pressure),
        ]
    );
    assert!(
        cpu.causes
            .iter()
            .all(|shown| shown.opened_unix_ms.is_none())
    );
    let memory = account(WhySubject::Mem, Some(1), &snapshot);
    assert_eq!(
        memory
            .causes
            .iter()
            .map(|shown| shown.cause.id)
            .collect::<Vec<_>>(),
        [CauseId::Context(Context::Pressure),]
    );
    assert_eq!(bare_subject(&snapshot), Bare::Subject(WhySubject::Mem));
    assert_eq!(super::super::deck_cause(&snapshot), None);
}

#[test]
fn reported_cpu_missing_or_cut_observations_keep_their_coverage_reason() {
    let mut snapshot = observed(5000, 6000);
    snapshot.procs_truncated = true;
    let cut = account(WhySubject::Cpu, Some(1), &snapshot);
    assert_eq!(cut.you.role, Role::Unavailable("truncated"));
    assert_eq!(cut.you.figure, None);
    assert!(cut.parts.is_some());
    snapshot.why = None;
    let absent = account(WhySubject::Cpu, Some(1), &snapshot);
    assert_eq!(absent.you.figure, None);
    assert!(matches!(absent.you.role, Role::Unavailable(_)));
    assert!(!absent.nothing_needed);
}
