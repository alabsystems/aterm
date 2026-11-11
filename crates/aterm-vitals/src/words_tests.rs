// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::model::{CpuView, GpuView, MemSample, MemView, ScopeView, TabView};
use crate::trend::TrendView;

fn scenario_a() -> Snapshot {
    Snapshot {
        cpu: Some(CpuView {
            cores: vec![
                crate::model::CoreView {
                    measured: true,
                    ..Default::default()
                };
                10
            ],
            busy: TrendView::flat(3.1),
            ..CpuView::default()
        }),
        gpu: Some(GpuView {
            load: TrendView::flat(0.04),
            ..GpuView::default()
        }),
        mem: Some(MemView {
            sample: MemSample {
                total: 32 << 30,
                app: 12 << 30,
                wired: (4 << 30) + (700 << 20),
                compressed: Some(1 << 30),
                pressure: Pressure::Normal,
                ..MemSample::default()
            },
            ..MemView::default()
        }),
        volume: Some(VolumeSample {
            total: 2_000_000_000_000,
            free: 1_810_000_000_000,
            reserved: 0,
            mount: "/".into(),
        }),
        aterm: Some(ScopeView {
            cores: TrendView::flat(1.6),
            footprint: Some((2 << 30) + (300 << 20)),
            procs: 17,
            ..ScopeView::default()
        }),
        ..Snapshot::default()
    }
}

#[test]
fn units_follow_the_platform_conventions() {
    assert_eq!(mem(310 << 20), "310 MiB");
    assert_eq!(mem(2_469_606_195), "2.3 GiB");
    assert_eq!(mem(16 << 10), "16 KiB");
    assert_eq!(mem(0), "0 KiB");
    assert_eq!(mem(1), "<1 KiB", "nonzero is never 0");
    assert_eq!(mem(512), "<1 KiB");
    assert_eq!(mem(1 << 10), "1 KiB");
    assert_eq!(gib_figure(0), "0.0");
    assert_eq!(gib_figure(18_202_624), "<0.1", "17 MiB is never 0.0 GiB");
    assert_eq!(gib_figure(3 << 29), "1.5");
    assert_eq!(disk(1_810_000_000_000), "1.81 TB");
    assert_eq!(disk(412_000_000_000), "412 GB");
    assert_eq!(disk(9_400_000_000), "9.4 GB");
    assert_eq!(gib_total(32 << 30), "32");
    assert_eq!(cores(-0.2), "0.0");
    assert_eq!(cores(f32::INFINITY), "0.0");
    assert_eq!(pct(Some(f32::INFINITY)), "–");
    assert_eq!(tokens_k(f64::INFINITY), "0");
    assert_eq!(duration_approx(f64::INFINITY), "~1 min");
    assert_eq!(rate_mb(f64::INFINITY), "0.0 MB/s");
}

use crate::explain::{ActivityId, CoalLane, DeckCause, WhyFacts};
use crate::history::Rates;
use crate::jobs::{Class, Consumer, Findings, JobKind, JobRow, JobsView, Tick, ZombieParent};
use crate::model::{
    Busiest, DiskFacts, GpuSample, Hog, LinkSample, PlanFact, Series, Thermal, Throttle,
};
use crate::ring::Cell;
use std::sync::Arc;

const NOW: i64 = 1_791_974_400_000;
const MIN_MS: i64 = 60_000;
const MAC: Locale = Locale {
    platform: Platform::MacOs,
    utc_offset_s: 0,
    utc_offset_at: None,
};
const LINUX: Locale = Locale {
    platform: Platform::Linux,
    utc_offset_s: 0,
    utc_offset_at: None,
};

fn row(now: f64, capacity: f64) -> ResourceView {
    ResourceView {
        now: Some(now),
        capacity: Some(capacity),
        heat_now: Some((now / capacity) as f32),
        ..ResourceView::default()
    }
}

fn steady(mut view: ResourceView, over: usize) -> ResourceView {
    let v = view.now;
    view.ring = (0..60)
        .map(|i| Cell {
            mean: v,
            max: v,
            tau_ms: 10_000.0,
            over_ms: if i >= 60 - over { 10_000.0 } else { 0.0 },
        })
        .collect::<Vec<_>>()
        .into();
    view.ring_k = NOW.div_euclid(10_000);
    view
}

fn series(now: f64, capacity: f64, binds: bool) -> Series {
    Series {
        now: Some(now),
        capacity: Some(capacity),
        ring: Arc::from(Vec::new()),
        binds,
        ..Series::default()
    }
}

fn fig(now: f64) -> ScopeFig {
    ScopeFig {
        now: Some(now),
        ..ScopeFig::default()
    }
}

fn missing(why: NoFigure) -> ScopeFig {
    ScopeFig {
        missing: Some(why),
        ..ScopeFig::default()
    }
}

fn snap() -> Snapshot {
    let tab = |key: u64| TabView {
        key,
        label: format!("Tab {key}"),
        ..TabView::default()
    };
    Snapshot {
        seq: 1,
        at_unix_ms: NOW,
        tabs: vec![tab(2), tab(3)],
        ..Snapshot::default()
    }
}

fn set(s: &mut Snapshot, res: Resource, view: ResourceView) {
    s.resources[res.index()] = view;
}

fn spotlight(context: Option<Context>) -> Option<Arc<WhyFacts>> {
    Some(Arc::new(WhyFacts {
        lane: CoalLane::On,
        clause: Some(DeckCause {
            activity: ActivityId::Spotlight,
            word: "Spotlight",
            context,
        }),
        ..WhyFacts::default()
    }))
}

fn watch(level: DiskLevel) -> DiskFacts {
    DiskFacts {
        level: Some(level),
        warn: Some(69_000_000_000),
        floor: Some(34_000_000_000),
        emergency: Some(8_600_000_000),
        watch: true,
        ..DiskFacts::default()
    }
}

fn disk_trouble() -> Snapshot {
    let mut s = snap();
    let total = 2_000_000_000_000u64;
    let available = 44_800_000_000u64;
    let mut view = steady(row((total - available) as f64, total as f64), 0);
    view.state = Some(State::Critical);
    view.aterm = fig(38.4e6);
    view.tabs = vec![(2, fig(0.2e6))];
    set(&mut s, Resource::Disk, view);
    s.volume = Some(VolumeSample {
        total,
        free: available,
        reserved: 0,
        mount: "/".into(),
    });
    s.facts.filling = Some(Filling {
        secs: 34.0 * 60.0,
        rate: 20e6,
        burst: true,
    });
    s.facts.disk_rates = Rates {
        rate10: Some(2.6e9 / 600.0),
        ..Rates::default()
    };
    s.facts.disk = Some(DiskFacts {
        stuck: 2,
        freed_today: Some((41_200_000_000, NOW - 90 * MIN_MS, "build caches".into())),
        hogs: vec![Hog {
            path: "~/aterm/target".into(),
            bytes: 212_000_000_000,
            partial: false,
            remedy: String::new(),
        }],
        ..watch(DiskLevel::Low)
    });
    s
}

fn job(id: &str, verdict: Verdict) -> JobRow {
    JobRow {
        id: id.into(),
        tab: 2,
        kind: JobKind::Shell,
        orphan_of: None,
        consumer: Consumer::default(),
        promoted_ms: None,
        task: None,
        pgid: 300,
        leader: (300, None),
        exe: "make".into(),
        members: 1,
        class: Class::Running,
        wait: None,
        named: None,
        stop: None,
        input: None,
        cpu_mc: None,
        zombies: None,
        standing: None,
        quiet_ms: None,
        phi_milli: None,
        mu_ms: None,
        mu_from: None,
        verdict,
        no_progress: false,
        working_descendant: None,
        episode: 0,
        exit: None,
        tick: Tick::default(),
    }
}

fn processes_trouble() -> Snapshot {
    let mut s = snap();
    set(&mut s, Resource::Processes, row(897.0, 16_000.0));
    s.facts.room = TaskRoom {
        procs: Some(897),
        procs_max: Some(16_000),
        user_procs: Some(578),
        user_procs_max: Some(10_666),
        ..TaskRoom::default()
    };
    s.set_jobs(Some(Arc::new(JobsView {
        rows: vec![
            job("j1", Verdict::Stuck),
            job("j2", Verdict::Quiet),
            job("j3", Verdict::Quiet),
        ],
        findings: Findings {
            zombies: vec![ZombieParent {
                ppid: 9,
                name: "node".into(),
                count: 1,
                per_min_deci: 0,
                tab: Some(2),
            }],
            ..Findings::default()
        },
        ..JobsView::default()
    })));
    s
}

