// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::model::{
    ClusterView, CoreView, CpuView, GpuView, MemCounters, MemRates, MemSample, MemView, Pressure,
    ScopeView, TabView, Thermal, Throttle,
};
use crate::trend::TrendView;

fn machine(s: &str) -> AwaitSys {
    match AwaitSys::parse(s).unwrap() {
        Parsed::Machine(w) => w,
        Parsed::Volume(_) => panic!("{s} routed to the volume"),
    }
}

fn snap(busy_per_core: f32) -> Snapshot {
    let cores: Vec<CoreView> = (0..10)
        .map(|_| CoreView {
            load: TrendView::flat(busy_per_core),
            measured: true,
        })
        .collect();
    Snapshot {
        at_ns: 5_000_000_000,
        seq: 3,
        cpu: Some(CpuView {
            busy: TrendView::flat(busy_per_core * 10.0),
            clusters: vec![
                ClusterView {
                    name: "Super".into(),
                    short: "S".into(),
                    cores: (0..4).collect(),
                },
                ClusterView {
                    name: "Efficiency".into(),
                    short: "E".into(),
                    cores: (4..10).collect(),
                },
            ],
            cores,
            history: Vec::new(),
            core_history: Vec::new(),
        }),
        gpu: Some(GpuView {
            load: TrendView::flat(0.02),
            ..GpuView::default()
        }),
        mem: Some(MemView {
            sample: MemSample {
                total: 16 << 30,
                pressure: Pressure::Normal,
                counters: Some(MemCounters::default()),
                ..MemSample::default()
            },
            rates: Some(MemRates::default()),
            ..MemView::default()
        }),
        global: ScopeView {
            wait: Some(0.05),
            ..ScopeView::default()
        },
        aterm: Some(ScopeView {
            cores: TrendView::flat(1.5),
            footprint: Some(800 << 20),
            ..ScopeView::default()
        }),
        tabs: vec![TabView {
            key: 7,
            scope: ScopeView {
                cores: TrendView::flat(0.12),
                footprint: Some(310_000_000),
                ..ScopeView::default()
            },
            ..TabView::default()
        }],
        procs_window_ns: Some(1_000_000_000),
        thermal: Some(Thermal::default()),
        ..Snapshot::default()
    }
}

#[test]
fn the_grammar_takes_every_metric_with_its_unit() {
    for ok in [
        "sys cpu<25%",
        "sys cpu<2.5c",
        "sys cpu.l0>50%",
        "sys cpu.super<=80%",
        "sys aterm<1c",
        "sys tab<0.5c for=10s",
        "sys wait<0.25",
        "sys gpu<10%",
        "sys mem=normal",
        "sys mem<critical",
        "sys mem=normal,warn",
        "sys mem!=critical and swap<1MB/s for=30s timeout=300000",
        "sys swap<1MiB",
        "sys decompress<100/s",
        "sys thrash=no",
        "sys tab.mem<2GB",
        "sys aterm.mem<4GiB",
        "sys thermal=nominal",
        "sys thermal<serious",
        "sys thermal<heavy",
        "sys quiet",
        "sys quiet for=20s timeout=600000",
        "sys quiet cpu<30%",
        "sys quiet and mem=normal",
        "sys timeout 5000 cpu<50%",
    ] {
        assert!(
            matches!(AwaitSys::parse(ok), Ok(Parsed::Machine(_))),
            "{ok}"
        );
    }
}

#[test]
fn refusals_name_what_is_wrong() {
    for bad in [
        "sys",
        "cpu<25%",
        "sys cpu<25",
        "sys cpu<25MB",
        "sys gpu<1c",
        "sys tab.mem<2",
        "sys swap<1",
        "sys mem<normal,warn",
        "sys mem=calm",
        "sys thrash<yes",
        "sys bogus<1",
        "sys cpu<25% gpu<10%",
        "sys cpu<25% and",
        "sys and cpu<25%",
        "sys cpu<25% for=10",
        "sys cpu<25% vol=/",
        "sys cpu<25% quiet",
        "sys cpu<25% scope=window",
        "sys cpu.<1%",
    ] {
        assert_eq!(AwaitSys::parse(bad), Err(USAGE.to_owned()), "{bad}");
    }
    assert_eq!(
        AwaitSys::parse("sys free>=10GB and cpu<5%"),
        Err(MIXED.to_owned())
    );
    assert_eq!(
        AwaitSys::parse("sys quiet free>=10GB"),
        Err(MIXED.to_owned())
    );
    for (t, metric) in [
        ("sys growth<1MB/s for=30s", "growth"),
        ("sys walk=complete", "walk"),
        ("sys eta>1h", "eta"),
    ] {
        assert_eq!(
            AwaitSys::parse(t),
            Err(format!("ERR unavailable {metric} why=not_sampled\n")),
            "{t}"
        );
    }
}

