// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::model::{
    CpuView, MemRates, MemSample, MemView, ScopeView, TabView, Throttle, VolumeSample,
};
use crate::trend::TrendView;

fn snap(seq: u64, at_s: u64) -> Snapshot {
    Snapshot {
        seq,
        at_ns: at_s * 1_000_000_000,
        ..Snapshot::default()
    }
}

fn kinds(e: &[Event]) -> Vec<&'static str> {
    e.iter().map(|e| e.kind.word()).collect()
}

#[test]
fn pressure_and_thermal_changes_are_events_and_the_first_reading_is_not() {
    let mut d = Detector::default();
    let mut s = snap(1, 1);
    s.mem = Some(MemView {
        sample: MemSample {
            total: 1,
            pressure: Pressure::Normal,
            ..MemSample::default()
        },
        ..MemView::default()
    });
    s.thermal = Some(Thermal::default());
    assert!(d.detect(&s).is_empty());
    let mut s2 = snap(2, 2);
    s2.mem = s.mem.clone();
    s2.mem.as_mut().unwrap().sample.pressure = Pressure::Warn;
    s2.thermal = Some(Thermal {
        level: Throttle::Heavy,
        raw: 2,
        low_power: None,
        exact: true,
    });
    let e = d.detect(&s2);
    assert_eq!(kinds(&e), ["pressure", "thermal"]);
    assert_eq!(e[0].seq, 2);
    assert_eq!(
        e[0].kind,
        EventKind::Pressure {
            from: Pressure::Normal,
            to: Pressure::Warn
        }
    );
    let mut s3 = snap(3, 3);
    s3.mem = s2.mem.clone();
    s3.mem.as_mut().unwrap().sample.pressure = Pressure::Unknown;
    assert!(d.detect(&s3).is_empty());
}

#[test]
fn swap_traffic_starting_and_stopping_are_events() {
    let mut d = Detector::default();
    let at = |seq, bps: f64| {
        let mut s = snap(seq, seq);
        s.mem = Some(MemView {
            sample: MemSample {
                total: 1,
                ..MemSample::default()
            },
            rates: Some(MemRates {
                swapin_bps: bps,
                ..MemRates::default()
            }),
            ..MemView::default()
        });
        s
    };
    assert!(d.detect(&at(1, 0.0)).is_empty());
    let e = d.detect(&at(2, 5e6));
    assert_eq!(
        e[0].kind,
        EventKind::Swap {
            started: true,
            bps: 5_000_000
        }
    );
    assert!(d.detect(&at(3, 4e6)).is_empty());
    assert!(d.detect(&at(3, f64::NAN)).is_empty());
    let e = d.detect(&at(4, 0.0));
    assert_eq!(
        e[0].kind,
        EventKind::Swap {
            started: false,
            bps: 0
        }
    );
}

#[test]
fn free_space_crossing_a_line_either_way_is_an_event() {
    let mut d = Detector::default();
    let at = |seq, available: u64| {
        let mut s = snap(seq, seq);
        s.volume = Some(VolumeSample {
            total: 100_000_000_000,
            free: available,
            reserved: 0,
            mount: "/".into(),
        });
        s
    };
    assert!(d.detect(&at(1, 50_000_000_000)).is_empty());
    let e = d.detect(&at(2, 9_000_000_000));
    assert_eq!(e.len(), 1);
    assert!(matches!(
        e[0].kind,
        EventKind::Disk {
            line: DiskLine::Pct10,
            below: true,
            ..
        }
    ));
    let e = d.detect(&at(3, 500_000_000));
    let lines: Vec<&str> = e
        .iter()
        .map(|e| match &e.kind {
            EventKind::Disk { line, .. } => line.word(),
            _ => "",
        })
        .collect();
    assert_eq!(lines, ["5%", "1GB"]);
    let e = d.detect(&at(4, 50_000_000_000));
    assert_eq!(e.len(), 3);
    assert!(
        e.iter()
            .all(|e| matches!(e.kind, EventKind::Disk { below: false, .. }))
    );
}