#[test]
fn job_summaries_qualify_local_stalls_without_hiding_independent_ones() {
    for verdict in [Verdict::Quiet, Verdict::Stuck] {
        let mut wrapper = job("wrapper", verdict);
        wrapper.class = crate::jobs::Class::Polling;
        let mut s = snap();
        for descendant in [None, Some((84682, 1_793_674_154_348_639)), None] {
            wrapper.working_descendant = descendant;
            let view = JobsView {
                rows: vec![wrapper.clone(), job("independent", Verdict::Stuck)],
                ..JobsView::default()
            };
            s.set_jobs(Some(Arc::new(view.clone())));
            let words = job_marks(&s)
                .iter()
                .map(|m| m.words.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            assert_eq!(
                words.contains("child activity"),
                descendant.is_some(),
                "{words}"
            );
            assert_eq!(words.contains("local"), descendant.is_some(), "{words}");
            assert!(
                words.contains("1 stuck") || words.contains("2 stuck"),
                "{words}"
            );
            assert_eq!(s.jobs.as_deref(), Some(&view));
            assert_eq!(
                s.resources[Resource::Processes.index()].state,
                Some(State::Stuck)
            );
            let compact = state(Resource::Processes, &s, &MAC).expect("stuck state");
            assert_eq!(
                compact.clauses.join("; ").contains("child activity"),
                descendant.is_some()
            );
            let details = processes_details(&s);
            assert_eq!(
                details
                    .iter()
                    .flat_map(|d| &d.forms)
                    .any(|w| w.contains("local")),
                descendant.is_some()
            );
        }
    }
}

fn ladder_rows(loc: &Locale) -> Vec<(Resource, ResourceView)> {
    let mut net = row(2.1e6, 11.8e6);
    net.basis = CapBasis::Peak;
    net.second = Some(series(0.3e6, 4.2e6, false));
    let mut tokens = row(3_100.0, 9_000.0);
    tokens.basis = CapBasis::Peak;
    let procs = if loc.platform == Platform::Linux {
        row(3_412.0, 126_592.0)
    } else {
        let mut v = row(897.0, 16_000.0);
        v.second = Some(series(578.0, 10_666.0, false));
        v
    };
    vec![
        (Resource::Cpu, row(17.7, 18.0)),
        (Resource::Gpu, row(12.0, 100.0)),
        (Resource::Memory, row(42.1 * GIB, 128.0 * GIB)),
        (Resource::Disk, row(1.11e12, 2e12)),
        (Resource::Network, net),
        (Resource::Tokens, tokens),
        (Resource::Processes, procs),
    ]
}

fn freed_facts() -> RowFacts {
    RowFacts {
        disk: Some(DiskFacts {
            freed_today: Some((41_200_000_000, NOW, "build caches".into())),
            ..watch(DiskLevel::Ok)
        }),
        ..RowFacts::default()
    }
}

fn strings(forms: &[&str]) -> Vec<String> {
    forms.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn amount_ladders() {
    use Ladder::{Cols, Det, NoCols};
    let golden = |res: Resource, loc: &Locale| -> [Vec<&'static str>; 3] {
        match (res, loc.platform) {
            (Resource::Cpu, _) => [
                vec!["17.7 of 18 cores"],
                vec!["17.7 of 18 cores", "17.7/18 cores"],
                vec!["17.7/18 cores"],
            ],
            (Resource::Gpu, _) => [
                vec!["busy 12% of the time"],
                vec!["busy 12% of the time", "busy 12% of time", "busy 12%"],
                vec!["busy 12% of time", "busy 12%"],
            ],
            (Resource::Memory, _) => [
                vec!["42.1 of 128 GiB in use"],
                vec!["42.1 of 128 GiB in use", "42.1 of 128 GiB", "42.1/128 GiB"],
                vec!["42.1/128 GiB"],
            ],
            (Resource::Disk, _) => [
                vec!["1.11 of 2.00 TB used"],
                vec![
                    "1.11 of 2.00 TB used · freed 41 GB",
                    "1.11/2.00 TB used · freed 41 GB",
                    "1.11 TB used · freed 41 GB",
                    "1.11 of 2.00 TB used",
                    "1.11/2.00 TB used",
                ],
                vec![
                    "1.11/2.00 TB used · freed 41 GB",
                    "1.11 TB used · freed 41 GB",
                    "1.11/2.00 TB used",
                ],
            ],
            (Resource::Network, _) => [
                vec!["2.1 of 11.8 MB/s peak"],
                vec!["2.1 of 11.8 MB/s peak", "2.1/11.8 MB/s peak"],
                vec!["2.1/11.8 MB/s peak"],
            ],
            (Resource::Tokens, _) => [
                vec!["aterm only, peak 9.0k/min", "aterm only, peak 9.0k"],
                vec![
                    "aterm only, peak 9.0k/min",
                    "aterm only, peak 9.0k",
                    "peak 9.0k/min",
                ],
                vec![
                    "aterm 3.1k/min, peak 9.0k",
                    "aterm 3.1k/9.0k peak",
                    "3.1k/9.0k peak",
                ],
            ],
            (Resource::Processes, Platform::Linux) => [
                vec!["3,412 of 126,592 threads"],
                vec![
                    "3,412 of 126,592 threads",
                    "3,412/126,592 threads",
                    "3.4k/127k threads",
                ],
                vec!["3,412/126,592 threads", "3.4k/127k threads"],
            ],
            (Resource::Processes, _) => [
                vec!["897 of 16,000 processes"],
                vec!["897 of 16,000 processes", "897 of 16,000", "897/16,000"],
                vec!["897/16,000"],
            ],
        }
    };
    let names_unit = |res: Resource, form: &str, loc: &Locale| match res {
        Resource::Cpu => form.contains("cores"),
        Resource::Gpu => form.contains("busy") && form.contains('%'),
        Resource::Memory => form.contains("GiB"),
        Resource::Disk => form.contains(" used"),
        Resource::Network => {
            form.contains("MB/s") && (form.contains("peak") || form.contains("link"))
        }
        Resource::Tokens => form.contains("peak") || form.contains("/min"),
        Resource::Processes => loc.platform != Platform::Linux || form.ends_with(" threads"),
    };
    for loc in [MAC, LINUX] {
        let facts = freed_facts();
        for (res, view) in ladder_rows(&loc) {
            let want = golden(res, &loc);
            for (ladder, want) in [Det, Cols, NoCols].into_iter().zip(want) {
                let got = amount_forms(res, &view, &facts, ladder, &loc);
                assert_eq!(got, strings(&want), "{res:?} {ladder:?} {loc:?}");
                for form in &got {
                    assert!(names_unit(res, form, &loc), "{res:?}: {form}");
                }
            }
            let blank = ResourceView::default();
            for ladder in [Det, Cols, NoCols] {
                assert!(amount_forms(res, &blank, &facts, ladder, &loc).is_empty());
            }
        }
    }

    let disk = row(1.11e12, 2e12);
    let calm = RowFacts::default();
    assert_eq!(
        amount_forms(Resource::Disk, &disk, &calm, Cols, &MAC),
        strings(&["1.11 of 2.00 TB used", "1.11/2.00 TB used"])
    );
    let mut off = freed_facts();
    off.disk.as_mut().expect("facts").watch = false;
    assert_eq!(
        amount_forms(Resource::Disk, &disk, &off, NoCols, &MAC),
        strings(&["1.11/2.00 TB used"])
    );
    assert_eq!(
        amount_forms(Resource::Disk, &row(512e9, 994e9), &calm, Det, &MAC),
        strings(&["512 of 994 GB used"])
    );

    let mut link = row(2.1e6, 22.9e6);
    link.basis = CapBasis::Link;
    link.second = Some(series(0.3e6, 22.9e6, false));
    assert_eq!(
        amount_forms(Resource::Network, &link, &calm, Cols, &MAC),
        strings(&["2.1 of 22.9 MB/s link", "2.1/22.9 MB/s link"])
    );
    let mut up = row(0.1e6, 11.8e6);
    up.basis = CapBasis::Peak;
    up.second = Some(series(3.0e6, 4.2e6, true));
    assert_eq!(
        amount_forms(Resource::Network, &up, &calm, Det, &MAC),
        strings(&["3.0 of 4.2 MB/s peak"])
    );

    let mut learning = row(3_100.0, 1.0);
    learning.capacity = None;
    learning.basis = CapBasis::Learning;
    for ladder in [Det, Cols] {
        assert_eq!(
            amount_forms(Resource::Tokens, &learning, &calm, ladder, &MAC),
            strings(&[
                "aterm only · learning its peak",
                "aterm only, learning peak",
                "learning peak",
                "learning"
            ])
        );
    }
    assert_eq!(
        amount_forms(Resource::Tokens, &learning, &calm, NoCols, &MAC),
        strings(&["aterm 3.1k/min, learning", "3.1k/min, learning", "3.1k/min"])
    );
    learning.now = None;
    assert_eq!(
        amount_forms(Resource::Tokens, &learning, &calm, NoCols, &MAC),
        strings(&[
            "aterm only · learning its peak",
            "aterm only, learning peak",
            "learning peak",
            "learning"
        ])
    );

    // The user's limit is no run's: the count is against the machine's limit.
    let counts = row(900.0, 16_000.0);
    assert_eq!(
        amount_forms(Resource::Processes, &counts, &calm, Cols, &MAC),
        strings(&["900 of 16,000 processes", "900 of 16,000", "900/16,000"])
    );
}

/// Record §8.1: where a start counter reads, the Processes run says starts a second and how
/// many run (`37 starts/s · 742 running`, then `37/s`), aterm's cell its own starts, the
/// spoken sentence the same against "its 7-day peak", and no form names a limit, the
/// machine's or the user's (those are the lens's Room's, and Details'). CONTROL: where no
/// start counter reads, the count's run names the machine's limit.
#[test]
fn the_processes_run_says_starts_a_second_and_never_a_limit() {
    use Ladder::{Cols, Det, NoCols};
    let mut s = snap();
    let mut view = row(37.2, 200.0);
    view.basis = CapBasis::Peak;
    view.aterm = fig(3.4);
    set(&mut s, Resource::Processes, view.clone());
    s.facts.room = TaskRoom {
        procs: Some(742),
        procs_max: Some(8_000),
        user_procs: Some(435),
        user_procs_max: Some(5_333),
        ..TaskRoom::default()
    };
    let run = glance_run(Resource::Processes, &view, &s, &MAC);
    assert_eq!(run, strings(&["37 starts/s \u{b7} 742 running", "37/s"]));
    let ladder = |l| amount_forms(Resource::Processes, &view, &s.facts, l, &MAC);
    assert_eq!(ladder(Det), strings(&["37 starts/s \u{b7} 742 running"]));
    assert_eq!(ladder(Cols), run);
    assert_eq!(ladder(NoCols), strings(&["37/s"]));
    let mut slow = view.clone();
    slow.now = Some(0.44);
    assert_eq!(
        glance_run(Resource::Processes, &slow, &s, &MAC)[0],
        "0.4 starts/s \u{b7} 742 running"
    );
    let ScopeForms::Each { aterm, .. } = scope_forms(Resource::Processes, &view, &MAC) else {
        panic!("aterm's cell");
    };
    assert_eq!(aterm.forms, strings(&["3.4 starts/s", "3.4/s"]));
    let said = row_sentence(Resource::Processes, &s, &[], &MAC);
    assert!(
        said.contains("of its 7-day peak: 37 starts a second, 742 running."),
        "{said}"
    );
    let all: Vec<String> = [Det, Cols, NoCols]
        .into_iter()
        .flat_map(ladder)
        .chain(run)
        .chain(aterm.forms)
        .collect();
    for form in &all {
        assert!(
            !["8,000", "5,333", "yours", "limit"]
                .iter()
                .any(|w| form.contains(w)),
            "{form}"
        );
    }
    let counts = row(742.0, 8_000.0);
    assert!(
        amount_forms(Resource::Processes, &counts, &s.facts, Cols, &MAC)
            .iter()
            .any(|f| f.contains("8,000")),
        "CONTROL"
    );
}

#[test]
fn rounding_parts_sum() {
    assert_eq!(bytes_dec(1_995_200_000_000), "2.00 TB");
    assert_eq!(bytes_dec(885_300_000_000), "885 GB");
    assert_eq!(bytes_dec(41_240_000_000), "41.2 GB");
    assert_eq!(
        bytes_dec(999_600_000_000),
        "1.00 TB",
        "rounds into the next unit"
    );
    assert_eq!(bytes_dec(9_995_000_000), "10.0 GB");
    assert_eq!(bytes_dec(99), "99 B");
    assert_eq!(bytes_lone(41_240_000_000), "41 GB");
    assert_eq!(bytes_lone(69_000_000_000), "69 GB");
    assert_eq!(bytes_lone(885_300_000_000), "885 GB");
    assert_eq!(bytes_lone(8_600_000_000), "8.6 GB");
    assert_eq!(bytes_lone(9_960_000_000), "10 GB");
    assert_eq!(bytes_lone(1_234_000_000_000), "1.2 TB");
    assert_eq!(rate_mb_lone(38.0e6), "38 MB/s");
    assert_eq!(rate_mb_lone(5.1e6), "5.1 MB/s");
    assert_eq!(rate_mb_lone(9.96e6), "10 MB/s");
    assert_eq!(rate_mb_lone(410e6), "410 MB/s");
    let vol = |total: u64, free: u64| VolumeSample {
        total,
        free,
        reserved: 0,
        mount: "/".into(),
    };
    assert_eq!(
        disk_space_line(&vol(2_000_000_000_000, 890_000_000_000)),
        "1.11 TB used · 890 GB free · 2.00 TB total"
    );
    assert_eq!(
        disk_space_line(&vol(2_000_000_000_000, 44_800_000_000)),
        "1.96 TB used · 44.8 GB free · 2.00 TB total"
    );
    assert_eq!(
        amount_forms(
            Resource::Disk,
            &row(1_955_200_000_000.0, 2e12),
            &RowFacts::default(),
            Ladder::NoCols,
            &MAC
        ),
        strings(&["1.96/2.00 TB used"])
    );
    let quanta = |s: &str| s.replace('.', "").parse::<u64>().expect("a figure");
    for whole in [
        99u64,
        12_345,
        128 << 30,
        512_000_000_000,
        994_400_000_000,
        1_995_200_000_000,
        2_000_000_000_000,
        7_777_777_777_777,
    ] {
        for split in [0.001, 0.1234, 0.5, 0.8765, 0.999] {
            let a = (whole as f64 * split) as u64;
            let b = (whole - a) / 3;
            let c = a.min(whole - a) / 2;
            for parts in [[a, b, whole - a - b], [c, c, whole - 2 * c]] {
                let p = parts_of(whole, &parts);
                let sum: u64 = p.parts.iter().map(|s| quanta(s)).sum();
                assert_eq!(sum, quanta(&p.whole), "{whole} {parts:?}: {p:?}");
                let decimals = |s: &str| s.split_once('.').map(|(_, f)| f.len());
                assert!(
                    p.parts.iter().all(|s| decimals(s) == decimals(&p.whole)),
                    "every part in the whole's decimal places: {p:?}"
                );
            }
        }
    }
    assert_eq!(
        parts_of(12_345, &[6_150, 6_150, 45]),
        Parts {
            whole: "12.3".into(),
            parts: strings(&["6.2", "6.1", "0.0"]),
            unit: "kB",
        }
    );
    assert_eq!(
        parts_of(1_020, &[255, 255, 255, 255]).parts,
        strings(&["0.26", "0.26", "0.25", "0.25"])
    );
}

#[test]
fn figures_follow_the_number_rules() {
    assert_eq!(pct(None), "–");
    assert_eq!(pct(Some(f32::NAN)), "–");
    assert_eq!(pct(Some(0.0)), "0%");
    assert_eq!(pct(Some(0.004)), "<1%");
    assert_eq!(pct(Some(0.125)), "13%", "half rounds up");
    assert_eq!(pct(Some(17.7 / 18.0)), "98%");
    assert_eq!(pct(Some(1.3)), "100%", "the over mark carries the excess");
    assert_eq!(rate_mb(2.1e6), "2.1 MB/s");
    assert_eq!(rate_mb(99.96e6), "100 MB/s");
    assert_eq!(rate_mb(410e6), "410 MB/s");
    assert_eq!(rate_mb(9_999e6), "9,999 MB/s");
    assert_eq!(grouped(999), "999");
    assert_eq!(grouped(16_000), "16,000");
    assert_eq!(grouped(1_000_000), "1,000,000");
    assert_eq!(tokens_k(897.0), "897");
    assert_eq!(tokens_k(3_100.0), "3.1k");
    assert_eq!(tokens_k(9_000.0), "9.0k");
    assert_eq!(tokens_k(99_960.0), "100k");
    assert_eq!(tokens_k(126_592.0), "127k");
    assert_eq!(tokens_k(999_600.0), "1.0M");
    assert_eq!(tokens_k(1_234_567.0), "1.2M");
    assert_eq!(duration_approx(20.0), "~1 min");
    assert_eq!(duration_approx(34.0 * 60.0), "~34 min");
    assert_eq!(duration_approx(3_590.0), "~1 h");
    assert_eq!(duration_approx(5.0 * 3600.0), "~5 h");
    assert_eq!(duration_approx(1.2 * 86_400.0), "~1 day");
    assert_eq!(duration_approx(2.0 * 86_400.0), "~2 days");
    assert_eq!(clock(NOW, NOW, 0), "10:40");
    assert_eq!(clock(NOW, NOW, 3_600), "11:40");
    assert_eq!(clock(NOW - 86_400_000 - 100 * MIN_MS, NOW, 0), "Tue 09:00");
}

/// The deck's duration never writes a bare `m` (it reads as megabytes), floors to the
/// minute, and speaks as minutes and hours. NEGATIVE CONTROL: `sys jobs`' own `12m` stays.
#[test]
fn a_duration_says_min_and_h_never_a_bare_m() {
    const MIN: u64 = 60_000;
    for (ms, want) in [
        (0, "under 1 min"),
        (59_999, "under 1 min"),
        (MIN, "1 min"),
        (12 * MIN + 59_999, "12 min"),
        (59 * MIN, "59 min"),
        (60 * MIN, "1 h"),
        (185 * MIN, "3 h 5 min"),
        (23 * 60 * MIN + 59 * MIN, "23 h 59 min"),
        (24 * 60 * MIN + 30 * MIN, "1 day"),
        (52 * 60 * MIN + 7 * MIN, "2 days 4 h"),
    ] {
        let got = duration(ms);
        assert_eq!(got, want, "{ms} ms");
        let bare_m = |w: &str| {
            w.strip_suffix('m')
                .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
        };
        assert!(!got.split(' ').any(bare_m), "{got}");
    }
    assert_eq!(speak(&duration(185 * MIN)), "3 hours 5 minutes");
    assert_eq!(speak(&duration(61 * MIN)), "1 hour 1 minute");
    assert_eq!(speak(&duration(0)), "under 1 minute");
    assert_eq!(
        crate::jobs::wire::age_words(12 * MIN),
        "12m",
        "sys jobs keeps its own"
    );
}

/// A program's age shows once it has run long, as `up …`, and as a lower bound while a
/// member's birth went unread. NEGATIVE CONTROLS: a young program and an unread age say
/// nothing.
#[test]
fn a_long_runner_says_how_long_it_has_been_up() {
    const MIN: u64 = 60_000;
    assert_eq!(
        up_words(Some(47 * MIN), false).as_deref(),
        Some("up 47 min")
    );
    assert_eq!(
        up_words(Some(185 * MIN), true).as_deref(),
        Some("up ≥ 3 h 5 min")
    );
    assert_eq!(
        up_words(Some(LONG_RUN_MS), false).as_deref(),
        Some("up 10 min")
    );
    assert_eq!(up_words(Some(LONG_RUN_MS - 1), false), None, "young");
    assert_eq!(up_words(None, true), None, "unread");
    assert_eq!(
        speak(&up_words(Some(47 * MIN), true).expect("long")),
        "up at least 47 minutes"
    );
}

/// The edges of [`footprints_over`] the deck's own test leaves (`aterm-gui`'s
/// `the_this_mac_fill_is_the_percent_and_nested_fills_never_widen_it` holds below, equal and
/// above in speech and the lens): only the Memory row says it; a tab with no name says
/// nothing of its own; with no used memory read, aterm's has nothing around it, nor has the
/// tab's while aterm has no figure; an aterm figure that is no number passes the tab on to the
/// machine's, as the bar does; the machine is named as the platform names it; and each says
/// whose it is apart from its name, so a tab named "aterm" is still the tab.
/// CONTROL: the view that says nothing on another row says both on Memory.
#[test]
fn a_footprint_past_the_scope_around_it_is_said_in_words() {
    let view = |used: Option<f64>, aterm: Option<f64>, tab: Option<f64>| ResourceView {
        now: used.map(|g| g * GIB),
        aterm: aterm.map_or_else(ScopeFig::default, |g| fig(g * GIB)),
        tab: tab.map_or_else(ScopeFig::default, |g| fig(g * GIB)),
        ..ResourceView::default()
    };
    let said = |v: &ResourceView, tab: Option<&str>, loc: &Locale| -> Vec<String> {
        footprints_over(Resource::Memory, v, tab, loc)
            .iter()
            .map(FootprintOver::said)
            .collect()
    };
    let both = view(Some(14.0), Some(15.0), Some(16.0));
    assert_eq!(
        said(&both, Some("Tab 1"), &MAC),
        [
            "aterm's footprint is larger than this Mac's used memory",
            "Tab 1's footprint is larger than aterm's"
        ],
        "CONTROL"
    );
    for res in Resource::ALL.into_iter().filter(|&r| r != Resource::Memory) {
        assert!(
            footprints_over(res, &both, Some("Tab 1"), &MAC).is_empty(),
            "{res:?}"
        );
    }
    assert_eq!(
        said(&both, None, &MAC),
        ["aterm's footprint is larger than this Mac's used memory"],
        "no tab named"
    );
    assert_eq!(
        said(&view(None, Some(15.0), None), Some("Tab 1"), &MAC),
        Vec::<String>::new(),
        "no used memory read"
    );
    assert_eq!(
        said(&view(None, None, Some(15.0)), Some("Tab 1"), &MAC),
        Vec::<String>::new(),
        "nothing around the tab"
    );
    assert_eq!(
        said(
            &ResourceView {
                aterm: fig(f64::NAN),
                ..view(Some(14.0), None, Some(15.0))
            },
            Some("Tab 1"),
            &MAC
        ),
        ["Tab 1's footprint is larger than this Mac's used memory"],
        "an aterm figure that is no number"
    );
    assert_eq!(
        said(&view(Some(14.0), Some(15.0), None), None, &LINUX),
        ["aterm's footprint is larger than this computer's used memory"]
    );
    let clauses: Vec<(bool, String)> =
        footprints_over(Resource::Memory, &both, Some("aterm"), &MAC)
            .iter()
            .map(|f| (f.tab, f.clause()))
            .collect();
    assert_eq!(
        clauses,
        [
            (false, "larger than this Mac's used memory".to_owned()),
            (true, "larger than aterm's".to_owned())
        ],
        "a tab named aterm is still the tab"
    );
}

#[test]
fn scope_cells_name_their_unit() {
    let each = |res: Resource, aterm: ScopeFig, tab: ScopeFig, loc: &Locale| {
        let view = ResourceView {
            aterm,
            tab,
            ..ResourceView::default()
        };
        match scope_forms(res, &view, loc) {
            ScopeForms::Each { aterm, tab } => (aterm, tab),
            ScopeForms::Span(c) => panic!("{res:?} spans: {c:?}"),
        }
    };
    let words = |forms: &[&str]| ScopeCell {
        forms: strings(forms),
        ink: InkKind::Words,
    };
    let words2 = |s: &str| ScopeCell {
        forms: strings(&[s]),
        ink: InkKind::Words2,
    };
    assert_eq!(
        each(Resource::Cpu, fig(6.8), fig(0.04), &MAC),
        (words(&["6.8 cores"]), words(&["0.0 cores"]))
    );
    assert_eq!(
        each(Resource::Memory, fig(5.0 * GIB), fig(0.18 * GIB), &MAC),
        (words(&["5.0 GiB"]), words(&["0.18 GiB"]))
    );
    assert_eq!(
        each(Resource::Disk, fig(38.4e6), fig(0.04e6), &MAC),
        (words(&["writes 38 MB/s"]), words(&["writes 0"]))
    );
    assert_eq!(
        each(Resource::Disk, fig(5.1e6), fig(0.2e6), &MAC),
        (
            words(&["writes 5.1 MB/s", "writes 5 MB/s"]),
            words(&["writes 0.2 MB/s"])
        )
    );
    assert_eq!(
        each(Resource::Disk, fig(410e6), fig(0.2e6), &MAC).0,
        words(&["writes 410 MB/s", "writes 0.41 GB/s", "writes 0.4 GB/s"])
    );
    assert_eq!(
        each(
            Resource::Tokens,
            fig(3_100.0),
            missing(NoFigure::NoAgent),
            &MAC
        ),
        (words(&["3.1k/min"]), words2("no agent"))
    );
    assert_eq!(
        each(Resource::Processes, fig(42.0), fig(1.0), &MAC),
        (words(&["42 processes", "42"]), words(&["1 process", "1"]))
    );
    assert_eq!(
        each(Resource::Processes, fig(212.0), fig(3.0), &LINUX),
        (
            words(&["212 processes", "212"]),
            words(&["3 processes", "3"])
        )
    );
    assert_eq!(
        each(
            Resource::Cpu,
            ScopeFig::default(),
            ScopeFig::default(),
            &MAC
        ),
        (words2("–"), words2("–"))
    );
    let span = |res: Resource, why: NoFigure, loc: &Locale| {
        let view = ResourceView {
            aterm: missing(why),
            tab: missing(why),
            ..ResourceView::default()
        };
        scope_forms(res, &view, loc)
    };
    let spanned = |forms: &[&str]| {
        ScopeForms::Span(ScopeCell {
            forms: strings(forms),
            ink: InkKind::Words2,
        })
    };
    for res in [Resource::Gpu, Resource::Network] {
        assert_eq!(
            span(res, NoFigure::NotPerApp, &MAC),
            spanned(&["no per-app figure", "not per app"])
        );
    }
    assert_eq!(
        span(Resource::Disk, NoFigure::NotReadHere, &LINUX),
        spanned(&["writes not read on Linux", "writes not read"])
    );
}

#[test]
fn rule_s_says_each_state_in_words() {
    let words = |s: &Snapshot, res: Resource| state(res, s, &MAC).expect("a state");

    let mut s = snap();
    let mut mem = row(112.6 * GIB, 128.0 * GIB);
    mem.state = Some(State::Warn);
    set(&mut s, Resource::Memory, mem);
    assert_eq!(
        words(&s, Resource::Memory),
        StateWords {
            state: State::Warn,
            clauses: strings(&["pressure warn"]),
            word: "warn".into(),
            label: "Memory warn".into(),
            amount_forms: strings(&["112.6/128 GiB"]),
            mark: Mark::Dot,
            ink: InkKind::Warn,
        }
    );

    let mut s = disk_trouble();
    s.facts.disk.as_mut().expect("facts").stuck = 0;
    s.resources[Resource::Disk.index()].state = Some(State::Warn);
    assert_eq!(
        words(&s, Resource::Disk),
        StateWords {
            state: State::Warn,
            clauses: strings(&["full in ~34 min, filling 1.2 GB/min", "full in ~34 min"]),
            word: "full ~34 min".into(),
            label: "Disk full ~34 min".into(),
            amount_forms: strings(&["1.96/2.00 TB used", "1.96 TB used"]),
            mark: Mark::Low,
            ink: InkKind::Warn,
        }
    );
    let w = words(&disk_trouble(), Resource::Disk);
    assert_eq!((w.state, w.ink), (State::Critical, InkKind::Critical));
    assert_eq!(
        (w.clauses, w.word.as_str(), w.label.as_str()),
        (
            strings(&["2 sessions stuck on a full disk", "2 stuck"]),
            "2 stuck",
            "Disk 2 stuck"
        )
    );
    let mut s = disk_trouble();
    s.facts.filling = None;
    s.facts.disk = Some(watch(DiskLevel::Low));
    s.resources[Resource::Disk.index()].state = Some(State::Warn);
    let w = words(&s, Resource::Disk);
    assert_eq!(
        (w.clauses, w.word.as_str(), w.label.as_str()),
        (strings(&["low, 45 GB free"]), "low", "Disk low")
    );
    s.facts.disk = Some(watch(DiskLevel::EmergencyFiles));
    s.resources[Resource::Disk.index()].state = Some(State::Critical);
    let w = words(&s, Resource::Disk);
    assert_eq!(
        (w.clauses, w.word.as_str(), w.label.as_str(), w.ink),
        (
            strings(&["emergency, out of file slots"]),
            "emergency",
            "Disk emergency",
            InkKind::Critical
        )
    );

    assert_eq!(
        words(&processes_trouble(), Resource::Processes),
        StateWords {
            state: State::Stuck,
            clauses: strings(&["1 stuck, 1 zombie, 2 quiet", "1 stuck"]),
            word: "1 stuck".into(),
            label: "1 stuck".into(),
            amount_forms: strings(&["897/16,000"]),
            mark: Mark::Stuck,
            ink: InkKind::Stuck,
        }
    );

    let mut s = snap();
    let mut gpu = row(12.0, 100.0);
    gpu.state = Some(State::Warn);
    set(&mut s, Resource::Gpu, gpu);
    s.facts.gpu_resets = Some((1, NOW - 28 * MIN_MS));
    let w = words(&s, Resource::Gpu);
    assert_eq!(
        (w.clauses, w.word.as_str(), w.label.as_str(), w.mark),
        (
            strings(&["GPU reset 1\u{d7} since 10:12", "reset 1\u{d7}"]),
            "reset 1\u{d7}",
            "GPU reset 1\u{d7}",
            Mark::Dot
        )
    );

    // The CPU's state is the machine's thermal level (A2 §3.2): its words say "thermal",
    // the OS's word alone where a run has room for no more, and its header label is the
    // clause itself, which names no row.
    let mut s = snap();
    let mut cpu = row(9.2, 10.0);
    cpu.state = Some(State::Warn);
    set(&mut s, Resource::Cpu, cpu);
    let level = |level: Throttle, raw: u8, exact: bool| Thermal {
        level,
        raw,
        low_power: Some(false),
        exact,
    };
    s.thermal = Some(level(Throttle::Heavy, 2, true));
    let w = words(&s, Resource::Cpu);
    assert_eq!(
        (
            w.state,
            w.clauses,
            w.word.as_str(),
            w.label.as_str(),
            w.mark,
            w.ink
        ),
        (
            State::Warn,
            strings(&["thermal serious"]),
            "serious",
            "thermal serious",
            Mark::Dot,
            InkKind::Warn
        )
    );
    s.resources[Resource::Cpu.index()].state = Some(State::Critical);
    s.thermal = Some(level(Throttle::Severe, 3, true));
    let w = words(&s, Resource::Cpu);
    assert_eq!(
        (w.label.as_str(), w.word.as_str(), w.ink),
        ("thermal critical", "critical", InkKind::Critical)
    );
    // No words without the exact level that makes the state.
    for thermal in [
        None,
        Some(level(Throttle::Light, 1, true)),
        Some(level(Throttle::Severe, 0, false)),
    ] {
        s.thermal = thermal;
        assert_eq!(state(Resource::Cpu, &s, &MAC), None, "{thermal:?}");
    }

    let mut s = snap();
    for res in Resource::ALL {
        let mut v = row(1.0, 2.0);
        if matches!(res, Resource::Network | Resource::Tokens) {
            v.state = Some(State::Warn);
        }
        set(&mut s, res, v);
        assert_eq!(state(res, &s, &MAC), None, "{res:?}");
    }
}

type Shape = (u8, bool, Option<Mark>, InkKind, Vec<String>);

fn shape(clauses: &[Clause]) -> Vec<Shape> {
    clauses
        .iter()
        .map(|c| (c.prio, c.keep, c.mark, c.ink, c.forms.clone()))
        .collect()
}

#[test]
fn details_lead_with_kept_state_clauses() {
    use InkKind::{Critical, Warn, Words, Words2};
    let f = |forms: &[&str]| strings(forms);

    assert_eq!(
        shape(&details(Resource::Disk, &disk_trouble(), &MAC)),
        vec![
            (
                0,
                true,
                Some(Mark::Low),
                Warn,
                f(&["full in ~34 min, filling 1.2 GB/min", "full in ~34 min"])
            ),
            (1, true, None, Warn, f(&["low, 45 GB free"])),
            (
                1,
                true,
                None,
                Critical,
                f(&["2 sessions stuck on a full disk", "2 stuck"])
            ),
            (
                3,
                false,
                None,
                Words,
                f(&["aterm freed 41 GB today", "freed 41 GB"])
            ),
            (4, false, None, Words, f(&["+2.6 GB in 10 min"])),
            (
                6,
                false,
                None,
                Words,
                f(&["most in ~/aterm/target, 212 GB", "~/aterm/target 212 GB"])
            ),
        ]
    );
    let mut s = disk_trouble();
    s.facts.filling = Some(Filling {
        secs: 5.0 * 3600.0,
        rate: 20e6,
        burst: false,
    });
    s.facts.disk_rates.rate10 = Some(-1.2e9 / 600.0);
    s.facts.disk = Some(DiskFacts {
        stalled_since: Some(NOW - 9 * MIN_MS),
        ..watch(DiskLevel::Ok)
    });
    assert_eq!(
        shape(&details(Resource::Disk, &s, &MAC)),
        vec![
            (
                2,
                true,
                None,
                Warn,
                f(&["disk watch stalled since 10:31", "watch stalled"])
            ),
            (
                2,
                false,
                None,
                Words,
                f(&["filling 1.2 GB/min, full in ~5 h", "full in ~5 h"])
            ),
            (4, false, None, Words, f(&["\u{2212}1.2 GB in 10 min"])),
            (5, false, None, Words, f(&["45 GB free"])),
        ]
    );
    let mut s = disk_trouble();
    s.facts.filling = None;
    s.facts.disk_rates = Rates::default();
    s.facts.disk.as_mut().expect("facts").watch = false;
    assert_eq!(
        shape(&details(Resource::Disk, &s, &MAC)),
        vec![
            (2, false, None, Words2, f(&["disk watch off"])),
            (
                4,
                false,
                None,
                Words2,
                f(&["growth after 10 min of readings", "growth after 10 min"])
            ),
            (5, false, None, Words, f(&["45 GB free"])),
            (
                6,
                false,
                None,
                Words,
                f(&["most in ~/aterm/target, 212 GB", "~/aterm/target 212 GB"])
            ),
        ]
    );
    assert_eq!(
        state(Resource::Disk, &s, &MAC),
        None,
        "no state the watch would judge"
    );

    let mut s = snap();
    s.mem = Some(MemView {
        sample: MemSample {
            total: 128 << 30,
            cached: (38.5 * GIB) as u64,
            swap_used: (6.4 * GIB) as u64,
            pressure: Pressure::Warn,
            ..MemSample::default()
        },
        ..MemView::default()
    });
    s.facts.swap_rate = Some(36.0 * MIB);
    assert_eq!(
        shape(&details(Resource::Memory, &s, &MAC)),
        vec![
            (0, true, Some(Mark::Dot), Warn, f(&["pressure warn"])),
            (1, false, None, Words, f(&["swapping 36 MiB/s"])),
            (2, false, None, Words, f(&["file cache 38.5 GiB"])),
            (3, false, None, Words, f(&["swap 6.4 GiB"])),
        ]
    );
    s.mem.as_mut().expect("mem").sample.pressure = Pressure::Normal;
    assert_eq!(
        shape(&details(Resource::Memory, &s, &MAC))[0],
        (0, false, None, Words2, f(&["pressure normal"]))
    );

    // The CPU's thermal state leads its Details and its glance, kept and marked as Memory's
    // pressure is, ahead of a kept "waiting" clause; it is said "Thermal serious.".
    let mut c = snap();
    set(&mut c, Resource::Cpu, row(9.2, 10.0));
    c.facts.load1 = Some(14.0);
    c.thermal = Some(Thermal {
        level: Throttle::Heavy,
        raw: 2,
        low_power: None,
        exact: true,
    });
    let full = details(Resource::Cpu, &c, &MAC);
    assert_eq!(
        shape(&full)[..2],
        [
            (0, true, Some(Mark::Dot), Warn, f(&["thermal serious"])),
            (
                0,
                false,
                None,
                Words,
                f(&["4 waiting for a core (load 14.0)", "load 14.0 on 10 cores"])
            ),
        ]
    );
    assert_eq!(full[0].spoken, "Thermal serious.");
    let glance = glance_details(Resource::Cpu, &c, &[], &MAC);
    assert_eq!(
        shape(&glance)[..2],
        [
            (0, true, Some(Mark::Dot), Warn, f(&["thermal serious"])),
            (0, true, None, Words, f(&["4 waiting for a core"])),
        ]
    );
    c.thermal = Some(Thermal {
        level: Throttle::Light,
        raw: 1,
        ..c.thermal.expect("a level")
    });
    assert!(
        details(Resource::Cpu, &c, &MAC)
            .iter()
            .chain(&glance_details(Resource::Cpu, &c, &[], &MAC))
            .all(|c| !c.forms[0].starts_with("thermal")),
        "CONTROL: fair says no thermal clause"
    );
    let service = |exe: &str, footprint: u64| crate::explain::CoalRow {
        id: 777,
        footprint: Some(footprint),
        live: 1,
        lead: Some(crate::explain::Lead {
            pid: 537,
            uid: Some(0),
            exe: Some(exe.into()),
            ..crate::explain::Lead::default()
        }),
        ..crate::explain::CoalRow::default()
    };
    let split = |row| {
        Some(std::sync::Arc::new(crate::explain::WhyFacts {
            lane: crate::explain::CoalLane::On,
            split: crate::explain::CoalSplit {
                rows: vec![row],
                ..crate::explain::CoalSplit::default()
            },
            ..crate::explain::WhyFacts::default()
        }))
    };
    s.why = split(service("fseventsd", (44.4 * GIB) as u64));
    assert_eq!(
        shape(&details(Resource::Memory, &s, &MAC))[0],
        (
            0,
            true,
            Some(Mark::Dot),
            Critical,
            f(&[
                "macOS's fseventsd holds 44.4 GiB",
                "fseventsd holds 44.4 GiB"
            ])
        ),
        "a service past the huge bar is memory's state, and leads"
    );
    s.why = split(service("mds", (1.3 * GIB) as u64));
    assert_eq!(
        details(Resource::Memory, &s, &MAC).len(),
        4,
        "a service under the bar is not named"
    );

    assert_eq!(
        shape(&details(Resource::Processes, &processes_trouble(), &MAC)),
        vec![
            (0, true, Some(Mark::Stuck), InkKind::Stuck, f(&["1 stuck"])),
            (1, false, Some(Mark::Zombie), Warn, f(&["1 zombie"])),
            (1, false, Some(Mark::Quiet), InkKind::Stuck, f(&["2 quiet"])),
            (
                2,
                false,
                None,
                Words,
                f(&["yours 578 of 10,666", "yours 578"])
            ),
        ]
    );

    let mut s = snap();
    set(&mut s, Resource::Cpu, row(17.7, 18.0));
    s.facts.load1 = Some(25.7);
    s.facts.busiest = Some(Busiest {
        name: "rustc".into(),
        cores: 5.9,
        n: 9,
        scope: Some(ProcScope::Tab(3)),
        born_ms: None,
        unborn: 9,
    });
    let busiest = (
        1,
        false,
        None,
        Words,
        f(&[
            "busiest rustc \u{d7}9 in Tab 3, 5.9 cores",
            "rustc \u{d7}9 in Tab 3",
        ]),
    );
    assert_eq!(
        shape(&details(Resource::Cpu, &s, &MAC)),
        vec![
            (
                0,
                false,
                None,
                Words,
                f(&["8 waiting for a core (load 25.7)", "load 25.7 on 18 cores"])
            ),
            busiest.clone(),
        ]
    );
    assert_eq!(
        shape(&details(Resource::Cpu, &s, &LINUX)),
        vec![
            (
                0,
                false,
                None,
                Words,
                f(&["load 25.7 on 18 cores", "load 25.7"])
            ),
            busiest.clone()
        ]
    );
    s.why = spotlight(Some(Context::Startup));
    assert_eq!(
        shape(&details(Resource::Cpu, &s, &MAC)),
        vec![
            (
                0,
                false,
                None,
                Words,
                f(&["8 waiting for a core (load 25.7)", "load 25.7 on 18 cores"])
            ),
            (
                1,
                false,
                Some(Mark::Cause),
                Words,
                f(&["Spotlight, after start-up", "Spotlight"])
            ),
            busiest
        ]
    );

    let mut s = snap();
    let mut net = row(2.1e6, 22.9e6);
    net.basis = CapBasis::Link;
    net.second = Some(series(0.3e6, 22.9e6, false));
    set(&mut s, Resource::Network, net);
    s.facts.net_read_s = 3_600;
    s.facts.link = Some(LinkSample {
        name: "en0".into(),
        kind: LinkKind::Wifi,
        counts: true,
        rate_bps: Some(183_200_000),
        ..LinkSample::default()
    });
    assert_eq!(
        shape(&details(Resource::Network, &s, &MAC)),
        vec![
            (
                0,
                false,
                None,
                Words2,
                f(&["learning its peak: 1 of 24 h read", "learning peak"])
            ),
            (
                0,
                false,
                Some(Mark::Up),
                Words,
                f(&["up 0.3 of 22.9 MB/s link", "up 0.3 MB/s"])
            ),
            (
                1,
                false,
                None,
                Words,
                f(&["Wi-Fi en0, link 22.9 MB/s", "Wi-Fi en0"])
            ),
        ]
    );
    s.facts.net_read_s = NET_LEARN_S;
    let clauses = details(Resource::Network, &s, &MAC);
    assert!(
        clauses.iter().all(|c| !c.forms[0].contains("learning")),
        "{clauses:?}"
    );
    assert!(!row_sentence(Resource::Network, &s, &[], &MAC).contains("Learning"));
    let mut lost = row(2.1e6, 1.0);
    lost.capacity = None;
    lost.heat_now = None;
    lost.basis = CapBasis::Peak;
    assert_eq!(
        amount_forms(
            Resource::Network,
            &lost,
            &RowFacts::default(),
            Ladder::Det,
            &MAC
        ),
        strings(&["2.1 MB/s, no peak in 7 days"])
    );

    let mut s = snap();
    s.facts.tokens_in = Some(410_000.0);
    s.facts.plan = Some(PlanFact {
        window_word: "weekly".into(),
        used_x10: 340,
        read_at: NOW - 2 * MIN_MS,
        ..PlanFact::default()
    });
    let input = (
        1,
        false,
        None,
        Words,
        f(&["input 410k/min, cache re-reads included", "input 410k/min"]),
    );
    assert_eq!(
        shape(&details(Resource::Tokens, &s, &MAC)),
        vec![
            (
                0,
                false,
                None,
                Words,
                f(&[
                    "weekly 34% used (account), read 10:38",
                    "weekly 34% used, 10:38"
                ])
            ),
            input.clone(),
        ]
    );
    s.facts.plan = None;
    assert_eq!(
        shape(&details(Resource::Tokens, &s, &MAC)),
        vec![input, (2, false, None, Words2, f(&["plan limit not read"]))]
    );

    let mut s = snap();
    set(&mut s, Resource::Gpu, row(12.0, 100.0));
    s.facts.gpu_resets = Some((1, NOW - 28 * MIN_MS));
    s.facts.gpu = Some(GpuSample {
        cores: Some(40),
        in_use_bytes: Some((1.7 * GIB) as u64),
        ..GpuSample::default()
    });
    assert_eq!(
        shape(&details(Resource::Gpu, &s, &MAC)),
        vec![
            (
                0,
                true,
                Some(Mark::Dot),
                Warn,
                f(&["GPU reset 1\u{d7} since 10:12"])
            ),
            (0, false, None, Words, f(&["40 GPU cores"])),
            (
                1,
                false,
                None,
                Words,
                f(&["1.7 GiB GPU memory in use", "1.7 GiB in use"])
            ),
        ]
    );
    set(&mut s, Resource::Gpu, ResourceView::default());
    assert!(details(Resource::Gpu, &s, &MAC).is_empty());
}

#[test]
fn cpu_details_name_the_cause_after_load_and_before_busiest() {
    let mut s = snap();
    set(&mut s, Resource::Cpu, row(3.3, 18.0));
    s.facts.load1 = Some(5.6);
    s.facts.busiest = Some(Busiest {
        name: "siriactionsd".into(),
        cores: 0.7,
        n: 1,
        scope: None,
        born_ms: None,
        unborn: 1,
    });
    let read = |s: &Snapshot| -> Vec<(Vec<String>, Option<Mark>, String)> {
        details(Resource::Cpu, s, &MAC)
            .into_iter()
            .map(|c| (c.forms, c.mark, c.spoken))
            .collect()
    };
    let load = (
        strings(&["load 5.6 on 18 cores", "load 5.6"]),
        None,
        "Load 5.6 on 18 cores.".to_owned(),
    );
    let busiest = (
        strings(&["busiest siriactionsd, 0.7 cores", "siriactionsd"]),
        None,
        "Busiest siriactionsd, 0.7 cores.".to_owned(),
    );
    assert_eq!(read(&s), [load.clone(), busiest.clone()]);
    for (context, long) in [
        (Some(Context::Startup), "Spotlight, after start-up"),
        (Some(Context::Wake), "Spotlight, since wake"),
        (Some(Context::Throttled), "Spotlight"),
        (None, "Spotlight"),
    ] {
        s.why = spotlight(context);
        let cause = (
            strings(&[long, "Spotlight"]),
            Some(Mark::Cause),
            format!("Why: {long}."),
        );
        assert_eq!(
            read(&s),
            [load.clone(), cause, busiest.clone()],
            "{context:?}"
        );
        assert_eq!(state(Resource::Cpu, &s, &MAC), None, "a cause is no state");
    }

    s.why = spotlight(Some(Context::Startup));
    s.at_ns = 15_000_000_000;
    assert_eq!(read(&s).len(), 3, "15 s old");
    s.at_ns += 1_000_000;
    assert_eq!(read(&s), [load.clone(), busiest.clone()], "older");
    s.at_ns = 0;
    s.why = Some(Arc::new(WhyFacts::default()));
    assert_eq!(read(&s), [load, busiest], "no clause held");
}

/// Tab 2 split in panes 2 and 3 speaks as one tab: named once, its figure both panes'.
#[test]
fn a_split_tab_speaks_for_all_its_panes() {
    let mut s = snap();
    s.tabs[1].label = "Tab 2".into();
    let mut cpu = steady(row(17.7, 18.0), 48);
    cpu.aterm = fig(6.8);
    cpu.tabs = vec![(2, fig(0.1)), (3, fig(2.4))];
    set(&mut s, Resource::Cpu, cpu);
    let said = |panes: &[u64]| row_sentence(Resource::Cpu, &s, panes, &MAC);
    assert!(
        said(&[2, 3]).contains(". aterm 6.8 cores, Tab 2 2.5 cores. "),
        "{}",
        said(&[2, 3])
    );
    assert_eq!(said(&[3, 2]), said(&[2, 3]));
    assert!(
        said(&[2]).contains(". aterm 6.8 cores, Tab 2 0.1 cores. "),
        "one pane, as before: {}",
        said(&[2])
    );
}

#[test]
fn row_sentences() {
    let mut s = snap();
    let mut cpu = steady(row(17.7, 18.0), 48);
    cpu.aterm = fig(6.8);
    cpu.tabs = vec![(2, fig(0.1))];
    set(&mut s, Resource::Cpu, cpu);
    s.facts.load1 = Some(25.7);
    assert_eq!(
        row_sentence(Resource::Cpu, &s, &[2], &MAC),
        "CPU 98 percent of capacity: 17.7 of 18 cores. aterm 6.8 cores, Tab 2 0.1 cores. \
         Steady near 98 percent. Over capacity 8 of the last 10 minutes. 8 waiting for a \
         core, load 25.7 on 18 cores."
    );
    s.why = spotlight(Some(Context::Startup));
    s.facts.busiest = Some(Busiest {
        name: "rustc".into(),
        cores: 5.9,
        n: 9,
        scope: Some(ProcScope::Tab(3)),
        born_ms: None,
        unborn: 9,
    });
    assert!(
        row_sentence(Resource::Cpu, &s, &[2], &MAC).ends_with(
            "8 waiting for a core, load 25.7 on 18 cores. Why: Spotlight, after start-up. \
             Busiest rustc, 9 processes in Tab 3, 5.9 cores."
        ),
        "the meter says the cause where no Details column shows"
    );

    assert_eq!(
        row_sentence(Resource::Disk, &disk_trouble(), &[2], &MAC),
        "Disk 98 percent of capacity: 1.96 terabytes used, 44.8 gigabytes free, 2.00 \
         terabytes total. aterm writes 38 megabytes per second, Tab 2 writes 0.2 megabytes per second. Steady near 98 \
         percent. Disk full in about 34 minutes, filling 1.2 gigabytes per minute. Disk low, \
         45 gigabytes free. 2 sessions stuck on a full disk. aterm freed 41 gigabytes \
         today. Used grew 2.6 gigabytes in the last 10 minutes. Most in ~/aterm/target, 212 \
         gigabytes."
    );

    let mut s = snap();
    let mut net = steady(row(2.1e6, 22.9e6), 0);
    net.basis = CapBasis::Link;
    net.second = Some(series(0.3e6, 22.9e6, false));
    net.aterm = missing(NoFigure::NotPerApp);
    net.tab = missing(NoFigure::NotPerApp);
    set(&mut s, Resource::Network, net);
    s.facts.net_read_s = 3_600;
    s.facts.link = Some(LinkSample {
        name: "en0".into(),
        kind: LinkKind::Wifi,
        counts: true,
        rate_bps: Some(183_200_000),
        ..LinkSample::default()
    });
    assert_eq!(
        row_sentence(Resource::Network, &s, &[2], &MAC),
        "Network 9 percent of its link rate: 2.1 of 22.9 megabytes per second, downloading. \
         Learning its peak: 1 of 24 hours read. No per-app figure. Steady near 9 percent. \
         Sending 0.3 of 22.9 megabytes per second. Wi-Fi en0, link 22.9 megabytes per \
         second."
    );
    s.net = Some(crate::NetView {
        tcp: Some(crate::TcpCounts {
            established: 14,
            listen: 6,
            time_wait: 3,
            ..crate::TcpCounts::default()
        }),
        ..crate::NetView::default()
    });
    assert!(
        row_sentence(Resource::Network, &s, &[2], &MAC)
            .ends_with("14 TCP connections established, 6 listening."),
        "network row sentence reports TCP connection counts when present"
    );

    let mut s = snap();
    let mut tokens = steady(row(3_100.0, 1.0), 0);
    tokens.capacity = None;
    tokens.heat_now = None;
    tokens.basis = CapBasis::Learning;
    tokens.aterm = fig(3_100.0);
    tokens.tabs = vec![(2, missing(NoFigure::NoAgent))];
    set(&mut s, Resource::Tokens, tokens);
    s.facts.tokens_read_s = 60;
    assert_eq!(
        row_sentence(Resource::Tokens, &s, &[2], &MAC),
        "Tokens: learning aterm's peak, 1 of 10 minutes. aterm 3.1 thousand per minute. \
         Tab 2 has no agent. Plan limit not read."
    );

    let unread_said = |unread: usize| {
        let mut s = snap();
        let mut cpu = steady(row(9.0, 18.0), 0);
        cpu.ring = (0..60)
            .map(|i| Cell {
                mean: (i >= unread).then_some(9.0),
                max: (i >= unread).then_some(9.0),
                tau_ms: if i >= unread { 10_000.0 } else { 0.0 },
                over_ms: 0.0,
            })
            .collect::<Vec<_>>()
            .into();
        set(&mut s, Resource::Cpu, cpu);
        row_sentence(Resource::Cpu, &s, &[], &MAC)
    };
    for (unread, said) in [
        (1, "Not read for 10 seconds of the last 10 minutes."),
        (12, "Not read for 2 of the last 10 minutes."),
        (59, "Not read for 9 of the last 10 minutes."),
    ] {
        let got = unread_said(unread);
        assert!(got.contains(said), "{unread}: {got}");
    }

    assert_eq!(
        row_sentence(Resource::Cpu, &Snapshot::default(), &[], &MAC),
        "CPU: reading."
    );
    assert_eq!(
        row_sentence(Resource::Gpu, &snap(), &[], &MAC),
        "GPU: not read."
    );
}

#[test]
fn the_disk_lens_answers_wrong() {
    let mut s = disk_trouble();
    s.facts.filling = None;
    s.facts.disk = Some(watch(DiskLevel::Ok));
    s.volume.as_mut().expect("volume").free = 885_000_000_000;
    s.facts.disk_rates.rate60 = Some(885e9 / (2.0 * 86_400.0));
    assert_eq!(
        disk_wrong(&s, &MAC).as_deref(),
        Some(
            "No. At the last hour's rate, 885 GB free lasts about 2 days. No session is \
             stuck on a full disk."
        )
    );
    s.volume.as_mut().expect("volume").free = 41_200_000_000;
    s.facts.disk = Some(watch(DiskLevel::Low));
    assert_eq!(
        disk_wrong(&s, &MAC).as_deref(),
        Some(
            "Yes. 41 GB free is under the 69 GB warning line. aterm cleans its build \
             caches below 34 GB free."
        )
    );
    s.facts.disk = Some(watch(DiskLevel::Short));
    assert_eq!(
        disk_wrong(&s, &MAC).as_deref(),
        Some(
            "Yes. 41 GB free is under the 34 GB cleaning line. aterm is cleaning its \
             build caches."
        )
    );
    s.facts.disk = Some(DiskFacts {
        stuck: 2,
        ..watch(DiskLevel::Ok)
    });
    assert!(
        disk_wrong(&s, &MAC)
            .is_some_and(|w| w.starts_with("Yes. 2 sessions are stuck on a full disk."))
    );
    s.facts.disk.as_mut().expect("facts").watch = false;
    assert_eq!(disk_wrong(&s, &MAC), None);
}

#[test]
fn wells_and_strip_sentences() {
    let mut s = snap();
    assert_eq!(
        strip_words(Resource::Gpu, &s),
        Some(strings(&[
            "no GPU reading on this system",
            "no GPU reading"
        ]))
    );
    s.facts.gpu = Some(GpuSample {
        name: Some("Apple M5 Max".into()),
        cores: Some(40),
        ..GpuSample::default()
    });
    assert_eq!(
        strip_words(Resource::Gpu, &s),
        Some(strings(&[
            "no GPU reading on this system",
            "no GPU reading"
        ])),
        "known hardware captions no capacity over the strip: unread utilization says so"
    );
    assert!(s.resources[Resource::Gpu.index()].now.is_none());
    set(&mut s, Resource::Gpu, steady(row(12.0, 100.0), 0));
    assert_eq!(strip_words(Resource::Gpu, &s), None);
    let mut tokens = row(3_100.0, 1.0);
    tokens.basis = CapBasis::Learning;
    set(&mut s, Resource::Tokens, tokens);
    s.facts.tokens_read_s = 60;
    assert_eq!(
        strip_words(Resource::Tokens, &s),
        Some(strings(&[
            "learning aterm's token peak: 1 of 10 min",
            "learning token peak: 1 of 10 min",
            "learning peak: 1 of 10 min",
            "learning: 1/10 min",
            "learning"
        ]))
    );

    let mut net = row(2.1e6, 11.8e6);
    net.second = Some(series(0.3e6, 4.2e6, false));
    assert_eq!(
        well(Resource::Network, &net),
        Some((Mark::Down, InkKind::Words2))
    );
    net.second = Some(series(4.0e6, 4.2e6, true));
    assert_eq!(
        well(Resource::Network, &net),
        Some((Mark::Up, InkKind::Words2))
    );
    let with = |state: State| ResourceView {
        state: Some(state),
        ..ResourceView::default()
    };
    assert_eq!(
        well(Resource::Disk, &with(State::Warn)),
        Some((Mark::Low, InkKind::Warn))
    );
    assert_eq!(
        well(Resource::Memory, &with(State::Critical)),
        Some((Mark::Dot, InkKind::Critical))
    );
    assert_eq!(
        well(Resource::Processes, &with(State::Quiet)),
        Some((Mark::Quiet, InkKind::Stuck))
    );
    assert_eq!(
        well(Resource::Processes, &with(State::Zombies)),
        Some((Mark::Zombie, InkKind::Warn))
    );
    assert_eq!(well(Resource::Cpu, &row(1.0, 2.0)), None);
}

/// The glance says a row as one figure run at the end of its bar: this Mac's reading in the
/// row's unit with the row's maximum, then without it. A share of something other than the
/// row's own capacity keeps its base (Network's peak, the user's process limit where it
/// binds); the system's process limit is said nowhere, and the processes' count stands bare
/// last. The GPU and a row not read say none.
#[test]
fn the_glance_run_says_the_reading_with_its_unit_and_the_maximum() {
    let s = snap();
    let said = |loc: &Locale| -> Vec<(Resource, Vec<String>)> {
        ladder_rows(loc)
            .into_iter()
            .map(|(res, view)| (res, glance_run(res, &view, &s, loc)))
            .collect()
    };
    let golden = |procs: &[&str]| {
        [
            (Resource::Cpu, strings(&["17.7 of 18 cores", "17.7 cores"])),
            (Resource::Gpu, Vec::new()),
            (
                Resource::Memory,
                strings(&["42.1 of 128 GiB used", "42.1/128 GiB used", "42.1 GiB used"]),
            ),
            (
                Resource::Disk,
                strings(&["1.11 of 2.00 TB used", "1.11/2.00 TB used", "1.11 TB used"]),
            ),
            (
                Resource::Network,
                strings(&["2.1 of 11.8 MB/s peak", "2.1 of 11.8 MB/s", "2.1 MB/s"]),
            ),
            (
                Resource::Tokens,
                tokens_amount(&ladder_rows(&MAC)[5].1, Ladder::Det),
            ),
            (Resource::Processes, strings(procs)),
        ]
    };
    assert_eq!(said(&MAC), golden(&["897 processes", "897"]));
    assert_eq!(said(&LINUX), golden(&["3,412 threads", "3,412"]));

    let mut slow = row(18.4e3, 200e3);
    slow.basis = CapBasis::Peak;
    assert_eq!(
        glance_run(Resource::Network, &slow, &s, &MAC),
        strings(&["18 of 200 KB/s peak", "18 of 200 KB/s", "18 KB/s"])
    );
    let crowded = row(9_000.0, 16_000.0);
    assert_eq!(
        glance_run(Resource::Processes, &crowded, &s, &MAC),
        strings(&["9,000 processes", "9,000"]),
        "no limit in the run"
    );
    assert!(
        glance_run(Resource::Cpu, &ResourceView::default(), &s, &MAC).is_empty(),
        "an unread figure is left out"
    );
    // aterm's and the tab's figures are never a run: they are the bar's segments.
    let mut cpu = row(17.7, 18.0);
    cpu.aterm = fig(6.8);
    cpu.tab = fig(0.04);
    assert_eq!(
        glance_run(Resource::Cpu, &cpu, &s, &MAC),
        strings(&["17.7 of 18 cores", "17.7 cores"])
    );
}

/// Where the link's own rates are read, the network's run says both directions, and its
/// Details no other direction; where they are not, the run says the busier against its peak.
#[test]
fn the_network_run_says_its_live_rates_where_the_link_reads_them() {
    let mut s = snap();
    let mut net = row(2.1e6, 11.8e6);
    net.basis = CapBasis::Peak;
    net.second = Some(series(0.3e6, 4.2e6, false));
    set(&mut s, Resource::Network, net.clone());
    assert_eq!(
        glance_run(Resource::Network, &net, &s, &MAC),
        strings(&["2.1 of 11.8 MB/s peak", "2.1 of 11.8 MB/s", "2.1 MB/s"]),
        "CONTROL: no link rates"
    );
    s.net = Some(crate::NetView {
        state: NetState::Online,
        rates: Some(crate::LinkRates {
            rx_bps: 4_000.0,
            tx_bps: 3_000.0,
            ..crate::LinkRates::default()
        }),
        ..crate::NetView::default()
    });
    assert_eq!(
        glance_run(Resource::Network, &net, &s, &MAC),
        strings(&["\u{2193}4K \u{2191}3K/s"]),
        "the rates keep their unit"
    );
    assert!(
        glance_details(Resource::Network, &s, &[], &MAC)
            .iter()
            .all(|c| c.forms.iter().all(|f| !f.contains("MB/s"))),
        "the run says the rates"
    );
}

/// The disk's Details says aterm's writes, and the tab's in the same unit, from 1 MB/s: the
/// row's run is the volume's space, and no column says the rates.
#[test]
fn the_disk_says_aterms_writes_from_a_megabyte_a_second() {
    let mut s = snap();
    let mut disk = row(1.11e12, 2e12);
    disk.aterm = fig(210e6);
    disk.tabs = vec![(2, fig(190e6))];
    set(&mut s, Resource::Disk, disk.clone());
    let writes = |s: &Snapshot, panes: &[u64]| -> Vec<Vec<String>> {
        glance_details(Resource::Disk, s, panes, &MAC)
            .into_iter()
            .map(|c| c.forms)
            .filter(|f| f.iter().any(|f| f.starts_with("aterm writes")))
            .collect()
    };
    assert_eq!(
        writes(&s, &[2]),
        [strings(&[
            "aterm writes 210 MB/s, 190 in Tab 2",
            "aterm writes 210 MB/s"
        ])]
    );
    assert_eq!(writes(&s, &[]), [strings(&["aterm writes 210 MB/s"])]);
    disk.aterm = fig(0.6e6);
    set(&mut s, Resource::Disk, disk);
    assert!(writes(&s, &[2]).is_empty(), "under a megabyte a second");
}

/// The glance says a clause only while it is unusual: Spotlight's cause always, the threads
/// waiting once one waits, the busiest program from a core, the other direction from 0.1 MB/s,
/// "yours" from 80% of the user's limit.
#[test]
fn the_glance_says_a_clause_only_while_it_is_unusual() {
    let forms = |res: Resource, s: &Snapshot| -> Vec<Vec<String>> {
        glance_details(res, s, &[], &MAC)
            .into_iter()
            .map(|c| c.forms)
            .collect()
    };
    let mut s = snap();
    set(&mut s, Resource::Cpu, row(3.3, 18.0));
    s.facts.load1 = Some(5.6);
    s.facts.busiest = Some(Busiest {
        name: "siriactionsd".into(),
        cores: 0.7,
        n: 1,
        scope: None,
        born_ms: None,
        unborn: 1,
    });
    assert_eq!(forms(Resource::Cpu, &s), Vec::<Vec<String>>::new(), "calm");
    assert_eq!(
        details(Resource::Cpu, &s, &MAC).len(),
        2,
        "CONTROL: load and busiest"
    );

    s.facts.load1 = Some(25.7);
    s.why = spotlight(Some(Context::Startup));
    s.facts.busiest = Some(Busiest {
        name: "rustc".into(),
        cores: 5.9,
        n: 9,
        scope: Some(ProcScope::Tab(3)),
        born_ms: None,
        unborn: 9,
    });
    assert_eq!(
        forms(Resource::Cpu, &s),
        [
            strings(&["8 waiting for a core"]),
            strings(&["Spotlight, after start-up", "Spotlight"]),
            strings(&["rustc \u{d7}9 in Tab 3", "rustc \u{d7}9"]),
        ]
    );
    assert_eq!(
        glance_details(Resource::Cpu, &s, &[], &LINUX)[0].forms,
        strings(&["load 25.7 on 18 cores", "load 25.7"]),
        "Linux says its load"
    );

    let mut net = row(2.1e6, 11.8e6);
    net.basis = CapBasis::Peak;
    net.second = Some(series(0.3e6, 4.2e6, false));
    set(&mut s, Resource::Network, net.clone());
    let up = glance_details(Resource::Network, &s, &[], &MAC);
    assert_eq!(
        up.iter()
            .map(|c| (c.forms.clone(), c.mark))
            .collect::<Vec<_>>(),
        [(strings(&["0.3 MB/s"]), Some(Mark::Up))]
    );
    net.second = Some(series(0.04e6, 4.2e6, false));
    set(&mut s, Resource::Network, net);
    assert_eq!(
        forms(Resource::Network, &s),
        Vec::<Vec<String>>::new(),
        "quiet"
    );

    let mut p = processes_trouble();
    let marked = glance_details(Resource::Processes, &p, &[], &MAC);
    assert!(
        !marked.is_empty() && marked.iter().all(|c| c.mark.is_some()),
        "the jobs' marks: {marked:?}"
    );
    p.facts.room.user_procs = Some(9_000);
    assert_eq!(
        forms(Resource::Processes, &p).last(),
        Some(&strings(&["yours 9,000 of 10,666", "yours 9,000"]))
    );
}

#[test]
fn the_cpu_brand_the_network_run_and_the_machines_totals_format_compactly() {
    assert_eq!(compact_cpu_brand("Apple M5 Max"), "M5 Max");
    assert_eq!(
        compact_cpu_brand("AMD Ryzen 9 9950X 16-Core Processor"),
        "Ryzen 9 9950X"
    );
    assert_eq!(
        compact_cpu_brand("13th Gen Intel(R) Core(TM) i9-13900K @ 3.00GHz"),
        "Core i9-13900K"
    );
    let mut s = scenario_a();

    s.facts.link = Some(LinkSample {
        name: "en0".into(),
        kind: LinkKind::Wifi,
        up: true,
        counts: true,
        ..LinkSample::default()
    });
    s.net = Some(crate::NetView {
        state: NetState::Online,
        srtt_ms: Some(14.2),
        rates: Some(crate::LinkRates {
            rx_bps: 12_400_000.0,
            tx_bps: 1_800_000.0,
            ..crate::LinkRates::default()
        }),
        ..crate::NetView::default()
    });
    // The network's run says both rates with their unit, never without it.
    assert_eq!(
        net_amount_det_forms(&s),
        ["\u{2193}12.4M \u{2191}1.8M/s".to_owned()]
    );
    // The legend's totals where neither memory nor disk is read: the machine's cores,
    // memory and disk, long then short, each measured by itself.
    s.resources[Resource::Cpu.index()].capacity = Some(18.0);
    s.resources[Resource::Memory.index()].capacity = Some(128.0 * f64::from(1u32 << 30));
    s.resources[Resource::Disk.index()].capacity = Some(2.0e12);
    s.mem = None;
    s.volume = None;
    let said =
        |s: &Snapshot| -> Vec<String> { machine_totals(s).into_iter().map(|t| t.said).collect() };
    assert_eq!(
        said(&s),
        [
            "18 cores \u{b7} 128 GiB \u{b7} 2 TB",
            "18c \u{b7} 128G \u{b7} 2T"
        ]
    );
    assert!(
        machine_totals(&s)
            .iter()
            .all(|t| t.widest == t.said && !t.used)
    );
    s.resources[Resource::Disk.index()].capacity = None;
    assert_eq!(said(&s), ["18 cores \u{b7} 128 GiB", "18c \u{b7} 128G"]);

    fn dst_offset(unix_s: i64) -> Option<i64> {
        // 2026-11-01T09:00:00Z: PDT (-7h) -> PST (-8h).
        Some(if unix_s < 1_793_523_600 {
            -7 * 3600
        } else {
            -8 * 3600
        })
    }
    let dst_loc = Locale {
        platform: Platform::MacOs,
        utc_offset_s: -8 * 3600,
        utc_offset_at: Some(dst_offset),
    };
    let at_pdt_ms = 1_793_518_200_000; // 2026-11-01T00:30:00-07:00 (23:30 Sat under -8h)
    let now_pst_ms = 1_793_529_000_000; // 2026-11-01T02:30:00-08:00
    assert_eq!(dst_loc.clock(at_pdt_ms, now_pst_ms), "00:30");
}

/// A volume that keeps 160 GB of its free blocks from this user: 8 TB, 400 GB of free
/// blocks, 240 GB of them free to this user and 160 GB reserved.
fn reserving_volume() -> VolumeSample {
    VolumeSample {
        total: 8_000_000_000_000,
        free: 240_000_000_000,
        reserved: 160_000_000_000,
        mount: "/".into(),
    }
}

/// Every Disk word says used, the volume's size less what this user can still write
/// (`docs/DESIGN-stats-deck-flow-2026-11-02.md` §3): the row's run and Details, the lens's
/// first line, the legend's totals and speech say `used` and never `unavailable`, and the
/// lens and speech add the blocks the volume keeps back where they are 1 % of it or more.
/// CONTROL: this volume reserves blocks, so a word that took used as size less every free
/// block (`7.60 TB`) would show here.
#[test]
fn every_disk_word_says_used_for_total_less_free() {
    let volume = reserving_volume();
    let occupied = bytes_dec(volume.total - volume.free - volume.reserved);
    assert_eq!(
        occupied, "7.60 TB",
        "CONTROL: size less the free blocks differs"
    );
    let mut s = Snapshot {
        seq: 1,
        volume: Some(volume.clone()),
        ..Snapshot::default()
    };
    let view = row(7.76e12, 8e12);
    s.resources[Resource::Disk.index()] = view.clone();
    let line = disk_space_line(&volume);
    assert_eq!(line, "7.76 TB used \u{b7} 240 GB free \u{b7} 8.00 TB total");
    assert_eq!(disk_reserved(&volume).as_deref(), Some("160 GB reserved"));
    assert_eq!(
        space_line(Resource::Disk, &s).as_deref(),
        Some("7.76 TB used \u{b7} 240 GB free \u{b7} 8.00 TB total \u{b7} 160 GB reserved")
    );
    let mut words: Vec<String> = Vec::new();
    for ladder in [Ladder::Det, Ladder::Cols] {
        words.extend(amount_forms(Resource::Disk, &view, &s.facts, ladder, &MAC));
    }
    assert_eq!(words[0], "7.76 of 8.00 TB used");
    let run = glance_run(Resource::Disk, &view, &s, &MAC);
    assert_eq!(
        run,
        ["7.76 of 8.00 TB used", "7.76/8.00 TB used", "7.76 TB used"]
    );
    words.extend(run);
    words.push(line);
    s.resources[Resource::Cpu.index()].capacity = Some(10.0);
    s.resources[Resource::Disk.index()].capacity = Some(8e12);
    words.extend(machine_totals(&s).into_iter().map(|t| t.said));
    let spoken = row_sentence(Resource::Disk, &s, &[], &MAC);
    assert!(spoken.contains("7.76 terabytes used"), "{spoken}");
    assert!(spoken.contains("160 gigabytes reserved"), "{spoken}");
    words.push(spoken);
    for w in &words {
        assert!(!w.contains("unavailable"), "{w:?}");
        assert!(!w.contains("7.60"), "size less the free blocks: {w:?}");
    }
    // Blocks kept back under 1 % of the volume, or none, go unsaid.
    for reserved in [0, 60_000_000_000] {
        let v = VolumeSample {
            reserved,
            ..reserving_volume()
        };
        assert_eq!(disk_reserved(&v), None, "{reserved}");
    }
}

/// Memory's first line says the reader's available figure ([`MemSample::available`]), apart
/// from total; an unread view says no figure rather than zero. NEGATIVE CONTROL: the classes
/// do not make it, so free plus the file cache (32 GiB here) is not what it says once the
/// reader's figure is 20 GiB.
#[test]
fn memory_words_say_the_readers_available_and_unread_is_not_zero() {
    let mut s = Snapshot {
        mem: Some(MemView {
            sample: MemSample {
                total: 128 << 30,
                app: 80 << 30,
                wired: 12 << 30,
                compressed: Some(4 << 30),
                cached: 24 << 30,
                available: 32 << 30,
                free: 8 << 30,
                ..MemSample::default()
            },
            ..MemView::default()
        }),
        ..Snapshot::default()
    };
    assert_eq!(
        space_line(Resource::Memory, &s).as_deref(),
        Some("96.0 GiB used \u{b7} 32.0 GiB available \u{b7} 128 GiB total")
    );
    let unread = glance_run(Resource::Memory, &ResourceView::default(), &s, &MAC);
    assert!(
        unread.is_empty(),
        "unread capacity must not become zero: {unread:?}"
    );
    s.mem.as_mut().unwrap().sample.available = 20 << 30;
    let line = space_line(Resource::Memory, &s).unwrap_or_default();
    assert!(line.contains("20.0 GiB available"), "{line}");
    assert!(!line.contains("32.0 GiB available"), "{line}");
}

/// The legend's totals say memory and disk as used of total where read, longest first,
/// and the template each is measured by has every used digit an `8`, so it is the same for
/// every reading of the same machine (`docs/DESIGN-stats-deck-flow-2026-11-02.md` §4).
/// CONTROL: the words themselves differ in width between two readings.
#[test]
fn the_legends_totals_say_used_of_total_measured_by_their_widest_figures() {
    let mut s = Snapshot {
        mem: Some(MemView {
            sample: MemSample {
                total: 32 << 30,
                app: 15_461_882_265,
                ..MemSample::default()
            },
            ..MemView::default()
        }),
        volume: Some(VolumeSample {
            total: 4_000_000_000_000,
            free: 1_660_000_000_000,
            reserved: 0,
            mount: "/".into(),
        }),
        ..Snapshot::default()
    };
    s.resources[Resource::Cpu.index()].capacity = Some(10.0);
    s.resources[Resource::Memory.index()].capacity = Some(f64::from(32_u32) * GIB);
    s.resources[Resource::Disk.index()].capacity = Some(4e12);
    let forms = machine_totals(&s);
    let pairs: Vec<(&str, &str)> = forms
        .iter()
        .map(|t| (t.said.as_str(), t.widest.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [
            (
                "10 cores \u{b7} 14.4 of 32 GiB \u{b7} 2.34 of 4 TB",
                "10 cores \u{b7} 88.8 of 32 GiB \u{b7} 8.88 of 4 TB"
            ),
            (
                "10c \u{b7} 14.4/32G \u{b7} 2.34/4T",
                "10c \u{b7} 88.8/32G \u{b7} 8.88/4T"
            ),
            (
                "10 cores \u{b7} 32 GiB \u{b7} 4 TB",
                "10 cores \u{b7} 32 GiB \u{b7} 4 TB"
            ),
            ("10c \u{b7} 32G \u{b7} 4T", "10c \u{b7} 32G \u{b7} 4T"),
        ]
    );
    assert_eq!(
        forms.iter().map(|t| t.used).collect::<Vec<_>>(),
        [true, true, false, false],
        "the used of total, then the totals alone"
    );
    // Another reading of the same machine: other words, the same templates.
    let before: Vec<String> = forms.iter().map(|t| t.widest.clone()).collect();
    s.mem.as_mut().unwrap().sample.app = 3 << 30;
    s.volume.as_mut().unwrap().free = 3_990_000_000_000;
    let after = machine_totals(&s);
    assert_eq!(
        after[0].said,
        "10 cores \u{b7} 3.0 of 32 GiB \u{b7} 0.01 of 4 TB"
    );
    assert_eq!(
        after.iter().map(|t| t.widest.clone()).collect::<Vec<_>>(),
        before
    );
    s.mem.as_mut().unwrap().sample.app = 9 << 30;
    assert_ne!(
        machine_totals(&s)[0].said.len(),
        forms[0].said.len(),
        "CONTROL: the words' own widths differ"
    );
    // A smaller volume's used figure is whole GB; an unread memory says its total alone.
    s.volume = Some(VolumeSample {
        total: 994_000_000_000,
        free: 482_000_000_000,
        reserved: 0,
        mount: "/".into(),
    });
    s.resources[Resource::Disk.index()].capacity = Some(994e9);
    s.mem = None;
    let small = machine_totals(&s);
    assert_eq!(small[0].said, "10 cores \u{b7} 32 GiB \u{b7} 512 of 994 GB");
    assert_eq!(
        small[0].widest,
        "10 cores \u{b7} 32 GiB \u{b7} 888 of 994 GB"
    );
}