#[test]
fn job_terms_parse_as_verdicts_or_classes_never_both() {
    let w = machine("sys job=a40977 done,stuck timeout=600000");
    assert_eq!(w.terms[0].metric, Metric::Job(JobRef::Id("a40977".into())));
    assert_eq!(
        w.terms[0].want,
        Want::Verdicts(vec![Verdict::Exited, Verdict::Stuck])
    );
    let w = machine("sys task=b4u3h4kc3 class=waiting,exited and jobs.stuck=0");
    assert_eq!(
        w.terms[0].metric,
        Metric::Job(JobRef::Task("b4u3h4kc3".into()))
    );
    assert_eq!(
        w.terms[0].want,
        Want::Classes(vec![Class::Waiting, Class::Exited])
    );
    assert_eq!(w.terms[1].metric, Metric::Jobs(JobCount::Stuck));
    for bad in [
        "sys job=a1",
        "sys job=a1 bogus",
        "sys job=a1 class=stuck",
        "sys job=a1 done,class=running",
        "sys job>a1 done",
        "sys job=a/1 done",
        "sys job=a1 done jobs.stuck=0",
        "sys jobs.stuck>1c",
        "sys jobs.lost=0",
    ] {
        assert_eq!(AwaitSys::parse(bad), Err(USAGE.to_owned()), "{bad}");
    }
    assert_eq!(
        AwaitSys::parse("sys job=a1 running"),
        Err("ERR usage: running is a class; say class=running (verdicts go bare)\n".to_owned())
    );
    assert_eq!(
        AwaitSys::parse("sys job=a1 done,waiting"),
        Err("ERR usage: waiting is a class; say class=waiting (verdicts go bare)\n".to_owned())
    );
    assert!(!machine("sys jobs.stuck=0").scope_aterm);
    assert!(!machine("sys jobs.stuck=0 scope=tab").scope_aterm);
    assert!(machine("sys jobs.stuck=0 scope=aterm").scope_aterm);
    assert_eq!(
        AwaitSys::parse("sys jobs.stuck=0 scope=machine"),
        Err(USAGE.to_owned())
    );
}

#[test]
fn a_job_that_left_the_view_is_read_from_the_ring() {
    use crate::jobs::{Demand, Findings, JobsView};
    use crate::sys::jobs_view::fixtures::row;
    let mut s = snap(0.1);
    s.procs_window_ns = Some(2_000_000_000);
    s.jobs = Some(std::sync::Arc::new(JobsView {
        rows: vec![row("a44120", 7, Class::Running, Verdict::Fine)],
        findings: Findings::default(),
        demand: Demand::Fast,
        lanes_off: Vec::new(),
        parked_tabs: Vec::new(),
        at_ms: 0,
    }));
    let ended = [
        EndedJob {
            tab: 7,
            id: "a1".into(),
            task: Some("tz9k4m2qa".into()),
            exit: Some(3),
        },
        EndedJob {
            tab: 9,
            id: "a9".into(),
            task: None,
            exit: Some(0),
        },
    ];
    let o = evaluate_with(&machine("sys job=a1 done"), &s, &[7], &ended);
    assert!(o.holds, "{o:?}");
    assert_eq!(o.values, ["job=a1 verdict=exited class=exited exit=3"]);
    let o = evaluate_with(
        &machine("sys task=tz9k4m2qa class=exited"),
        &s,
        &[7],
        &ended,
    );
    assert!(o.holds, "{o:?}");
    let o = evaluate_with(&machine("sys job=a1 stuck"), &s, &[7], &ended);
    assert!(!o.holds);
    assert_eq!(o.refused.as_deref(), Some("ERR gone job=a1\n"));
    let o = evaluate_with(&machine("sys job=a9 done"), &s, &[7], &ended);
    assert!(!o.holds && o.refused.is_none(), "{o:?}");
    assert_eq!(o.absent, ["job=a9"]);
    let o = evaluate_with(&machine("sys job=a9 done scope=aterm"), &s, &[7, 9], &ended);
    assert!(o.holds, "{o:?}");
    let mut s2 = s.clone();
    s2.jobs = Some(std::sync::Arc::new(JobsView {
        rows: vec![row("a5", 7, Class::Exited, Verdict::Exited)],
        findings: Findings::default(),
        demand: Demand::Fast,
        lanes_off: Vec::new(),
        parked_tabs: Vec::new(),
        at_ms: 0,
    }));
    assert!(evaluate(&machine("sys job=a5 done"), &s2, &[7]).holds);
    assert_eq!(
        evaluate(&machine("sys job=a5 stuck"), &s2, &[7])
            .refused
            .as_deref(),
        Some("ERR gone job=a5\n")
    );
    let o = evaluate_with(&machine("sys job=a44120 class=running"), &s, &[7], &ended);
    assert!(o.holds && o.absent.is_empty(), "{o:?}");
}