#[test]
fn a_load_move_of_a_core_that_holds_ten_seconds_is_one_event() {
    let mut d = Detector::default();
    let at = |seq: u64, level: f32| {
        let mut s = snap(seq, seq);
        s.cpu = Some(CpuView {
            busy: TrendView::flat(level),
            ..CpuView::default()
        });
        s.global.top = Some(("trustc".into(), 2.0));
        s
    };
    assert!(d.detect(&at(1, 0.5)).is_empty());
    assert!(d.detect(&at(2, 3.0)).is_empty());
    assert!(d.detect(&at(3, 0.6)).is_empty());
    for t in 4..14 {
        assert!(d.detect(&at(t, 3.0)).is_empty(), "t={t}");
    }
    let e = d.detect(&at(14, 3.1));
    assert_eq!(
        e[0].kind,
        EventKind::Load {
            scope: LoadScope::Global,
            from_mc: 500,
            to_mc: 3100,
            top: Some("trustc".into()),
        }
    );
    for t in 15..40 {
        assert!(d.detect(&at(t, 3.1)).is_empty());
    }
    for t in 41..56 {
        assert!(
            d.detect(&at(t, f32::NAN)).is_empty(),
            "a NaN level is no move (t={t})"
        );
    }
    assert!(d.detect(&at(56, f32::INFINITY)).is_empty());
    for t in 57..67 {
        assert!(d.detect(&at(t, 0.5)).is_empty(), "t={t}");
    }
    assert_eq!(
        d.detect(&at(67, 0.5))[0].kind,
        EventKind::Load {
            scope: LoadScope::Global,
            from_mc: 3100,
            to_mc: 500,
            top: Some("trustc".into()),
        },
        "control: the reference the NaN levels skipped still moves on a real step"
    );
}

#[test]
fn tabs_opening_and_closing_are_events_after_the_first_sweep() {
    let mut d = Detector::default();
    let at = |seq, keys: &[u64]| {
        let mut s = snap(seq, seq);
        s.aterm = Some(ScopeView::default());
        s.tabs = keys
            .iter()
            .map(|&k| TabView {
                key: k,
                tab: k,
                label: format!("Tab {k}"),
                program: String::new(),
                shell_pid: 0,
                scope: ScopeView::default(),
            })
            .collect();
        s
    };
    assert!(d.detect(&at(1, &[1, 2])).is_empty());
    let e = d.detect(&at(2, &[2, 3]));
    assert_eq!(
        e.iter().map(|e| e.kind.clone()).collect::<Vec<_>>(),
        [
            EventKind::Tab {
                key: 3,
                label: "Tab 3".into(),
                opened: true
            },
            EventKind::Tab {
                key: 1,
                label: "Tab 1".into(),
                opened: false
            }
        ]
    );
    assert!(d.detect(&snap(3, 3)).iter().all(|e| e.kind.word() != "tab"));
}

#[test]
fn a_reader_going_away_and_coming_back_is_a_lane_event() {
    let mut d = Detector::default();
    let mut s = snap(1, 1);
    s.mem = Some(MemView::default());
    assert!(d.detect(&s).is_empty());
    let e = d.detect(&snap(2, 2));
    assert!(e.iter().any(|e| e.kind
        == EventKind::Lane {
            lane: "mem",
            available: false
        }));
    let e = d.detect(&s);
    assert!(e.iter().any(|e| e.kind
        == EventKind::Lane {
            lane: "mem",
            available: true
        }));
}