#[test]
fn job_terms_read_the_engines_rows_of_the_target_tab_only() {
    use crate::jobs::{Demand, Findings, JobsView};
    use crate::sys::jobs_view::fixtures::{row, stuck};
    let mut s = snap(0.1);
    s.procs_window_ns = Some(2_000_000_000);
    s.jobs = Some(std::sync::Arc::new(JobsView {
        rows: vec![
            stuck("a40977", 7),
            row("a44120", 7, Class::Running, Verdict::Fine),
            stuck("a9", 9),
        ],
        findings: Findings::default(),
        demand: Demand::Fast,
        lanes_off: Vec::new(),
        parked_tabs: Vec::new(),
        at_ms: 0,
    }));
    let o = evaluate(&machine("sys jobs.stuck=1 and jobs.running>=1"), &s, &[7]);
    assert!(o.holds, "{o:?}");
    assert_eq!(o.values, ["jobs.stuck=1", "jobs.running=1"]);
    let o = evaluate(&machine("sys job=a40977 done,stuck"), &s, &[7]);
    assert!(o.holds, "{o:?}");
    assert_eq!(
        o.values,
        ["job=a40977 verdict=stuck class=waiting wait=tcp quiet_ms=1152000 phi=8.34"]
    );
    let o = evaluate(&machine("sys job=a40977 class=running"), &s, &[7]);
    assert!(!o.holds);
    let o = evaluate(&machine("sys job=a9 stuck"), &s, &[7]);
    assert!(!o.holds);
    assert_eq!(o.values, ["job=a9 verdict=warming"]);
    let o = evaluate(&machine("sys task=bq7f2k1za class=running"), &s, &[7]);
    assert!(
        !o.holds,
        "the first row by id carrying the task is the stuck one"
    );
    s.jobs = None;
    let o = evaluate(&machine("sys jobs.stuck=0"), &s, &[7]);
    assert_eq!(
        o.refused.as_deref(),
        Some("ERR unavailable jobs.stuck why=not_sampled\n")
    );
    let o = evaluate(&machine("sys job=a40977 done"), &s, &[7]);
    assert_eq!(
        o.refused.as_deref(),
        Some("ERR unavailable job why=not_sampled\n")
    );
    s.procs_window_ns = None;
    let o = evaluate(&machine("sys jobs.stuck=0"), &s, &[7]);
    assert!(o.refused.is_none() && !o.holds, "{o:?}");
}

/// The volume wait (`docs/DESIGN-harness-occupancy-2026-10-20.md` §3.1, run by `aterm-gui`'s
/// `control_disk`): a size needs its exact unit and is whole; `room` and the level words read;
/// one or two terms join by `and`; it takes `vol=` and `timeout=` but not `for=` or `scope=`,
/// and never mixes with machine terms.
#[test]
fn the_volume_wait_is_parsed_by_the_one_grammar() {
    let volume = |s: &str| match AwaitSys::parse(s) {
        Ok(Parsed::Volume(v)) => v,
        other => panic!("{s}: {other:?}"),
    };
    assert_eq!(
        volume("sys free>=5GiB"),
        VolumeWait {
            terms: vec![VolumeTerm::Free {
                strict: false,
                need: Need::Bytes(5 << 30),
            }],
            vol: None,
            timeout_ms: TIMEOUT_DEFAULT_MS,
        }
    );
    assert_eq!(
        volume("sys free>room and disk=ok,low vol=/Volumes/x timeout=600000"),
        VolumeWait {
            terms: vec![
                VolumeTerm::Free {
                    strict: true,
                    need: Need::Room,
                },
                VolumeTerm::Disk(vec![DiskWord::Ok, DiskWord::Low]),
            ],
            vol: Some("/Volumes/x".to_owned()),
            timeout_ms: 600_000,
        }
    );
    assert_eq!(
        volume("sys free>=10GB timeout=99999999999").timeout_ms,
        TIMEOUT_MAX_MS
    );
    assert_eq!(
        volume("sys disk=emergency,short timeout 5000"),
        VolumeWait {
            terms: vec![VolumeTerm::Disk(vec![DiskWord::Emergency, DiskWord::Short])],
            vol: None,
            timeout_ms: 5_000,
        }
    );
    for bad in [
        "sys free>=5000",
        "sys free>=5gb",
        "sys free>=1.5GiB",
        "sys free<room",
        "sys free<=room",
        "sys free<5GiB and free>=1GiB",
        "sys free>1GiB and free<=5GiB",
        "sys free=5GiB",
        "sys disk>=ok",
        "sys disk=ok,nope",
        "sys disk=",
        "sys free>=1GB and",
        "sys and free>=1GB",
        "sys free>=1GB disk=ok",
        "sys free>=1GB vol=",
        "sys free>=1GB vol=/a vol=/b",
        "sys free>=1GB timeout=x",
        "sys free>=1GB timeout=+5000",
        "sys free>=1GB timeout +5000",
        "sys free>=1KB and free>=2KB and free>=3KB",
        "sys free>=5GiB for=20m",
        "sys disk=ok scope=aterm",
        "sys mem_free>=1GiB",
        "sys cpu<50",
        "sys",
    ] {
        assert_eq!(AwaitSys::parse(bad), Err(USAGE.to_owned()), "{bad}");
    }
    for mixed in [
        "sys free>=5GiB and slots>=1",
        "sys quiet free>=1GB",
        "sys disk=ok and jobs.stuck=0",
    ] {
        assert_eq!(AwaitSys::parse(mixed), Err(MIXED.to_owned()), "{mixed}");
    }
    assert_eq!(AwaitSys::parse("sys cpu<50% vol=/x"), Err(USAGE.to_owned()));
}

#[test]
fn low_free_observers_keep_explicit_exact_bytes_and_volume() {
    for (operator, inclusive) in [("<", false), ("<=", true)] {
        for (size, bytes) in [
            ("0B", 0),
            ("68GiB", 68 << 30),
            ("18446744073709551615B", u64::MAX),
        ] {
            let text = format!("sys free{operator}{size} and disk=short vol=/data timeout=600000");
            assert_eq!(
                AwaitSys::parse(&text),
                Ok(Parsed::Volume(VolumeWait {
                    terms: vec![
                        VolumeTerm::FreeBelow { inclusive, bytes },
                        VolumeTerm::Disk(vec![DiskWord::Short])
                    ],
                    vol: Some("/data".into()),
                    timeout_ms: 600_000,
                }))
            );
        }
    }
    for text in [
        "sys free<room",
        "sys free<=room",
        "sys free<68",
        "sys free<=1.5GiB",
        "sys free<18446744073709551616B",
        "sys free<=18446744073709551615GiB",
        "sys free<=5GiB and free>1GiB",
        "sys free>=1GiB and free<5GiB",
    ] {
        assert_eq!(AwaitSys::parse(text), Err(USAGE.into()), "{text}");
    }
    assert_eq!(
        AwaitSys::parse("sys free<68GiB and cpu<50%"),
        Err(MIXED.into())
    );
}

/// Each wait reads back from its canonical spelling as itself (a fixed point), two spellings
/// of one wait spell it one way, and waits that differ spell apart.
#[test]
fn a_wait_reads_back_from_its_one_spelling() {
    for text in [
        "sys cpu<25%",
        "sys cpu<2.5c",
        "sys cpu.l0>50%",
        "sys cpu.L0>50%",
        "sys cpu.super<=80%",
        "sys aterm<1c and tab<0.5c for=10s",
        "sys wait<0.25",
        "sys mem=normal,warn",
        "sys mem<critical and thermal!=critical",
        "sys swap<1.5MB/s for=30s scope=aterm",
        "sys decompress<100/s and thrash=no",
        "sys tab.mem<2GB and aterm.mem<4GiB",
        "sys quiet",
        "sys quiet cpu<30% for=20s",
        "sys quiet and mem=normal",
        "sys job=a40977 done,stuck",
        "sys task=b4u3h4kc3 class=waiting,exited and jobs.stuck=0",
        "sys slots>=1 for=20m",
        "sys slots>=1 and avail>=8GiB and load<0.5 and cores_idle>=4c",
    ] {
        let wait = machine(text);
        let spelled = wait.canonical();
        assert_eq!(machine(&spelled), wait, "{text} spelled {spelled}");
        assert_eq!(machine(&spelled).canonical(), spelled, "{text}");
    }
    for (a, b) in [
        ("sys cpu.L0>50%", "sys cpu.l0>50%"),
        ("sys avail>=8GiB", "sys avail>=8589934592B"),
        ("sys cores_idle>=4c", "sys cores_idle>=4"),
        ("sys slots>=1 for=20m", "sys slots>=1 for=1200s"),
        ("sys job=a1 done", "sys job=a1 exited"),
    ] {
        assert_eq!(machine(a).canonical(), machine(b).canonical(), "{a} | {b}");
    }
    assert_eq!(
        machine("sys avail>=8GiB").canonical(),
        "sys avail>=8589934592B"
    );
    for (a, b) in [
        ("sys slots>=1", "sys slots>1"),
        ("sys slots>=1", "sys slots>=1 for=1s"),
        ("sys tab<1c", "sys tab<1c scope=aterm"),
        ("sys quiet", "sys quiet cpu<25%"),
    ] {
        assert_ne!(machine(a).canonical(), machine(b).canonical(), "{a} | {b}");
    }
}

#[test]
fn machine_numbers_stay_finite_through_parsing_and_unit_scaling() {
    let finite = format!("1{}", "0".repeat(300));
    let too_large = "9".repeat(400);
    for term in [
        format!("load>{too_large}"),
        format!("avail>={finite}TB"),
        format!("swap<{finite}TiB/s"),
    ] {
        assert_eq!(
            AwaitSys::parse(&format!("sys {term}")),
            Err(USAGE.to_owned())
        );
    }

    for text in [
        format!("sys load<{finite}"),
        "sys avail>=1.5TB".to_owned(),
        "sys swap<1.5MB/s".to_owned(),
    ] {
        let wait = machine(&text);
        let spelled = wait.canonical();
        assert_eq!(machine(&spelled), wait);
        assert_eq!(machine(&spelled).canonical(), spelled);
    }
}

#[test]
fn an_explicit_term_replaces_the_presets_and_timeouts_clamp() {
    let w = machine("sys quiet cpu<30% timeout=9999999");
    assert_eq!(w.timeout_ms, TIMEOUT_MAX_MS);
    let cpu: Vec<&Term> = w.terms.iter().filter(|t| t.metric == Metric::Cpu).collect();
    assert_eq!(cpu.len(), 1);
    assert!(!cpu[0].preset);
    assert_eq!(w.terms.len(), 5);
    assert_eq!(machine("sys tab<1c for=1.5m").for_ms, 90_000);
    assert_eq!(machine("sys tab<1c").timeout_ms, TIMEOUT_DEFAULT_MS);
}