#[test]
fn a_closed_tab_whose_processes_live_on_is_an_orphan_event() {
    use crate::model::{ProcState, ProcView};
    let mut d = Detector::default();
    let tab = |k: u64| TabView {
        key: k,
        tab: k,
        label: format!("Tab {k}"),
        program: String::new(),
        shell_pid: 0,
        scope: ScopeView::default(),
    };
    let proc = |pid: u32, name: &str, scope: RowScope, cores: f32| ProcView {
        scheduling: None,
        cpu_accounting: crate::model::CpuAccounting::default(),
        pid,
        ppid: 1,
        pgid: pid,
        name: name.into(),
        cores: Some(cores),
        wait: None,
        footprint: None,
        peak: None,
        read_bps: None,
        write_bps: None,
        state: ProcState::Run,
        scope,
        refused: false,
        gpu: None,
        birth_ms: None,
    };
    let mut s1 = snap(1, 1);
    s1.aterm = Some(ScopeView::default());
    s1.tabs = vec![tab(1), tab(2)];
    s1.procs = vec![
        proc(10, "sh", RowScope::Tab(2), 0.0),
        proc(11, "node", RowScope::Tab(2), 0.4),
        proc(12, "make", RowScope::Tab(2), 0.9),
        proc(20, "sh", RowScope::Tab(1), 0.0),
    ];
    assert!(d.detect(&s1).is_empty());
    let mut s2 = snap(2, 2);
    s2.aterm = Some(ScopeView::default());
    s2.tabs = vec![tab(1)];
    s2.procs = vec![
        proc(11, "node", RowScope::Others, 0.4),
        proc(12, "cc", RowScope::Others, 0.9),
        proc(20, "sh", RowScope::Tab(1), 0.0),
    ];
    let e = d.detect(&s2);
    assert_eq!(kinds(&e), ["tab", "orphan"]);
    assert_eq!(
        e[1].kind,
        EventKind::Orphan {
            key: 2,
            label: "Tab 2".into(),
            procs: 1,
            top: Some("node".into()),
        }
    );
    let mut s3 = snap(3, 3);
    s3.aterm = Some(ScopeView::default());
    s3.tabs = Vec::new();
    s3.procs = vec![proc(11, "node", RowScope::Others, 0.4)];
    assert_eq!(kinds(&d.detect(&s3)), ["tab"]);
}

#[test]
fn a_job_whose_verdict_moves_is_a_job_event_and_a_steady_one_is_not() {
    use crate::jobs::JobsView;
    use crate::sys::jobs_view::fixtures::{row, stuck};
    let mut w = JobWatch::default();
    let view = |rows: Vec<crate::jobs::JobRow>| JobsView {
        rows,
        ..JobsView::default()
    };
    let e = w.step(
        &view(vec![
            row("a1", 7, Class::Running, Verdict::Fine),
            stuck("a2", 7),
        ]),
        5,
        9,
    );
    assert!(w.seeded());
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e[0].seq, 5);
    assert!(matches!(
        &e[0].kind,
        EventKind::Job { id, from: None, to: Verdict::Stuck, class: Class::Waiting, .. } if id == "a2"
    ));
    let steady = view(vec![
        row("a1", 7, Class::Running, Verdict::Fine),
        stuck("a2", 7),
    ]);
    assert!(w.step(&steady, 6, 10).is_empty());
    let mut done = row("a1", 7, Class::Exited, Verdict::Exited);
    done.exit = Some(0);
    let e = w.step(
        &view(vec![
            done,
            stuck("a2", 7),
            row("a1", 8, Class::Running, Verdict::Fine),
        ]),
        7,
        11,
    );
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(
        e[0].kind,
        EventKind::Job {
            tab: 7,
            id: "a1".into(),
            from: Some(Verdict::Fine),
            to: Verdict::Exited,
            class: Class::Exited,
            task: Some("bq7f2k1za".into()),
            exit: Some(0),
        }
    );
    assert_eq!(e[0].kind.word(), "job");
}

#[test]
fn millicores_round_and_never_go_negative() {
    assert_eq!(millicores(1.2345), 1235);
    assert_eq!(millicores(-1.0), 0);
    assert_eq!(millicores(f32::NAN), 0);
}