#[test]
fn evaluation_reads_every_metric_and_prints_its_value() {
    let s = snap(0.1);
    let o = evaluate(
        &machine("sys cpu<25% and wait<0.25 and gpu<10% and mem=normal and tab<0.5c"),
        &s,
        &[7],
    );
    assert!(o.holds, "{o:?}");
    assert_eq!(
        o.values,
        ["cpu=10%", "wait=0.05", "gpu=2%", "mem=normal", "tab=0.12c"]
    );
    let o = evaluate(
        &machine("sys cpu.super>5% and cpu.l1>5% and cpu<0.5c"),
        &s,
        &[7],
    );
    assert_eq!(o.values, ["cpu.super=10%", "cpu.l1=10%", "cpu=1.00c"]);
    assert!(!o.holds, "1.0c is not below 0.5c");
    let o = evaluate(&machine("sys tab.mem<1GB and aterm.mem>500MB"), &s, &[7]);
    assert!(o.holds);
    assert_eq!(o.values, ["tab.mem=310MB", "aterm.mem=839MB"]);
    let o = evaluate(&machine("sys quiet"), &s, &[7]);
    assert!(o.holds, "{o:?}");
    assert_eq!(
        o.values,
        [
            "cpu=10%",
            "wait=0.05",
            "gpu=2%",
            "thermal=none",
            "swap=0B/s"
        ]
    );
    assert!(!evaluate(&machine("sys quiet"), &snap(0.5), &[7]).holds);
}

#[test]
fn an_unmeasurable_preset_term_drops_out_and_an_explicit_one_refuses() {
    let mut s = snap(0.1);
    s.gpu = None;
    s.thermal = None;
    let o = evaluate(&machine("sys quiet"), &s, &[7]);
    assert!(o.holds);
    assert_eq!(o.skipped, ["gpu:unsupported", "thermal:unsupported"]);
    assert_eq!(
        met_reply(&o, 20_007, 1_792_185_697_412, "2b11.310"),
        "OK sys cpu=10% wait=0.05 swap=0B/s skipped=gpu:unsupported,thermal:unsupported \
         held_ms=20007 as_of=21:21:37 next=2b11.310\n"
    );
    let o = evaluate(&machine("sys gpu<10%"), &s, &[7]);
    assert_eq!(
        o.refused.as_deref(),
        Some("ERR unavailable gpu why=unsupported\n")
    );
    assert!(!o.holds);
}

#[test]
fn a_warming_metric_parks_rather_than_refusing() {
    let mut s = snap(0.1);
    s.cpu = None;
    s.mem.as_mut().unwrap().rates = None;
    let o = evaluate(&machine("sys cpu<25% and swap<1MB/s"), &s, &[7]);
    assert!(!o.holds);
    assert!(o.refused.is_none());
    assert_eq!(o.values, ["cpu=warming", "swap=warming"]);
}

#[test]
fn ordered_levels_compare_by_rank() {
    let mut s = snap(0.1);
    s.mem.as_mut().unwrap().sample.pressure = Pressure::Warn;
    assert!(evaluate(&machine("sys mem<critical"), &s, &[]).holds);
    assert!(!evaluate(&machine("sys mem=normal"), &s, &[]).holds);
    assert!(evaluate(&machine("sys mem=normal,warn"), &s, &[]).holds);
    assert!(evaluate(&machine("sys mem!=critical"), &s, &[]).holds);
    assert!(evaluate(&machine("sys thrash=no"), &s, &[]).holds);
}

/// `thermal` ranks the level 0 to 3 and prints the OS's own word where the level is exact
/// and the counter heuristic's where not; either word set names the same rank, and the
/// retired `throttle` is refused. Control: the code before A2 S3 refused `thermal` and
/// answered `throttle=light` at Fair.
#[test]
fn the_thermal_metric_ranks_the_os_level() {
    let mut s = snap(0.1);
    let fair = Thermal {
        level: Throttle::Light,
        raw: 1,
        low_power: Some(false),
        exact: true,
    };
    s.thermal = Some(fair);
    let o = evaluate(&machine("sys thermal<serious"), &s, &[]);
    assert!(o.holds, "{o:?}");
    assert_eq!(o.values, ["thermal=fair"]);
    assert!(evaluate(&machine("sys thermal<heavy"), &s, &[]).holds);
    assert!(evaluate(&machine("sys thermal>nominal"), &s, &[]).holds);
    assert!(!evaluate(&machine("sys thermal=nominal"), &s, &[]).holds);
    assert!(!evaluate(&machine("sys thermal>=serious"), &s, &[]).holds);
    assert!(evaluate(&machine("sys thermal=fair,serious"), &s, &[]).holds);
    s.thermal = Some(Thermal {
        level: Throttle::Severe,
        raw: 3,
        ..fair
    });
    let o = evaluate(&machine("sys thermal>=serious"), &s, &[]);
    assert!(o.holds);
    assert_eq!(o.values, ["thermal=critical"]);
    s.thermal = Some(Thermal {
        level: Throttle::Light,
        raw: 1,
        low_power: None,
        exact: false,
    });
    let o = evaluate(&machine("sys thermal=fair"), &s, &[]);
    assert!(o.holds, "the heuristic's light is rank 1 too");
    assert_eq!(o.values, ["thermal=light"]);
    assert_eq!(
        machine("sys thermal<heavy").canonical(),
        "sys thermal<serious"
    );
    for retired in [
        "sys throttle=none",
        "sys throttle<heavy",
        "sys thermal=warm",
    ] {
        assert_eq!(AwaitSys::parse(retired), Err(USAGE.to_owned()), "{retired}");
    }
}

/// `quiet`'s thermal term waits out throttling, not fair: it holds at the OS's nominal and
/// fair and at the heuristic's none, and not at serious or above, nor at the heuristic's
/// light, which counts throttling. Its reading prints the snapshot's word, and an explicit
/// `thermal` term replaces it. Control: a preset that wants rank 0 (`thermal=nominal`) never
/// holds at fair, where this Mac sits all day.
#[test]
fn the_quiet_preset_waits_out_throttling_not_fair() {
    let mut s = snap(0.1);
    let at = |level, raw, exact| Thermal {
        level,
        raw,
        low_power: Some(false),
        exact,
    };
    for (thermal, met) in [
        (at(Throttle::None, 0, true), true),
        (at(Throttle::Light, 1, true), true),
        (at(Throttle::Heavy, 2, true), false),
        (at(Throttle::Severe, 3, true), false),
        (at(Throttle::None, 0, false), true),
        (at(Throttle::Light, 1, false), false),
    ] {
        s.thermal = Some(thermal);
        let o = evaluate(&machine("sys quiet"), &s, &[7]);
        assert_eq!(o.holds, met, "{thermal:?}: {o:?}");
        assert!(o.skipped.is_empty(), "{o:?}");
    }
    s.thermal = Some(at(Throttle::Light, 1, true));
    let o = evaluate(&machine("sys quiet"), &s, &[7]);
    assert_eq!(
        o.values,
        [
            "cpu=10%",
            "wait=0.05",
            "gpu=2%",
            "thermal=fair",
            "swap=0B/s"
        ]
    );
    assert!(!evaluate(&machine("sys quiet thermal=nominal"), &s, &[7]).holds);
    s.thermal = Some(at(Throttle::Heavy, 2, true));
    assert!(evaluate(&machine("sys quiet thermal<critical"), &s, &[7]).holds);
    assert_eq!(machine("sys quiet thermal<critical").terms.len(), 5);
}

#[test]
fn a_tab_the_snapshot_does_not_hold_yet_is_warming() {
    let o = evaluate(&machine("sys tab<1c"), &snap(0.1), &[99]);
    assert!(o.refused.is_none());
    assert!(!o.holds);
    assert_eq!(o.values, ["tab=warming"]);
    let mut s = snap(0.1);
    s.tabs[0].scope.footprint = None;
    let o = evaluate(&machine("sys tab.mem<1GB"), &s, &[7]);
    assert_eq!(
        o.refused.as_deref(),
        Some("ERR unavailable tab.mem why=refused\n")
    );
}

#[test]
fn scheduling_unmeasured_cpu_keeps_quiet_pending_without_skipping_cpu() {
    let mut s = snap(0.0);
    assert!(evaluate(&machine("sys quiet"), &s, &[7]).holds);
    s.cpu.as_mut().unwrap().cores[0].measured = false;
    for text in ["sys quiet", "sys cpu<25%", "sys cpu.l0<25%"] {
        let result = evaluate(&machine(text), &s, &[7]);
        assert!(!result.holds, "{text}: {result:?}");
        assert!(result.refused.is_none(), "{text}: {result:?}");
        assert!(!result.skipped.iter().any(|s| s.starts_with("cpu")));
    }
    assert!(evaluate(&machine("sys cpu.l1<25%"), &s, &[7]).holds);
}

#[test]
fn for_holds_over_consecutive_snapshots_by_their_stamps() {
    let mut l = Latch::default();
    let s = 1_000_000_000u64;
    assert_eq!(l.step(true, s, 2_000), None);
    assert_eq!(l.step(true, s, 2_000), None, "the same snapshot again");
    assert_eq!(l.step(true, 2 * s, 2_000), None);
    assert_eq!(l.step(true, 3 * s, 2_000), Some(2_000));
    let mut l = Latch::default();
    assert_eq!(l.step(true, s, 2_000), None);
    assert_eq!(
        l.step(false, 2 * s, 2_000),
        None,
        "a break restarts the hold"
    );
    assert_eq!(l.step(true, 3 * s, 2_000), None);
    assert_eq!(l.step(true, 5 * s, 2_000), Some(2_000));
    assert_eq!(
        l.step(false, 5 * s, 2_000),
        None,
        "a falsified re-evaluation at the same stamp clears the hold"
    );
    assert_eq!(l.step(true, 5 * s, 2_000), None);
    assert_eq!(l.step(true, 7 * s, 2_000), Some(2_000));
    assert_eq!(Latch::default().step(true, s, 0), Some(0));
}

#[test]
fn sizes_print_compactly() {
    assert_eq!(size_word(0.0), "0B");
    assert_eq!(size_word(1_234.0), "1.2KB");
    assert_eq!(size_word(310e6), "310MB");
    assert_eq!(size_word(7.1e6), "7.1MB");
}

/// A snapshot of `cores` logical cores, these load averages, `avail`
/// bytes and these slots.
fn occupancy(
    cores: usize,
    load: Option<[f64; 3]>,
    avail: Option<u64>,
    slots: Option<SlotsView>,
) -> Snapshot {
    Snapshot {
        at_ns: 5_000_000_000,
        cpu: Some(CpuView {
            cores: vec![
                CoreView {
                    measured: true,
                    ..Default::default()
                };
                cores
            ],
            ..CpuView::default()
        }),
        load,
        mem: avail.map(|available| MemView {
            sample: MemSample {
                available,
                ..MemSample::default()
            },
            ..MemView::default()
        }),
        slots,
        ..Snapshot::default()
    }
}

fn read_slots(n: u16, held: u16, free: u16, pending: u16, discounted: u16) -> SlotsView {
    SlotsView::Read {
        n,
        held,
        free,
        pending,
        unknown: n - held - free - pending,
        discounted,
    }
}

#[test]
fn the_occupancy_terms_parse_with_their_units() {
    let num = |op, v| Want::Num {
        op,
        v,
        cores: false,
    };
    for (text, metric, want) in [
        ("sys load<0.5", Metric::Load, num(Op::Lt, 0.5)),
        ("sys cores_idle>=4", Metric::CoresIdle, num(Op::Ge, 4.0)),
        ("sys cores_idle>=4c", Metric::CoresIdle, num(Op::Ge, 4.0)),
        (
            "sys avail>=8GiB",
            Metric::Avail,
            num(Op::Ge, 8_589_934_592.0),
        ),
        ("sys avail>8GB", Metric::Avail, num(Op::Gt, 8e9)),
        ("sys slots>=1", Metric::Slots, num(Op::Ge, 1.0)),
        ("sys cpu<50%", Metric::Cpu, num(Op::Lt, 50.0)),
    ] {
        let w = machine(text);
        assert_eq!(w.terms.len(), 1, "{text}");
        assert_eq!(w.terms[0].metric, metric, "{text}");
        assert_eq!(w.terms[0].want, want, "{text}");
    }
    assert_eq!(machine("sys slots>=1 for=20m").for_ms, 1_200_000);
    let w = machine("sys slots>=1 and avail>=8GiB and load<0.5 and mem<critical");
    assert_eq!(
        w.terms.iter().map(|t| t.metric.name()).collect::<Vec<_>>(),
        ["slots", "avail", "load", "mem"]
    );
    for bad in [
        "sys avail>=8",
        "sys avail>=8%",
        "sys slots>=1c",
        "sys slots>=1GB",
        "sys load<50%",
        "sys load<0.5c",
        "sys cores_idle>=4%",
        "sys cpu<50",
        "sys slots=free",
    ] {
        assert_eq!(AwaitSys::parse(bad), Err(USAGE.to_owned()), "{bad}");
    }
    assert_eq!(
        AwaitSys::parse("sys free>=5GiB and slots>=1"),
        Err(MIXED.to_owned())
    );
}

#[test]
fn the_occupancy_terms_read_load_cores_memory_and_slots() {
    let s = occupancy(
        18,
        Some([2.0, 4.0, 1.0]),
        Some(96 << 30),
        Some(read_slots(3, 2, 1, 0, 0)),
    );
    let o = evaluate(
        &machine("sys load<0.5 and cores_idle>=14 and avail>=8GiB and slots>=1"),
        &s,
        &[],
    );
    assert!(o.holds, "{o:?}");
    assert_eq!(
        o.values,
        ["load=0.11", "cores_idle=14.00c", "avail=103GB", "slots=1/3"]
    );
    for (text, value) in [
        ("sys load<0.1", "load=0.11"),
        ("sys cores_idle>14", "cores_idle=14.00c"),
        ("sys avail>=100GiB", "avail=103GB"),
        ("sys slots>=2", "slots=1/3"),
    ] {
        let o = evaluate(&machine(text), &s, &[]);
        assert!(!o.holds && o.refused.is_none(), "{text}: {o:?}");
        assert_eq!(o.values, [value], "{text}");
    }
    let busy = occupancy(4, Some([30.0, 30.0, 30.0]), None, None);
    assert_eq!(
        evaluate(&machine("sys cores_idle<1"), &busy, &[]).values,
        ["cores_idle=0.00c"]
    );
    let s = occupancy(18, None, None, Some(read_slots(3, 2, 1, 0, 1)));
    let o = evaluate(&machine("sys slots>=1"), &s, &[]);
    assert!(!o.holds && o.refused.is_none(), "{o:?}");
    assert_eq!(o.values, ["slots=0/3"]);
    let s = occupancy(18, None, None, Some(read_slots(3, 0, 1, 2, 0)));
    assert!(evaluate(&machine("sys slots>=1"), &s, &[]).holds);
}

#[test]
fn an_occupancy_term_the_host_cannot_read_refuses_and_a_first_read_warms() {
    let refused = |s: &Snapshot, text: &str| evaluate(&machine(text), s, &[]).refused;
    for (slots, why) in [
        (
            Some(SlotsView::Unread(SlotsUnread::UnreadFormat)),
            "unread_format",
        ),
        (Some(SlotsView::Unread(SlotsUnread::Refused)), "refused"),
        (
            Some(SlotsView::Unread(SlotsUnread::Unsupported)),
            "unsupported",
        ),
        (Some(read_slots(0, 0, 0, 0, 0)), "not_sampled"),
        (None, "not_sampled"),
    ] {
        let s = occupancy(18, Some([1.0; 3]), Some(1 << 30), slots);
        assert_eq!(
            refused(&s, "sys slots>=1").as_deref(),
            Some(format!("ERR unavailable slots why={why}\n").as_str()),
            "{slots:?}"
        );
    }
    let s = occupancy(18, None, None, None);
    assert_eq!(
        refused(&s, "sys load<0.5").as_deref(),
        Some("ERR unavailable load why=unsupported\n")
    );
    assert_eq!(
        refused(&s, "sys cores_idle>=4").as_deref(),
        Some("ERR unavailable cores_idle why=unsupported\n")
    );
    assert_eq!(
        refused(&s, "sys avail>=8GiB").as_deref(),
        Some("ERR unavailable avail why=unsupported\n")
    );
    let mut s = occupancy(18, Some([1.0; 3]), None, Some(read_slots(3, 0, 0, 3, 0)));
    s.cpu = None;
    let o = evaluate(
        &machine("sys slots>=1 and load<0.5 and cores_idle>=1"),
        &s,
        &[],
    );
    assert!(!o.holds && o.refused.is_none(), "{o:?}");
    assert_eq!(
        o.values,
        ["slots=warming", "load=warming", "cores_idle=warming"]
    );
}

/// A slots term holds only when it holds whichever way each unproven slot turns out.
#[test]
fn a_slots_term_holds_only_on_slots_proven_either_way() {
    let on = |slots: SlotsView, text: &str| {
        evaluate(&machine(text), &occupancy(18, None, None, Some(slots)), &[]).holds
    };
    let unknown = read_slots(1, 0, 0, 0, 0);
    let unproven = read_slots(3, 0, 0, 2, 0);
    for text in ["sys slots<1", "sys slots=0", "sys slots<=0", "sys slots!=1"] {
        assert!(!on(unknown, text), "{text} on one unknown slot");
        assert!(!on(unproven, text), "{text} on three unproven slots");
    }
    assert!(!on(unknown, "sys slots>=1"));
    assert!(on(unknown, "sys slots<2"), "even free it is one");
    assert!(on(unproven, "sys slots!=4"), "no count reaches four");
    let one_or_two = read_slots(3, 1, 1, 1, 0);
    assert!(on(one_or_two, "sys slots>=1"));
    assert!(!on(one_or_two, "sys slots=1"));
    assert!(on(one_or_two, "sys slots<3"));
    let none_free = read_slots(1, 1, 0, 0, 0);
    for text in ["sys slots<1", "sys slots=0", "sys slots<=0", "sys slots!=1"] {
        assert!(on(none_free, text), "{text} on one held slot");
    }
    assert!(on(read_slots(3, 2, 1, 0, 0), "sys slots=1"));
}

/// Past [`SLOTS_FRESH_MS`] a slots reading is warming, an unread one included.
#[test]
fn a_slots_reading_older_than_the_armed_period_is_warming() {
    let at = |slots: SlotsView, age_ms: Option<u64>| {
        let mut s = occupancy(18, None, None, Some(slots));
        s.slots_age_ms = age_ms;
        s
    };
    let free = read_slots(3, 2, 1, 0, 0);
    let w = machine("sys slots>=1");
    for age in [None, Some(0), Some(SLOTS_FRESH_MS)] {
        assert!(evaluate(&w, &at(free, age), &[]).holds, "{age:?}");
    }
    let o = evaluate(&w, &at(free, Some(SLOTS_FRESH_MS + 1)), &[]);
    assert!(!o.holds && o.refused.is_none(), "{o:?}");
    assert_eq!(o.values, ["slots=warming"]);
    let unread = SlotsView::Unread(SlotsUnread::NotSampled);
    let o = evaluate(&w, &at(unread, Some(60_000)), &[]);
    assert!(
        o.refused.is_none() && o.values == ["slots=warming"],
        "{o:?}"
    );
    let o = evaluate(&w, &at(unread, Some(0)), &[]);
    assert_eq!(
        o.refused.as_deref(),
        Some("ERR unavailable slots why=not_sampled\n")
    );
}

#[test]
fn a_slots_wait_for_twenty_minutes_holds_only_across_stamps_that_span_them() {
    let w = machine("sys slots>=1 for=20m");
    let min = 60_000_000_000u64;
    let at = |t: u64, free: u16| {
        let mut s = occupancy(18, None, None, Some(read_slots(1, 1 - free, free, 0, 0)));
        s.at_ns = t;
        s
    };
    let mut l = Latch::default();
    let mut feed = |s: Snapshot| l.step(evaluate(&w, &s, &[]).holds, s.at_ns, w.for_ms);
    assert_eq!(feed(at(0, 1)), None);
    assert_eq!(feed(at(10 * min, 1)), None);
    assert_eq!(feed(at(15 * min, 0)), None, "a held read restarts the hold");
    assert_eq!(feed(at(20 * min, 1)), None);
    assert_eq!(feed(at(39 * min, 1)), None);
    assert_eq!(feed(at(40 * min, 1)), Some(1_200_000));
}
