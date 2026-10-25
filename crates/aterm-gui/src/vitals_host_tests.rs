// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vitals host's tests (`crate::vitals_host`).

use super::*;
use crate::harness_host::{DISK_TICK, HostNote};
use crate::vitals_jobs::JobsBoard;
use aterm_agent::harness::footer::FactsOwner;
use aterm_agent::harness::session_usage::{ModelTokens, More, UsageFacts};
use aterm_agent::supervise::BackgroundEvidence;
use aterm_vitals::jobs::{Consumer, Demand, JobRow, JobsView};
use aterm_vitals::model::Resource;
use aterm_vitals::words::{self, Locale, Platform};

#[test]
#[cfg(unix)]
fn volume_reads_use_only_complete_local_mount_roots() {
    use aterm_sysprobe::volumes::{Mount, Table};
    let mount = |on: &str, local, automounted, fstype: &str| Mount {
        on: PathBuf::from(on),
        from: String::new(),
        fstype: fstype.to_owned(),
        local,
        automounted,
        removable: false,
        read_only: false,
        container: None,
    };
    let table = Table {
        mounts: vec![
            mount("/", true, false, "apfs"),
            mount("/System/Volumes/Data", true, false, "apfs"),
            mount("/Volumes/Remote", false, false, "smbfs"),
            mount("/auto", true, true, "apfs"),
            mount("/autofs", true, false, "autofs"),
        ],
        complete: true,
        firmlinks: vec![(
            PathBuf::from("/Users"),
            PathBuf::from("/System/Volumes/Data/Users"),
        )],
    };
    let expected = aterm_sysprobe::VolumeReading {
        total: 100,
        free: 70,
        available: 60,
        mount: PathBuf::from("/System/Volumes/Data"),
        fs: "apfs".to_owned(),
    };
    let mut calls = Vec::new();
    assert_eq!(
        local_volume(Path::new("/Users//a/project/link"), &table, |path| {
            calls.push(path.to_path_buf());
            Some(expected.clone())
        }),
        Some(expected)
    );
    assert_eq!(calls, [PathBuf::from("/System/Volumes/Data")]);
    for path in [
        "/Volumes/Remote/offline",
        "/auto/offline",
        "/autofs/offline",
        "relative",
        "/Users//a/../elsewhere",
    ] {
        assert_eq!(
            local_volume(Path::new(path), &table, |_| panic!("unadmitted {path}")),
            None
        );
    }
    let mut incomplete = table;
    incomplete.complete = false;
    assert_eq!(
        local_volume(Path::new("/Users//a/project"), &incomplete, |_| {
            panic!("an incomplete table cannot rule out a nested remote mount")
        }),
        None
    );
}

#[test]
fn readings_of_every_os_map_to_the_models_classes() {
    let linux = aterm_sysprobe::MemoryDetail {
        total: 32 << 30,
        free: 2 << 30,
        app: 9 << 30,
        shared: Some(1 << 30),
        wired: 1 << 30,
        file_backed: 11 << 30,
        commit: Some(aterm_sysprobe::Commit {
            charge: 18 << 30,
            limit: 24 << 30,
            exact: false,
        }),
        pressure: None,
        ..aterm_sysprobe::MemoryDetail::default()
    };
    let m = mem_sample(linux);
    assert_eq!((m.app, m.cached), (10 << 30, 11 << 30));
    assert_eq!(m.compressed, None, "an unreported class is not a zero");
    assert_eq!(m.pressure, Pressure::Unknown, "no PSI is not normal");
    assert_eq!(
        m.commit,
        Some(CommitSample {
            charge: 18 << 30,
            limit: 24 << 30,
            exact: false
        })
    );
    let mac = aterm_sysprobe::MemoryDetail {
        file_backed: 3 << 30,
        purgeable: Some(1 << 30),
        compressed: Some(2 << 30),
        exact: true,
        ..aterm_sysprobe::MemoryDetail::default()
    };
    let m = mem_sample(mac);
    assert_eq!(
        (m.cached, m.compressed, m.commit),
        (4 << 30, Some(2 << 30), None)
    );

    let gpu = |device_pct, unread| aterm_sysprobe::GpuReading {
        device_pct,
        unread,
        name: Some("amdgpu".into()),
        ..aterm_sysprobe::GpuReading::default()
    };
    let asleep = gpu(None, Some(aterm_sysprobe::Why::Asleep));
    assert_eq!(gpu_sample(asleep).device_pct, Some(0), "asleep is idle");
    let privileged = gpu(
        None,
        Some(aterm_sysprobe::Why::NeedsPrivilege("cap_perfmon")),
    );
    assert_eq!(gpu_sample(privileged).device_pct, None, "unread is not 0");
    assert_eq!(gpu_sample(gpu(Some(37), None)).device_pct, Some(37));
}

#[test]
fn the_new_readings_reach_the_model() {
    let g = gpu_sample(aterm_sysprobe::GpuReading {
        device_pct: Some(12),
        renderer_pct: Some(10),
        tiler_pct: Some(3),
        in_use_bytes: Some(1_700 << 20),
        alloc_bytes: Some(2_048 << 20),
        recoveries: Some(1),
        cores: Some(40),
        ..aterm_sysprobe::GpuReading::default()
    });
    assert_eq!(
        (
            g.in_use_bytes,
            g.alloc_bytes,
            g.recoveries,
            g.renderer_pct,
            g.tiler_pct
        ),
        (
            Some(1_700 << 20),
            Some(2_048 << 20),
            Some(1),
            Some(10),
            Some(3)
        )
    );
    assert_eq!(
        gpu_sample(aterm_sysprobe::GpuReading::default()).recoveries,
        None
    );

    let link = |name: &str, kind, up| aterm_sysprobe::LinkReading {
        name: name.into(),
        kind,
        up,
        rate_bps: Some(183_000_000),
        rx: aterm_sysprobe::LinkCounters {
            bytes: 5_000,
            ..aterm_sysprobe::LinkCounters::default()
        },
        tx: aterm_sysprobe::LinkCounters {
            bytes: 700,
            ..aterm_sysprobe::LinkCounters::default()
        },
        ..aterm_sysprobe::LinkReading::default()
    };
    use aterm_sysprobe::LinkKind as Probe;
    let wifi = link_sample(&link("en0", Probe::Wifi, true));
    assert_eq!(
        wifi,
        LinkSample {
            name: "en0".into(),
            kind: LinkKind::Wifi,
            up: true,
            counts: true,
            rx: 5_000,
            tx: 700,
            tx_drops: if cfg!(target_os = "macos") {
                None
            } else {
                Some(0)
            },
            rate_bps: Some(183_000_000),
            ..LinkSample::default()
        }
    );
    let kinds: Vec<(LinkKind, bool)> = [
        link("en7", Probe::Wired, true),
        link("pdp_ip0", Probe::Cellular, true),
        link("utun3", Probe::Other, true),
        link("bridge100", Probe::Other, true),
        link("lo0", Probe::Other, true),
        link("en1", Probe::Wired, false),
    ]
    .iter()
    .map(|l| {
        let s = link_sample(l);
        (s.kind, s.counts)
    })
    .collect();
    assert_eq!(
        kinds,
        [
            (LinkKind::Ethernet, true),
            (LinkKind::Cellular, true),
            (LinkKind::Vpn, false),
            (LinkKind::Bridge, false),
            (LinkKind::Other, false),
            (LinkKind::Ethernet, false),
        ]
    );
    assert_eq!(
        proc_limits(aterm_sysprobe::ProcessLimits {
            system: Some(16_000),
            per_user: Some(10_666),
        }),
        ProcLimits {
            system: Some(16_000),
            per_user: Some(10_666),
        }
    );
}

#[test]
fn two_readings_give_this_process_a_scope_and_the_machine_its_cores() {
    let mut reader = Reader::new(Arc::default());
    let req = Request {
        self_pid: std::process::id(),
        cadence_ms: 1_000,
        ..Request::default()
    };
    let _ = reader.read(&req);
    let mut x = 0u64;
    for i in 0..2_000_000u64 {
        x = x.wrapping_mul(31).wrapping_add(i);
    }
    assert_ne!(x, 1);
    let snap = reader.read(&req);
    assert!(snap.at_unix_ms > 0, "the wall clock is read");
    assert!(
        snap.facts.disk.is_some(),
        "the disk facts ride on every reading"
    );
    if cfg!(any(
        target_os = "macos",
        target_os = "linux",
        all(windows, target_pointer_width = "64")
    )) {
        let cpu = snap.cpu.as_ref().expect("cores are read");
        assert!(cpu.total() > 0);
        assert!(snap.mem.is_some(), "memory is read");
        assert!(snap.aterm.is_some(), "this process is aterm's root");
    }
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        assert!(snap.facts.load1.is_some(), "the load average is read");
        assert!(
            snap.facts.proc_limits.system.is_some(),
            "the limits are read"
        );
        assert!(
            snap.facts.pid_counts.0.is_some(),
            "the processes are counted"
        );
    }

    let hidden = Request {
        cadence_ms: 10_000,
        machine_only: true,
        ..req
    };
    let snap = reader.read_machine(&hidden);
    assert!(snap.aterm.is_none(), "a hidden reading sweeps no process");
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        assert!(snap.cpu.is_some(), "the machine counters are still read");
        assert!(snap.facts.pid_counts.0.is_some());
    }
}

/// Over one free slot, a full reading's sweep finds no holder in any tab (`Some` of none); a
/// hidden reading, which sweeps no process, attributes nothing (`None`).
#[cfg(target_os = "macos")]
#[test]
fn a_full_reading_attributes_the_slot_holders_and_a_hidden_one_does_not() {
    let dir = aterm_tempfile::tempdir().expect("scratch slots");
    std::fs::write(dir.path().join("slot-0"), b"").expect("a free slot");
    let mut reader = Reader::new(Arc::default());
    reader.slots = aterm_sysprobe::slots::SlotReader::new(dir.path().to_path_buf());
    let req = Request {
        self_pid: std::process::id(),
        cadence_ms: 1_000,
        ..Request::default()
    };
    let snap = reader.read(&req);
    assert!(
        matches!(
            snap.slots,
            Some(aterm_vitals::model::SlotsView::Read { n: 1, .. })
        ),
        "{:?}",
        snap.slots
    );
    assert_eq!(snap.slot_tabs, Some(vec![]));
    let hidden = reader.read_machine(&Request {
        machine_only: true,
        ..req
    });
    assert_eq!(hidden.slot_tabs, None);
}

#[test]
fn a_busy_reader_keeps_its_pending_messages_and_forgoes_newer_work() {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let mut host = VitalsHost {
        tx: Some(tx),
        tried: true,
        ..VitalsHost::default()
    };
    let request = |pid| {
        Msg::Read(Box::new(Request {
            self_pid: pid,
            ..Request::default()
        }))
    };
    let pid_of = |m: Msg| match m {
        Msg::Read(r) | Msg::Facts(r) => r.self_pid,
        Msg::Pull => 0,
    };
    assert!(host.offer(request(1)));
    assert!(
        !host.offer(request(2)),
        "a full queue forgoes the newer one"
    );
    assert_eq!(pid_of(rx.try_recv().expect("one pending reading")), 1);
    assert!(matches!(
        rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    assert!(host.offer(request(3)));
    assert_eq!(
        pid_of(rx.try_recv().expect("room for the current reading")),
        3
    );
    drop(rx);
    assert!(!host.offer(request(4)));
    assert!(host.tx.is_none(), "a reader that ended is not asked again");
    assert!(!host.offer(request(5)));
    assert!(host.tx.is_none());
    assert!(host.tried, "no reader is restarted after disconnect");
}

#[test]
fn hidden_decks_read_the_machine_every_ten_seconds() {
    assert_eq!(Audience::of(false, true, true), Audience::Hidden);
    assert_eq!(Audience::of(false, true, false), Audience::Hidden);
    assert_eq!(Audience::of(true, true, true), Audience::Shown);
    assert_eq!(Audience::of(true, true, false), Audience::Background);
    assert_eq!(
        Audience::of(true, false, true),
        Audience::Shown,
        "the Monitor tab alone"
    );
    assert_eq!(Audience::of(false, false, true), Audience::None);

    for lens in [false, true] {
        assert_eq!(Audience::Hidden.cadence(lens), Some((HIDDEN_PERIOD, true)));
        assert_eq!(Audience::None.cadence(lens), None);
    }
    let lens = crate::stats_lens::LENS_PERIOD;
    assert_eq!(
        Audience::Shown.cadence(false),
        Some((FOCUSED_PERIOD, false))
    );
    assert_eq!(
        Audience::Background.cadence(false),
        Some((BACKGROUND_PERIOD, false))
    );
    assert_eq!(Audience::Shown.cadence(true), Some((lens, false)));
    assert_eq!(Audience::Background.cadence(true), Some((lens, false)));
    assert_eq!(HIDDEN_PERIOD, Duration::from_secs(10));

    let t0 = Instant::now();
    let s = Duration::from_secs;
    assert_eq!(
        due_at(None, None, HIDDEN_PERIOD, t0),
        t0,
        "the first at once"
    );
    assert_eq!(
        due_at(Some(t0), Some(t0 + HIDDEN_PERIOD), HIDDEN_PERIOD, t0 + s(3)),
        t0 + HIDDEN_PERIOD
    );
    let back = t0 + s(3);
    assert!(due_at(Some(t0), Some(t0 + HIDDEN_PERIOD), FOCUSED_PERIOD, back) <= back);
    assert_eq!(
        due_at(Some(t0), Some(t0), FOCUSED_PERIOD, t0 + s(1) / 10),
        t0
    );
}

#[test]
fn peaks_round_trip_through_the_json_file() {
    let dir = aterm_tempfile::tempdir().expect("scratch state");
    let path = dir.path().join(PEAKS_FILE);
    let reopen = || PeakFile::open(Some(path.clone()), Instant::now()).1;
    let t0 = Instant::now();
    let (mut file, restored) = PeakFile::open(Some(path.clone()), t0);
    assert_eq!(restored, Peaks::default(), "no file, no peaks");

    let mut peaks = Peaks::default();
    for (day, v) in [(20_380, 2.5e6), (20_381, 1.1e7), (20_381, 3.25e6)] {
        peaks.net_rx.note_bucket(day, v);
    }
    peaks.net_tx.note_bucket(20_381, 412_345.75);
    peaks.tokens.note_bucket(20_381, 9_012.5);
    let (a, b) = (account("aaaaaaaa-0000"), account("bbbbbbbb-0000"));
    peaks.note_plan(&a, plan_day(20_381, 10, 412));
    peaks.note_plan(&b, plan_day(20_380, 22, 87));
    peaks.note_plan(&b, plan_day(20_381, 9, 95));
    file.keep(&peaks, t0 + PEAKS_EVERY / 2, false);
    assert!(!path.exists(), "within ten minutes of the start: not yet");
    file.keep(&peaks, t0 + PEAKS_EVERY, false);
    assert_eq!(reopen(), peaks, "the file gives the peaks back");
    assert_eq!(reopen().plan.len(), 2, "and both accounts' weeks");

    let mut more = peaks.clone();
    more.tokens.note_bucket(20_382, 12_000.0);
    more.note_plan(&a, plan_day(20_382, 8, 450));
    file.keep(&more, t0 + PEAKS_EVERY + PEAKS_EVERY / 2, false);
    assert_eq!(reopen(), peaks, "within ten minutes of the last write");
    file.keep(&more, t0 + PEAKS_EVERY + PEAKS_EVERY / 2, true);
    assert_eq!(reopen(), more, "the thread's stop writes at once");
    std::fs::write(&path, "kept").expect("a stand-in");
    file.keep(&more, t0 + 10 * PEAKS_EVERY, true);
    assert_eq!(
        std::fs::read_to_string(&path).expect("readable"),
        "kept",
        "unchanged peaks are not written"
    );

    for text in ["kept", "{", r#"{"schema":2,"peaks":[]}"#] {
        std::fs::write(&path, text).expect("a damaged file");
        assert_eq!(reopen(), Peaks::default(), "{text}");
    }
    let text = peaks_json(&more).expect("text");
    std::fs::write(
        &path,
        format!("{text}{}", " ".repeat(PEAKS_MAX_BYTES as usize)),
    )
    .expect("big");
    assert_eq!(reopen(), Peaks::default(), "a file over the bound");
    std::fs::write(&path, &text).expect("small");
    assert_eq!(reopen(), more, "control: the same peaks within the bound");
    assert_eq!(PeakFile::open(None, t0).1, Peaks::default());
}

fn account(id: &str) -> String {
    AccountKey::claude(id).expect("a key").to_string()
}

/// A reading of the weekly figure at `hour` UTC on `day`, in a window that resets 3 days on.
fn plan_day(day: u32, hour: i64, used_x10: u16) -> PlanDay {
    let at = i64::from(day) * 86_400 + hour * 3_600;
    PlanDay {
        day,
        used_x10,
        resets_at: (i64::from(day) + 3) * 86_400,
        at,
    }
}

/// The text the peaks file held before it kept the plan (schema 1).
const PEAKS_BEFORE_PLAN: &str = r#"{"schema":1,"peaks":[{"name":"net_rx","read_s":86400,"days":[[20380,2500000.0],[20381,11000000.0]]},{"name":"net_tx","read_s":86400,"days":[[20381,412345.75]]},{"name":"tokens","read_s":3000,"days":[[20381,9012.5]]}]}"#;

#[test]
fn a_peaks_file_from_before_the_plan_still_reads() {
    let peaks = peaks_of(PEAKS_BEFORE_PLAN).expect("a schema-1 file");
    assert_eq!(peaks.net_rx.peak(20_381), Some(11_000_000.0));
    assert_eq!(peaks.net_tx.peak(20_381), Some(412_345.75));
    assert_eq!(
        (peaks.tokens.peak(20_381), peaks.tokens.read_s),
        (Some(9_012.5), 3000)
    );
    assert!(peaks.plan.is_empty());

    let a = account("aaaaaaaa-0000");
    let with = |plan: &str| {
        let text = PEAKS_BEFORE_PLAN
            .strip_suffix('}')
            .map(|head| format!("{head},\"plan\":{plan}}}"))
            .expect("an object");
        peaks_of(&text).expect("the peaks")
    };
    let kept = with(&format!(
        r#"[{{"account":"{a}","days":[[20381,412,1761177600,1760954400]]}}]"#
    ));
    assert_eq!(
        (kept.net_rx.clone(), kept.plan.len()),
        (peaks.net_rx.clone(), 1),
        "control: a plan row reads"
    );
    for plan in [
        r#"{"weeks":[]}"#.to_owned(),
        r#"[{"account":"c-0","days":[[20381,412,1761177600,1760954400]]}]"#.to_owned(),
        format!(r#"[{{"account":"{a}","days":[[20381,412,1761177600]]}}]"#),
        format!(r#"[{{"account":"{a}","days":[[20381,70000,1761177600,1760954400]]}}]"#),
        format!(r#"[{{"account":"{a}","days":[[20381,1001,1761177600,1760954400]]}}]"#),
    ] {
        let read = with(&plan);
        assert_eq!(
            (read.plan.len(), &read.tokens),
            (0, &peaks.tokens),
            "{plan}: no week, the peaks kept"
        );
    }
}

#[test]
fn the_plan_rows_leave_an_older_reader_its_peaks() {
    #[derive(serde::Deserialize)]
    struct OlderDoc {
        schema: u32,
        peaks: Vec<PeakDoc>,
    }
    let mut peaks = peaks_of(PEAKS_BEFORE_PLAN).expect("a schema-1 file");
    peaks.note_plan(&account("aaaaaaaa-0000"), plan_day(20_381, 10, 412));
    let text = peaks_json(&peaks).expect("text");
    assert!(text.contains("\"plan\""), "{text}");
    let older: OlderDoc = aterm_json::from_str(&text).expect("an older build reads the file");
    assert_eq!(older.schema, PEAKS_SCHEMA);
    let back = Peaks::from_pairs(
        older
            .peaks
            .iter()
            .map(|p| (p.name.as_str(), p.read_s, p.days.as_slice())),
    );
    assert_eq!(
        back,
        Peaks {
            plan: Vec::new(),
            ..peaks
        },
        "with every peak"
    );
}

#[test]
fn the_fullest_peaks_file_fits_its_bound() {
    use aterm_vitals::peaks::{PEAK_DAYS, PLAN_ACCOUNTS, PLAN_ROWS};
    let widest = || aterm_vitals::DailyMax {
        days: (0..PEAK_DAYS)
            .rev()
            .map(|age| (u32::MAX - age, f64::MIN_POSITIVE))
            .collect(),
        read_s: u32::MAX,
    };
    let mut fullest = Peaks {
        net_rx: widest(),
        net_tx: widest(),
        tokens: widest(),
        plan: Vec::new(),
    };
    for n in 0..PLAN_ACCOUNTS {
        let key = account(&format!("{n:08}-0000-0000-0000-000000000000"));
        for age in 0..PEAK_DAYS {
            for window in [0, 7_200] {
                let read = PlanDay {
                    day: u32::MAX - age,
                    used_x10: 1000,
                    resets_at: i64::MAX - window,
                    at: i64::MIN + i64::from(age),
                };
                fullest.note_plan(&key, read);
            }
        }
    }
    assert_eq!(fullest.plan.len(), PLAN_ACCOUNTS);
    assert!(fullest.plan.iter().all(|week| week.days.len() == PLAN_ROWS));
    let text = peaks_json(&fullest).expect("text");
    assert!(
        text.len() as u64 <= PEAKS_MAX_BYTES,
        "{} bytes over the {PEAKS_MAX_BYTES}-byte bound reads as no peaks at all",
        text.len()
    );
    let dir = aterm_tempfile::tempdir().expect("scratch state");
    let path = dir.path().join(PEAKS_FILE);
    std::fs::write(&path, &text).expect("the file");
    assert_eq!(PeakFile::open(Some(path), Instant::now()).1, fullest);
}

#[test]
fn week_facts_place_a_reading_on_its_local_day() {
    const DAY: u32 = 20_381;
    let midnight = i64::from(DAY) * 86_400;
    let row = |hour: i64| WeekRow {
        account: AccountKey::claude("aaaaaaaa-0000").expect("a key"),
        front: true,
        figure: Some(WeeklyFigure {
            used_x10: 412,
            resets_at: midnight + 3 * 86_400,
            at: midnight + hour * 3_600,
        }),
    };
    let day = |hour: i64, offset_at: OffsetProbe| {
        week_facts(&[row(hour)], offset_at)[0]
            .reading
            .map(|r| r.day)
    };
    assert_eq!(
        day(3, |_| Some(-7 * 3_600)),
        Some(DAY - 1),
        "03:00 UTC is the evening before at UTC-7"
    );
    assert_eq!(
        day(20, |_| Some(9 * 3_600)),
        Some(DAY + 1),
        "20:00 UTC is the next morning at UTC+9"
    );
    assert_eq!(day(20, |_| None), Some(DAY), "an unread zone counts in UTC");
    assert_eq!(local_day(midnight - 1, |_| Some(0)), DAY - 1);

    let facts = week_facts(&[row(10)], |_| Some(0));
    assert_eq!(
        facts,
        [WeekFact {
            account: account("aaaaaaaa-0000"),
            front: true,
            reading: Some(PlanDay {
                day: DAY,
                used_x10: 412,
                resets_at: midnight + 3 * 86_400,
                at: midnight + 10 * 3_600,
            }),
        }]
    );
    let unread = WeekRow {
        figure: None,
        ..row(10)
    };
    let wide = WeekRow {
        figure: row(10).figure.map(|f| WeeklyFigure {
            used_x10: 70_000,
            ..f
        }),
        ..row(10)
    };
    assert!(
        week_facts(&[unread, wide], |_| Some(0))
            .iter()
            .all(|f| f.reading.is_none()),
        "an account with no figure, a figure no reading holds"
    );
}

#[test]
fn each_account_once_keeps_the_newest_and_the_front() {
    let (a, b) = (
        AccountKey::claude("aaaaaaaa-0000").expect("a key"),
        AccountKey::claude("bbbbbbbb-0000").expect("a key"),
    );
    let fig = |used_x10: u32, at: i64| {
        Some(WeeklyFigure {
            used_x10,
            resets_at: 1_761_177_600,
            at,
        })
    };
    let row = |account, front, figure| WeekRow {
        account,
        front,
        figure,
    };
    let rows = each_account_once(vec![
        row(b, false, fig(95, 1_760_950_000)),
        row(a, false, fig(410, 1_760_954_000)),
        row(b, true, None),
        row(a, false, fig(412, 1_760_954_400)),
        row(b, false, fig(90, 1_760_940_000)),
    ]);
    assert_eq!(
        rows,
        [
            row(b, true, fig(95, 1_760_950_000)),
            row(a, false, fig(412, 1_760_954_400)),
        ]
    );
}

/// A request's week rows reach the model ([`Reader::read`], [`Reader::read_machine`]) on the day
/// the reader's zone probe gives, for each figure and the reading itself, full or hidden, and the
/// reading's facts carry the front account's run.
#[test]
fn a_reading_carries_the_front_accounts_plan_week() {
    const AHEAD: i64 = 3 * 86_400;
    let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX));
    let resets_at = now + 3 * 86_400;
    let day_of = |unix: i64| u32::try_from((unix + AHEAD).div_euclid(86_400)).expect("a day");
    let row = |id: &str, front, used_x10, at| WeekRow {
        account: AccountKey::claude(id).expect("a key"),
        front,
        figure: Some(WeeklyFigure {
            used_x10,
            resets_at,
            at,
        }),
    };
    let mut reader = Reader::new(Arc::default());
    reader.offset_at = |_| Some(AHEAD);
    let req = Request {
        self_pid: std::process::id(),
        cadence_ms: 1_000,
        weeks: vec![
            row("bbbbbbbb-0000", false, 610, now - 120),
            row("aaaaaaaa-0000", true, 340, now - 60),
        ],
        ..Request::default()
    };
    let snap = reader.read(&req);
    let run = snap.facts.plan_week.expect("the front account's run");
    assert_eq!(
        (run.week.account, run.front, run.today),
        (
            account("aaaaaaaa-0000"),
            true,
            day_of(snap.at_unix_ms.div_euclid(1000))
        ),
        "the front account's, its right end the reading's day in the probe's zone"
    );
    assert_eq!(
        run.week.days,
        [PlanDay {
            day: day_of(now - 60),
            used_x10: 340,
            resets_at,
            at: now - 60,
        }]
    );
    assert_eq!(
        reader.vitals.peaks().plan.len(),
        2,
        "both accounts' weeks are kept"
    );

    let hidden = Request {
        cadence_ms: 10_000,
        machine_only: true,
        weeks: vec![row("aaaaaaaa-0000", true, 345, now - 30)],
        ..req
    };
    let snap = reader.read_machine(&hidden);
    let run = snap.facts.plan_week.expect("a hidden reading's run");
    assert_eq!(
        run.week.days.last().map(|d| (d.used_x10, d.at)),
        Some((345, now - 30)),
        "a hidden reading notes the newer figure"
    );
}

const FREE_MOUNT: &str = "/System/Volumes/Data";
const FREE_SIZE: u64 = 994_662_584_320;

#[test]
fn the_free_history_round_trips_through_the_json_file() {
    let dir = aterm_tempfile::tempdir().expect("scratch state");
    let path = dir.path().join(FREE_FILE);
    let at0: i64 = 1_791_936_000_000;
    let hour = 3_600_000;
    let now = at0 + 3 * hour;
    let reopen = |now_ms| FreeFile::open(Some(path.clone()), Instant::now(), now_ms).1;
    let t0 = Instant::now();
    let (mut file, restored) = FreeFile::open(Some(path.clone()), t0, now);
    assert_eq!(restored, FreeHistory::default(), "no file, no history");

    let mut day = FreeHistory::new();
    for m in 0..3 * 60 {
        day.note_on(
            FREE_MOUNT,
            FREE_SIZE,
            at0 + m * 60_000,
            412_345_678_901 - m as u64 * 1_000_000,
        );
    }
    file.keep(&day, t0 + PEAKS_EVERY / 2, false);
    assert!(!path.exists(), "within ten minutes of the start: not yet");
    file.keep(&day, t0 + PEAKS_EVERY, false);
    let back = reopen(now);
    assert_eq!(
        back.window(now),
        day.window(now),
        "the file gives the hours back"
    );
    assert_eq!(back.saved(), day.saved());
    assert_eq!(
        back.volume(),
        Some(&VolumeId {
            mount: FREE_MOUNT.into(),
            total: FREE_SIZE
        })
    );

    let mut more = day.clone();
    more.note_on(FREE_MOUNT, FREE_SIZE, now, 1);
    file.keep(&more, t0 + PEAKS_EVERY + PEAKS_EVERY / 2, false);
    assert_eq!(
        reopen(now).saved(),
        day.saved(),
        "within ten minutes of the last write"
    );
    file.keep(&more, t0 + PEAKS_EVERY + PEAKS_EVERY / 2, true);
    assert_eq!(
        reopen(now).saved(),
        more.saved(),
        "the thread's stop writes at once"
    );
    std::fs::write(&path, "kept").expect("a stand-in");
    file.keep(&more, t0 + 10 * PEAKS_EVERY, true);
    assert_eq!(
        std::fs::read_to_string(&path).expect("readable"),
        "kept",
        "an unchanged history is not written"
    );

    let text = free_json(&more).expect("text");
    std::fs::write(&path, &text).expect("the history");
    let later = at0 + 160 * 600_000;
    assert_eq!(
        read_of(&reopen(later), later),
        [412_345_678_901 - 179_000_000, 1],
        "only the last 24 hours are kept"
    );
    assert_eq!(
        reopen(now + 25 * hour),
        FreeHistory::default(),
        "a day and more after the last write"
    );
    let peaks = peaks_json(&Peaks::default()).expect("peaks");
    for damaged in [
        "kept",
        "{",
        "",
        r#"{"schema":2,"mount":"/","total":1,"buckets":[[1791936000000,5,1]]}"#,
        r#"{"schema":1,"mount":"/","total":1,"buckets":[[1791936000000,"5",1]]}"#,
        r#"{"schema":1,"mount":"/","total":-1,"buckets":[[1791936000000,5,1]]}"#,
        r#"{"schema":1,"mount":"/","buckets":[[1791936000000,5,1]]}"#,
        &peaks,
    ] {
        std::fs::write(&path, damaged).expect("a damaged file");
        assert_eq!(reopen(now), FreeHistory::default(), "{damaged}");
    }
    std::fs::write(
        &path,
        format!("{text}{}", " ".repeat(PEAKS_MAX_BYTES as usize)),
    )
    .expect("big");
    assert_eq!(reopen(now), FreeHistory::default(), "a file over the bound");
    std::fs::write(&path, &text).expect("small");
    assert_eq!(
        reopen(now).saved(),
        more.saved(),
        "control: within the bound"
    );
    assert_eq!(FreeFile::open(None, t0, now).1, FreeHistory::default());

    let ten = aterm_vitals::history::FREE_BUCKET_S * 1000;
    let first = i64::MIN / ten * ten;
    let widest = SavedFree {
        volume: VolumeId {
            mount: "\u{1}".repeat(1024),
            total: u64::MAX,
        },
        rows: (0..aterm_vitals::history::FREE_BUCKETS as i64)
            .map(|k| (first + k * ten, u64::MAX, u32::MAX))
            .collect(),
    };
    let full = FreeHistory::restored(&widest, first + 143 * ten);
    assert_eq!(full.saved(), Some(widest), "a full day");
    let wide = free_json(&full).expect("text");
    assert!(
        wide.len() as u64 <= PEAKS_MAX_BYTES,
        "a full day of the widest rows fits the read bound: {}",
        wide.len()
    );
}

fn read_of(h: &FreeHistory, now_ms: i64) -> Vec<u64> {
    h.window(now_ms).into_iter().flatten().collect()
}

struct Fills {
    vitals: aterm_vitals::Vitals,
}

impl Source for Fills {
    fn reading(&mut self, _req: &Request) -> Snapshot {
        self.vitals.absorb(&Sample {
            at_unix_ms: unix_ms_now(),
            volume: Some(VolumeSample {
                total: FREE_SIZE,
                free: 7_000,
                available: 7_000,
                mount: FREE_MOUNT.into(),
            }),
            ..Sample::default()
        })
    }

    fn model(&mut self) -> &mut aterm_vitals::Vitals {
        &mut self.vitals
    }
}

#[test]
fn the_probe_thread_reads_on_from_the_kept_history_and_writes_it_at_quit() {
    let dir = aterm_tempfile::tempdir().expect("scratch state");
    let path = dir.path().join(FREE_FILE);
    let mut earlier = FreeHistory::new();
    earlier.note_on(FREE_MOUNT, FREE_SIZE, unix_ms_now() - 3_600_000, 9_000);
    std::fs::write(&path, free_json(&earlier).expect("text")).expect("an earlier run's file");
    let mut host = VitalsHost::default();
    host.start(
        || Fills {
            vitals: aterm_vitals::Vitals::new(),
        },
        || true,
        Some(dir.path().join(PEAKS_FILE)),
        None,
    );
    assert!(host.offer(Msg::Read(Box::default())));
    assert!(
        host.stop_at_exit(Duration::from_secs(60)),
        "the thread ended"
    );
    let now = unix_ms_now();
    let kept = FreeFile::open(Some(path), Instant::now(), now).1;
    assert_eq!(read_of(&kept, now), [9_000, 7_000], "an hour ago, then now");
}

struct Moves {
    vitals: aterm_vitals::Vitals,
    peaks: Peaks,
    ended: Option<std::sync::mpsc::Sender<()>>,
}

impl Drop for Moves {
    fn drop(&mut self) {
        if let Some(ended) = self.ended.take() {
            let _ = ended.send(());
        }
    }
}

impl Source for Moves {
    fn reading(&mut self, _req: &Request) -> Snapshot {
        self.vitals.restore_peaks(self.peaks.clone());
        Snapshot::default()
    }

    fn model(&mut self) -> &mut aterm_vitals::Vitals {
        &mut self.vitals
    }
}

#[test]
fn the_quit_writes_the_peaks_that_moved() {
    let dir = aterm_tempfile::tempdir().expect("scratch state");
    let path = dir.path().join(PEAKS_FILE);
    let mut peaks = Peaks::default();
    peaks.tokens.note_bucket(20_381, 9_012.5);
    peaks.note_plan(&account("aaaaaaaa-0000"), plan_day(20_381, 10, 412));
    let mut host = VitalsHost::default();
    let moved = peaks.clone();
    host.start(
        move || Moves {
            vitals: aterm_vitals::Vitals::new(),
            peaks: moved,
            ended: None,
        },
        || true,
        Some(path.clone()),
        None,
    );
    host.offer(Msg::Read(Box::default()));
    assert!(
        host.stop_at_exit(Duration::from_secs(60)),
        "the thread ended"
    );
    assert!(host.adopt(Instant::now()), "its reading reached the slot");
    assert_eq!(PeakFile::open(Some(path.clone()), Instant::now()).1, peaks);
    assert!(host.tx.is_none() && host.thread.is_none());

    let mut idle = VitalsHost::default();
    assert!(idle.stop_at_exit(Duration::ZERO), "nothing to wait for");

    static LATE_COPY: std::sync::LazyLock<crate::sys_host::Shared> =
        std::sync::LazyLock::new(crate::sys_host::Shared::new);
    let late_path = dir.path().join("late-peaks.json");
    let mut late = VitalsHost::default();
    let (entered, ready) = std::sync::mpsc::channel();
    let (release, waiting) = std::sync::mpsc::channel();
    let (ended, finished) = std::sync::mpsc::channel();
    let moved = peaks.clone();
    late.start(
        move || {
            entered.send(()).expect("construction entered");
            waiting
                .recv_timeout(Duration::from_secs(60))
                .expect("construction released");
            Moves {
                vitals: aterm_vitals::Vitals::new(),
                peaks: moved,
                ended: Some(ended),
            }
        },
        || true,
        Some(late_path.clone()),
        Some(&LATE_COPY),
    );
    ready
        .recv_timeout(Duration::from_secs(60))
        .expect("the reader is being constructed");
    assert!(late.offer(Msg::Read(Box::default())));
    assert!(
        !late.stop_at_exit(Duration::ZERO),
        "the deliberately blocked constructor cannot have ended"
    );
    release
        .send(())
        .expect("finish construction after the quit");
    finished
        .recv_timeout(Duration::from_secs(60))
        .expect("the late reader drained and ended");
    assert!(
        late.adopt(Instant::now()),
        "the queued reading still landed"
    );
    assert_eq!(PeakFile::open(Some(late_path), Instant::now()).1, peaks);
}

fn usage(output: u64, complete: bool) -> UsageFacts {
    UsageFacts {
        models: vec![
            ModelTokens {
                label: "opus".into(),
                input: 400_000,
                output,
            },
            ModelTokens {
                label: "sonnet".into(),
                input: 1_000,
                output: 20,
            },
        ],
        more: More {
            models: 2,
            input: 5,
            output: 7,
        },
        unread: 1,
        unread_more: true,
        complete,
        ..UsageFacts::default()
    }
}

fn footer(session: Option<&str>, usage: Option<UsageFacts>) -> FooterFacts {
    FooterFacts {
        usage,
        owner: session.map(|s| FactsOwner {
            floor: None,
            session_id: s.to_owned(),
        }),
        ..FooterFacts::default()
    }
}

#[test]
fn token_rows_are_one_per_session() {
    let tabs = [
        (11, footer(Some("s1"), Some(usage(3_000, false)))),
        (12, footer(Some("s1"), Some(usage(9_999, true)))),
        (13, footer(Some("s2"), Some(usage(50, true)))),
        (14, footer(None, Some(usage(50, true)))),
        (15, footer(Some("s3"), None)),
    ];
    let read = Instant::now();
    let rows = each_session_once(
        tabs.iter()
            .filter_map(|(key, facts)| tab_tokens(*key, facts, Some(read)))
            .collect(),
    );
    assert_eq!(
        rows,
        [
            TokenRow {
                totals: TabTokens {
                    key: 11,
                    session: "s1".into(),
                    out_total: 3_027,
                    in_total: 401_005,
                    unread: 1,
                    unread_more: true,
                    complete: false,
                    read_ms: None,
                },
                read_at: Some(read),
            },
            TokenRow {
                totals: TabTokens {
                    key: 13,
                    session: "s2".into(),
                    out_total: 77,
                    in_total: 401_005,
                    unread: 1,
                    unread_more: true,
                    complete: true,
                    read_ms: None,
                },
                read_at: Some(read),
            },
        ]
    );
    let epoch = read + Duration::from_millis(1500);
    assert_eq!(mono_ms_of(read + Duration::from_secs(3), read), 3_000);
    assert_eq!(mono_ms_of(read, epoch), -1_500);
}

fn publish(app: &crate::App, session: u64, id: &str, plan: Option<PlanUsage>, read: Instant) {
    let entry = app.pool.get(session).expect("session");
    let mut tl = entry
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    tl.note_foreground_group(4242);
    let facts = FooterFacts {
        plan,
        subscription: plan.map(|p| aterm_agent::harness::profile::SubscriptionDisplay {
            account: Some(p.account),
            weekly_figure: Some(WeeklyFigure {
                used_x10: u32::from(p.used_x10),
                resets_at: p.resets_at,
                at: p.read_at,
            }),
            ..Default::default()
        }),
        ..footer(Some(id), Some(usage(7_000, true)))
    };
    assert!(tl.set_claude_footer(4242, None, Some(facts)));
    tl.note_claude_footer_read(4242, read);
    assert_eq!(tl.claude_footer_read_at(), Some(read));
}

#[test]
fn every_pane_counts_and_the_plan_is_the_frontmost_windows() {
    let mut app = crate::App::headless_for_test();
    let w0 = crate::WindowId(0);
    let v0 = app.windows[&w0].tab_set.active().expect("a tab").focus;
    let split = app.split_active_stub_tab(w0);
    assert!(
        app.focus_pane_in(w0, crate::pane::FocusDir::Left)
            || app.focus_pane_in(w0, crate::pane::FocusDir::Up)
    );
    assert_eq!(app.windows[&w0].tab_set.active().expect("a tab").focus, v0);
    let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).expect("seconds"));
    let plan = |id: &str, used_x10| PlanUsage {
        account: AccountKey::claude(id).expect("a key"),
        window: Bucket::SevenDay,
        used_x10,
        resets_at: now + 3_600,
        read_at: now - 60,
    };
    let read = Instant::now();
    publish(
        &app,
        split,
        "s-split",
        Some(plan("aaaaaaaa-0000", 340)),
        read,
    );
    let s1 = app.next_session_id;
    let w1 = app.insert_logical_window(crate::stub_session(s1), 24, 80);
    publish(&app, s1, "s-other", Some(plan("bbbbbbbb-0000", 610)), read);
    for ws in app.windows.values_mut() {
        ws.focused = false;
    }

    app.frontmost_window = Some(w0);
    let req = app.vitals_request(FOCUSED_PERIOD, false);
    let rows: Vec<(u64, &str, Option<Instant>)> = req
        .tokens
        .iter()
        .map(|r| (r.totals.key, r.totals.session.as_str(), r.read_at))
        .collect();
    assert_eq!(
        rows,
        [(0, "s-split", Some(read)), (s1, "s-other", Some(read))],
        "the split pane's agent is its tab's"
    );
    let front = req.plan.as_ref().expect("the first window's plan");
    assert_eq!((front.used_x10, front.account), (340, AccountIx(0)));
    let weeks = |req: &Request| -> Vec<(String, bool, Option<u32>)> {
        req.weeks
            .iter()
            .map(|w| (w.account.to_string(), w.front, w.figure.map(|f| f.used_x10)))
            .collect()
    };
    let (a, b) = (account("aaaaaaaa-0000"), account("bbbbbbbb-0000"));
    assert_eq!(
        weeks(&req),
        [(a.clone(), true, Some(340)), (b.clone(), false, Some(610))],
        "every pane's account, the front tab's marked"
    );

    app.frontmost_window = Some(w1);
    let req = app.vitals_request(FOCUSED_PERIOD, false);
    let front = req.plan.as_ref().expect("the frontmost window's plan");
    assert_eq!((front.used_x10, front.account), (610, AccountIx(1)));
    assert_eq!(weeks(&req), [(a, false, Some(340)), (b, true, Some(610))]);
}

#[test]
fn the_front_tabs_plan_is_a_dated_fact_while_current() {
    let now = 1_792_050_000;
    let a = AccountKey::claude("aaaaaaaa-0000").expect("a key");
    let b = AccountKey::claude("bbbbbbbb-0000").expect("a key");
    let plan = PlanUsage {
        account: b,
        window: Bucket::SevenDayOpus,
        used_x10: 612,
        resets_at: now + 3_600,
        read_at: now - 120,
    };
    assert_eq!(
        plan_fact(&plan, &[a, b], now),
        Some(PlanFact {
            window_word: "Opus weekly".into(),
            used_x10: 612,
            read_at: (now - 120) * 1000,
            account: AccountIx(1),
        })
    );
    for (bucket, word) in [
        (Bucket::FiveHour, "5-hour"),
        (Bucket::SevenDay, "weekly"),
        (Bucket::SevenDaySonnet, "Sonnet weekly"),
        (Bucket::Fable, "Fable weekly"),
    ] {
        let p = PlanUsage {
            window: bucket,
            ..plan
        };
        assert_eq!(plan_fact(&p, &[b], now).expect("current").window_word, word);
    }
    assert_eq!(plan_fact(&plan, &[a, b], now - 120 + 15 * 60 + 1), None);
    assert_eq!(plan_fact(&plan, &[a, b], now + 3_600), None);
}

const GB: u64 = 1_000_000_000;

fn watched_board() -> (Shelf, VolumeKey) {
    use aterm_agent::harness::disk::{Hog as PassHog, Thresholds};
    let key = VolumeKey::Container("disk3".to_owned());
    let shelf = Shelf::new();
    let thresholds = Thresholds {
        warn: 69 * GB,
        floor: 34 * GB,
        target: 50 * GB,
        emergency: 8 * GB,
        files_emergency: 10_000,
    };
    shelf.publish(|b| {
        b.watched = true;
        b.volumes.push(crate::disk_board::VolumeRow {
            reading: aterm_sysprobe::volumes::Reading {
                mount: PathBuf::from("/System/Volumes/Data"),
                key: key.clone(),
                total: 2_000 * GB,
                free: 41 * GB,
                files_free: Some(1 << 30),
                dev: 1,
                read_only: false,
            },
            thresholds,
            level: thresholds.level(Some(41 * GB), Some(1 << 30)),
            room: None,
            as_of: Instant::now(),
            as_of_unix: 0,
            reclaimed_here: true,
        });
        b.episode.stuck.insert("s1".to_owned(), "ENOSPC".to_owned());
        b.last_pass = Some(crate::disk_board::PassSummary {
            key: key.clone(),
            ended_at: Instant::now(),
            free_after: None,
            tiers: vec![1],
            freed: 0,
            demand: None,
            ran_out: false,
            spent: false,
            held_back: false,
            hogs: 1,
            largest: 212 * GB,
            left: vec![PassHog {
                path: PathBuf::from("/Users//x/aterm/target"),
                bytes: 212 * GB,
                bytes_partial: false,
                why: "a live checkout's build directory".to_owned(),
            }],
            ended_at_unix: Some(1_792_000_000),
        });
    });
    shelf.tally_freed(&[(41 * GB, crate::disk_board::BUILD_CACHES)]);
    (shelf, key)
}

#[test]
fn the_disk_row_with_the_harness_off() {
    let (shelf, key) = watched_board();
    let home = Path::new("/Users//x");
    let survey = Where::default();
    assert!(shelf.watched());
    let on = disk_facts(&shelf, Some(&key), Clock::read, &survey, home);
    assert!(on.watch);
    assert_eq!(on.level, Some(DiskLevel::Low));
    assert_eq!(
        (on.warn, on.floor, on.emergency, on.stuck),
        (Some(69 * GB), Some(34 * GB), Some(8 * GB), 1)
    );
    let freed = on.freed_today.as_ref().expect("freed today");
    assert_eq!((freed.0, freed.2.as_str()), (41 * GB, "build caches"));
    assert_eq!(freed.1 % 1000, 0, "seconds became ms");
    assert_eq!(on.hogs[0].path, "~/aterm/target");
    assert_eq!(on.hogs_at, Some(1_792_000_000_000));

    shelf.publish(|b| b.watched = false);
    assert!(!shelf.watched());
    let off = disk_facts(
        &shelf,
        Some(&key),
        || panic!("no clock is read with the harness off"),
        &survey,
        home,
    );
    assert_eq!(off, DiskFacts::default());

    let mut vitals = aterm_vitals::Vitals::new();
    let t0: i64 = 1_792_000_000_000;
    let mut snap = None;
    let mut available = 900 * GB;
    for i in 0..70u64 {
        let ms = i * 10_000;
        let sample = Sample {
            at_ns: ms * 1_000_000,
            at_unix_ms: t0 + i64::try_from(ms).expect("ms"),
            mono_ms: ms,
            cadence_ms: 10_000,
            volume: Some(VolumeSample {
                total: 2_000 * GB,
                free: available,
                available,
                mount: "/System/Volumes/Data".into(),
            }),
            disk: Some(off.clone()),
            ..Sample::default()
        };
        snap = Some(vitals.absorb(&sample));
        available -= 43_333_333;
    }
    let snap = snap.expect("a snapshot");
    let loc = Locale {
        platform: Platform::MacOs,
        utc_offset_s: 0,
    };
    let said: Vec<String> = words::details(Resource::Disk, &snap, &loc)
        .into_iter()
        .map(|c| c.forms[0].clone())
        .collect();
    let last = available + 43_333_333;
    assert!(said.iter().any(|c| c == "disk watch off"), "{said:?}");
    assert!(said.iter().any(|c| c == "+2.6 GB in 10 min"), "{said:?}");
    assert!(
        said.iter()
            .any(|c| *c == format!("{} free", words::bytes_lone(last))),
        "{said:?}"
    );
    assert!(
        !said
            .iter()
            .any(|c| c.contains("low") || c.contains("freed") || c.contains("stuck")),
        "{said:?}"
    );
    assert!(words::state(Resource::Disk, &snap, &loc).is_none());
    assert!(words::disk_wrong(&snap, &loc).is_none(), "no Wrong? off");
}

#[test]
fn the_newest_rows_win_and_the_survey_works_with_the_harness_off() {
    let (shelf, key) = watched_board();
    let home = Path::new("/Users//x");
    let survey_at = |at_unix_ms| crate::disk_where::Found {
        hogs: vec![Hog {
            path: "~/wt".into(),
            bytes: 96 * GB,
            partial: true,
            remedy: "9 target folders".into(),
        }],
        at_unix_ms: Some(at_unix_ms),
        cut: false,
    };
    let pass_ms = 1_792_000_000_000;
    let raw = shelf.deck_facts(&key, &Clock::read());
    let newer = deck_disk(raw.clone(), Some(survey_at(pass_ms + 1)), true, home);
    assert_eq!(
        (newer.hogs[0].path.as_str(), newer.hogs_at, newer.sizing),
        ("~/wt", Some(pass_ms + 1), true)
    );
    let older = deck_disk(raw.clone(), Some(survey_at(pass_ms - 1)), false, home);
    assert_eq!(
        (older.hogs[0].path.as_str(), older.hogs_at, older.sizing),
        ("~/aterm/target", Some(pass_ms), false)
    );
    let empty = crate::disk_where::Found {
        hogs: Vec::new(),
        ..survey_at(pass_ms + 1)
    };
    let cut = crate::disk_where::Found {
        cut: true,
        ..survey_at(pass_ms + 1)
    };
    for found in [empty, cut.clone()] {
        let kept = deck_disk(raw.clone(), Some(found.clone()), false, home);
        assert_eq!(kept.hogs[0].path, "~/aterm/target", "{found:?}");
    }
    let alone = deck_disk(DiskFactsRaw::default(), Some(cut), false, home);
    assert_eq!(alone.hogs[0].path, "~/wt", "no pass rows to keep");

    let off = deck_disk(DiskFactsRaw::default(), Some(survey_at(1)), false, home);
    assert!(!off.watch);
    assert_eq!(off.hogs.len(), 1);
    let mut snap = aterm_vitals::Vitals::new().absorb(&Sample {
        at_unix_ms: 1_792_000_000_000,
        disk: Some(off),
        ..Sample::default()
    });
    snap.facts.disk.as_mut().expect("facts").level = None;
    let loc = Locale::default();
    let said: Vec<String> = words::details(Resource::Disk, &snap, &loc)
        .into_iter()
        .map(|c| c.forms[0].clone())
        .collect();
    assert!(
        said.iter().any(|c| c == "most in ~/wt, ≥ 96 GB"),
        "{said:?}"
    );
}

fn with_reader(app: &mut crate::App, board: &'static JobsBoard) -> Receiver<Msg> {
    let (tx, rx) = std::sync::mpsc::sync_channel(2);
    app.vitals.tx = Some(tx);
    app.vitals.tried = true;
    app.vitals.board = board;
    rx
}

fn read_req(rx: &Receiver<Msg>, why: &str) -> Box<Request> {
    rx.try_iter()
        .find_map(|m| match m {
            Msg::Read(r) => Some(r),
            _ => None,
        })
        .expect(why)
}

fn asked(rx: &Receiver<Msg>) -> usize {
    rx.try_iter().filter(|m| matches!(m, Msg::Read(_))).count()
}

fn land(app: &crate::App, seq: u64, rows: Vec<JobRow>, demand: Demand) {
    let view = JobsView {
        rows,
        demand,
        ..JobsView::default()
    };
    app.vitals.latest.lock().unwrap().snap = Some(Arc::new(Snapshot {
        seq,
        jobs: Some(Arc::new(view)),
        ..Snapshot::default()
    }));
}

fn sid_of_tab0(app: &crate::App) -> String {
    let store = app.store.read().unwrap();
    store
        .by_local(0)
        .expect("tab 0's session")
        .sid
        .as_str()
        .to_owned()
}

fn hung_call() -> JobRow {
    JobRow {
        standing: Some(aterm_vitals::jobs::Standing::Capped),
        ..crate::vitals_jobs::tests::call_row(0, Consumer::AgentBg, true, 31, 27098, "python3")
    }
}

#[test]
fn delegated_work_pulls_once_before_any_snapshot_and_uses_the_earlier_deadline() {
    static BOARD: JobsBoard = JobsBoard::new();
    let mut app = crate::App::headless_for_test();
    let rx = with_reader(&mut app, &BOARD);
    let now = Instant::now();
    app.serve_windowless_vitals(Audience::None, now, false);
    assert_eq!(asked(&rx), 0);
    app.serve_windowless_vitals(Audience::None, now, true);
    let first = read_req(&rx, "registration starts the first reading");
    assert!(!first.machine_only);
    assert_eq!(first.cadence_ms, 5000);
    app.serve_windowless_vitals(Audience::None, now + Duration::from_secs(1), true);
    assert_eq!(asked(&rx), 0, "only one reading waits for a snapshot");
    assert!(!app.vitals.chips_shown, "hidden reads do not show meters");

    land(&app, 1, Vec::new(), Demand::Idle);
    assert!(app.adopt_vitals(Audience::None, now, true));
    assert_eq!(app.vitals_deadline(), Some(now + BACKGROUND_PERIOD));
    app.serve_windowless_vitals(
        Audience::None,
        now + BACKGROUND_PERIOD - Duration::from_millis(1),
        true,
    );
    assert_eq!(asked(&rx), 0);
    app.serve_windowless_vitals(Audience::None, now + BACKGROUND_PERIOD, true);
    assert_eq!(asked(&rx), 1, "delegated work needs no suspect call");

    land(&app, 2, vec![hung_call()], Demand::Slow);
    assert!(app.adopt_vitals(Audience::None, now, true));
    assert_eq!(app.vitals_deadline(), Some(now + BACKGROUND_PERIOD));
    land(&app, 3, vec![hung_call()], Demand::Fast);
    assert!(app.adopt_vitals(Audience::None, now, true));
    assert_eq!(app.vitals_deadline(), Some(now + Duration::from_secs(2)));
    app.serve_windowless_vitals(Audience::None, now, false);
    assert_eq!(
        app.vitals_deadline(),
        Some(now + Duration::from_secs(2)),
        "the last delegated job leaving does not stop the engine"
    );

    land(&app, 4, Vec::new(), Demand::Idle);
    assert!(app.adopt_vitals(Audience::None, now, false));
    assert_eq!(app.vitals_deadline(), None);
}

/// A new view's age counts from when the probe began the oldest reading handed over since the
/// last adoption that carries it, not from the adoption, so the facts gate sees the probe's read.
#[test]
fn a_views_age_counts_from_the_probes_read_not_its_adoption() {
    static BOARD: JobsBoard = JobsBoard::new();
    let mut app = crate::App::headless_for_test();
    let _rx = with_reader(&mut app, &BOARD);
    let sid = sid_of_tab0(&app);
    let view = Arc::new(JobsView {
        rows: vec![hung_call()],
        demand: Demand::Fast,
        ..JobsView::default()
    });
    let snap = |seq| {
        Arc::new(Snapshot {
            seq,
            jobs: Some(Arc::clone(&view)),
            ..Snapshot::default()
        })
    };
    let read = Instant::now();
    hand_over(&app.vitals.latest, &snap(1), Vec::new(), read);
    hand_over(
        &app.vitals.latest,
        &snap(2),
        Vec::new(),
        read + Duration::from_secs(1),
    );
    let adopted = read + Duration::from_secs(3);
    assert!(app.adopt_vitals(Audience::None, adopted, false));
    let (age, calls) = BOARD
        .facts(&sid, adopted)
        .expect("a view inside JOBS_FRESH");
    assert_eq!((age, calls.len()), (Duration::from_secs(3), 1));
}

/// Readings the loop never adopted pile up while it sleeps; a new view's age counts from the
/// reading that made it, not from the first one handed over since the last adoption.
#[test]
fn an_earlier_reading_of_an_older_view_does_not_age_a_new_one() {
    static BOARD: JobsBoard = JobsBoard::new();
    let mut app = crate::App::headless_for_test();
    let _rx = with_reader(&mut app, &BOARD);
    let sid = sid_of_tab0(&app);
    let snap = |seq| {
        Arc::new(Snapshot {
            seq,
            jobs: Some(Arc::new(JobsView {
                rows: vec![hung_call()],
                demand: Demand::Fast,
                ..JobsView::default()
            })),
            ..Snapshot::default()
        })
    };
    let read = Instant::now();
    hand_over(&app.vitals.latest, &snap(1), Vec::new(), read);
    hand_over(
        &app.vitals.latest,
        &snap(2),
        Vec::new(),
        read + Duration::from_secs(70),
    );
    let adopted = read + Duration::from_secs(71);
    assert!(app.adopt_vitals(Audience::None, adopted, false));
    let (age, calls) = BOARD
        .facts(&sid, adopted)
        .expect("a new view, a second old");
    assert_eq!((age, calls.len()), (Duration::from_secs(1), 1));
}

/// With no window and a hung call standing, an ask the thread's full queue drops is due again
/// `JOBS_FLOOR` on, not at the next edge or disk look; once the thread is gone, nothing is.
#[test]
fn a_dropped_windowless_ask_is_asked_again_at_the_floor() {
    static BOARD: JobsBoard = JobsBoard::new();
    let mut app = crate::App::headless_for_test();
    let rx = with_reader(&mut app, &BOARD);
    let now = Instant::now();
    land(&app, 1, vec![hung_call()], Demand::Fast);
    assert!(app.adopt_vitals(Audience::None, now, false));
    let due = now + Duration::from_secs(2);
    assert_eq!(app.vitals_deadline(), Some(due));
    let tx = app.vitals.tx.clone().expect("the thread's queue");
    for _ in 0..2 {
        tx.try_send(Msg::Read(Box::default())).expect("room");
    }
    app.serve_windowless_vitals(Audience::None, due, false);
    assert_eq!(asked(&rx), 2, "the ask was dropped");
    assert_eq!(app.vitals.jobs_due, Some(due + JOBS_FLOOR));
    drop((tx, rx));
    app.serve_windowless_vitals(Audience::None, due + JOBS_FLOOR, false);
    assert_eq!(app.vitals.jobs_due, None, "no thread: nothing to ask");
}

/// A dropped ask stays due at the floor across an in-flight pull's whole reading that ran no
/// engine pass (it carries the held view); a view made by a reading begun since is the look, and
/// then nothing is due.
#[test]
fn a_dropped_ask_outlives_a_whole_reading_that_did_not_look() {
    static BOARD: JobsBoard = JobsBoard::new();
    let mut app = crate::App::headless_for_test();
    let rx = with_reader(&mut app, &BOARD);
    let now = Instant::now();
    land(&app, 1, Vec::new(), Demand::Idle);
    assert!(app.adopt_vitals(Audience::None, now, false));
    let held = app.vitals.snapshot.as_ref().and_then(|s| s.jobs.clone());
    let tx = app.vitals.tx.clone().expect("the thread's queue");
    for _ in 0..2 {
        tx.try_send(Msg::Read(Box::default())).expect("room");
    }
    app.vitals.disk_pull = true;
    app.serve_windowless_vitals(Audience::None, now, false);
    assert_eq!(asked(&rx), 2, "the look's ask was dropped");
    let pulled = |seq, jobs| {
        Arc::new(Snapshot {
            seq,
            jobs,
            ..Snapshot::default()
        })
    };
    hand_over(&app.vitals.latest, &pulled(2, held), Vec::new(), now);
    let later = now + Duration::from_secs(1);
    assert!(app.adopt_vitals(Audience::None, later, false));
    assert_eq!(app.vitals.jobs_due, Some(now + JOBS_FLOOR), "still owed");
    let looked = Some(Arc::new(JobsView::default()));
    hand_over(&app.vitals.latest, &pulled(3, looked), Vec::new(), later);
    assert!(app.adopt_vitals(Audience::None, later, false));
    assert_eq!(app.vitals.jobs_due, None, "a new view since: looked");
}

#[test]
fn machine_only_and_held_engine_views_do_not_move_delegated_sweeps() {
    static BOARD: JobsBoard = JobsBoard::new();
    let mut app = crate::App::headless_for_test();
    let rx = with_reader(&mut app, &BOARD);
    let now = Instant::now();
    app.serve_windowless_vitals(Audience::None, now, true);
    assert_eq!(asked(&rx), 1);
    land(&app, 1, Vec::new(), Demand::Idle);
    assert!(app.adopt_vitals(Audience::None, now, true));
    let held = app.vitals.snapshot.as_ref().unwrap().jobs.clone();
    let land_held = |app: &crate::App, seq, machine_only| {
        app.vitals.latest.lock().unwrap().snap = Some(Arc::new(Snapshot {
            seq,
            jobs: held.clone(),
            machine_only,
            ..Snapshot::default()
        }));
    };

    land_held(&app, 2, true);
    assert!(app.adopt_vitals(Audience::None, now + Duration::from_secs(4), true));
    assert_eq!(app.vitals.full_adopted_at, Some(now));
    assert_eq!(app.vitals_deadline(), Some(now + BACKGROUND_PERIOD));
    app.serve_windowless_vitals(Audience::None, now + BACKGROUND_PERIOD, true);
    assert_eq!(asked(&rx), 1);
    land_held(&app, 3, true);
    assert!(app.adopt_vitals(Audience::None, now + BACKGROUND_PERIOD, true));
    assert_eq!(app.vitals_deadline(), None, "the full sweep is pending");
    app.serve_windowless_vitals(Audience::None, now + BACKGROUND_PERIOD, true);
    assert_eq!(
        asked(&rx),
        0,
        "a late machine reading adds no duplicate sweep"
    );

    land_held(&app, 4, false);
    assert!(app.adopt_vitals(Audience::None, now + BACKGROUND_PERIOD, true));
    assert_eq!(
        app.vitals.adopted_at,
        Some(now),
        "the engine view did not change"
    );
    assert_eq!(app.vitals.full_adopted_at, Some(now + BACKGROUND_PERIOD));
    assert_eq!(app.vitals_deadline(), Some(now + 2 * BACKGROUND_PERIOD));
}

#[test]
fn first_delegation_after_machine_only_and_return_from_visible_read_full() {
    static BOARD: JobsBoard = JobsBoard::new();
    let mut app = crate::App::headless_for_test();
    let rx = with_reader(&mut app, &BOARD);
    let now = Instant::now();
    app.vitals.latest.lock().unwrap().snap = Some(Arc::new(Snapshot {
        seq: 1,
        machine_only: true,
        ..Snapshot::default()
    }));
    assert!(app.adopt_vitals(Audience::Hidden, now, false));
    assert_eq!(app.vitals.full_adopted_at, None);
    app.serve_windowless_vitals(Audience::Hidden, now, true);
    assert!(!read_req(&rx, "first delegation reads full").machine_only);
    land(&app, 2, Vec::new(), Demand::Idle);
    assert!(app.adopt_vitals(Audience::Hidden, now, true));

    app.vitals.windowed = true;
    app.vitals.delegated_due = None;
    land(&app, 3, Vec::new(), Demand::Idle);
    let visible = now + Duration::from_secs(8);
    assert!(app.adopt_vitals(Audience::Shown, visible, true));
    app.serve_windowless_vitals(Audience::None, visible, true);
    assert_eq!(asked(&rx), 0);
    assert_eq!(app.vitals_deadline(), Some(visible + BACKGROUND_PERIOD));
    app.serve_windowless_vitals(Audience::None, visible + BACKGROUND_PERIOD, true);
    assert!(!read_req(&rx, "delegated cadence survives visibility").machine_only);
    assert!(!app.vitals.chips_shown);
}

#[test]
fn a_late_machine_snapshot_does_not_duplicate_a_pending_engine_sweep() {
    static BOARD: JobsBoard = JobsBoard::new();
    let mut app = crate::App::headless_for_test();
    let rx = with_reader(&mut app, &BOARD);
    let now = Instant::now();
    land(&app, 1, vec![hung_call()], Demand::Fast);
    assert!(app.adopt_vitals(Audience::None, now, true));
    let held = app.vitals.snapshot.as_ref().unwrap().jobs.clone();
    let due = now + Duration::from_secs(2);
    app.serve_windowless_vitals(Audience::None, due, true);
    assert_eq!(asked(&rx), 1);
    app.vitals.latest.lock().unwrap().snap = Some(Arc::new(Snapshot {
        seq: 2,
        jobs: held,
        machine_only: true,
        ..Snapshot::default()
    }));
    assert!(app.adopt_vitals(Audience::None, due, true));
    assert_eq!(app.vitals_deadline(), None);
    app.serve_windowless_vitals(Audience::None, due, true);
    assert_eq!(asked(&rx), 0, "the engine's full sweep is already pending");
}

#[test]
fn with_no_window_the_disk_look_reads_once_and_the_board_names_the_hung_call() {
    static BOARD: JobsBoard = JobsBoard::new();
    let mut app = crate::App::headless_for_test();
    let rx = with_reader(&mut app, &BOARD);
    let sid = sid_of_tab0(&app);
    app.service_vitals(Instant::now());
    assert_eq!(asked(&rx), 0, "no window and no look: nothing is read");
    assert_eq!(app.vitals_deadline(), None);

    app.note_harness_host(HostNote::DiskLooked);
    assert_eq!(asked(&rx), 1, "the look's one reading");
    app.service_vitals(Instant::now());
    assert_eq!(asked(&rx), 0, "nothing more while it is read");

    land(&app, 1, vec![hung_call()], Demand::Fast);
    app.on_vitals_wake();
    let posted = Instant::now();
    let named = |at| match BOARD.evidence(&sid, at) {
        BackgroundEvidence::NoProgress { worst } => Some((worst.pid, worst.quiet_min())),
        BackgroundEvidence::Unknown => None,
    };
    assert_eq!(named(posted), Some((27098, 31)), "the board speaks");

    let due = app
        .vitals_deadline()
        .expect("the engine's demand names the next reading");
    assert_eq!(
        Some(due),
        app.vitals.adopted_at.map(|at| at + Duration::from_secs(2)),
        "the engine's fast cadence, from the adoption"
    );
    app.service_vitals(due - Duration::from_millis(1));
    assert_eq!(asked(&rx), 0, "not before the engine's cadence");
    app.service_vitals(due);
    assert_eq!(asked(&rx), 1, "the engine's demand reads");
    assert_eq!(app.vitals_deadline(), None, "named again when it lands");

    land(&app, 2, Vec::new(), Demand::Idle);
    app.on_vitals_wake();
    assert_eq!(app.vitals_deadline(), None, "no agent call: nothing due");
    app.service_vitals(due + DISK_TICK / 2);
    assert_eq!(asked(&rx), 0, "nothing more within the tick");
}

/// With a window showing the bars, their cadence reads at once and then a period on; the disk
/// watch's look asks nothing, and a jobs view whose engine is fast moves no deadline.
#[cfg(unix)]
#[test]
fn with_a_window_showing_the_bars_the_look_and_the_engine_move_nothing() {
    static BOARD: JobsBoard = JobsBoard::new();
    let bars = crate::stats_deck::tests::attach_stats();
    let mut app = crate::App::headless_for_test();
    let rx = with_reader(&mut app, &BOARD);
    let t0 = Instant::now();
    app.service_vitals(t0);
    assert_eq!(asked(&rx), 1, "read at once");
    let first = app.vitals_deadline().expect("the bars' cadence");
    let period = first - t0;
    assert!(
        [FOCUSED_PERIOD, BACKGROUND_PERIOD].contains(&period),
        "{period:?}"
    );

    app.note_harness_host(HostNote::DiskLooked);
    assert_eq!((asked(&rx), app.vitals_deadline()), (0, Some(first)));
    land(&app, 1, vec![hung_call()], Demand::Fast);
    app.on_vitals_wake();
    assert_eq!(
        app.vitals_deadline(),
        Some(first),
        "the demand moves nothing"
    );
    app.service_vitals(first);
    assert_eq!(
        (asked(&rx), app.vitals_deadline()),
        (1, Some(first + period))
    );
    land(&app, 2, vec![hung_call()], Demand::Slow);
    app.on_vitals_wake();

    drop(bars);
    app.service_vitals(first + Duration::from_millis(1));
    assert_eq!(asked(&rx), 0);
    assert_eq!(
        app.vitals_deadline(),
        app.vitals.adopted_at.map(|at| at + Duration::from_secs(30)),
        "the held view's demand (slow)"
    );
    let _bars = crate::stats_deck::tests::attach_stats();
    app.service_vitals(first + Duration::from_millis(2));
    assert_eq!(asked(&rx), 1, "shown again: read at once");
}

/// A deck whose every window is occluded reads the machine counters every ten seconds, and with no
/// window in view the disk watch's look asks one whole reading for the jobs engine.
#[cfg(unix)]
#[test]
fn a_hidden_deck_reads_the_machine_and_the_engine_reads_whole() {
    static BOARD: JobsBoard = JobsBoard::new();
    let _bars = crate::stats_deck::tests::attach_stats();
    let mut app = crate::App::headless_for_test();
    let rx = with_reader(&mut app, &BOARD);
    for ws in app.windows.values_mut() {
        ws.occluded = true;
    }
    let kinds = |rx: &Receiver<Msg>| -> Vec<bool> {
        rx.try_iter()
            .filter_map(|m| match m {
                Msg::Read(r) => Some(r.machine_only),
                _ => None,
            })
            .collect()
    };
    let t0 = Instant::now();
    app.service_vitals(t0);
    assert_eq!(kinds(&rx), [true], "the machine counters at once");
    assert_eq!(app.vitals_deadline(), Some(t0 + HIDDEN_PERIOD));
    app.service_vitals(t0 + HIDDEN_PERIOD / 2);
    assert_eq!(kinds(&rx), Vec::<bool>::new(), "not before ten seconds");

    app.note_harness_host(HostNote::DiskLooked);
    assert_eq!(kinds(&rx), [false], "the look's one whole reading");
    let asked = app.vitals.asked_at.expect("asked");
    land(&app, 1, vec![hung_call()], Demand::Slow);
    app.on_vitals_wake();
    let engine = app
        .vitals
        .adopted_at
        .map(|at| at + Duration::from_secs(30))
        .expect("the view's adoption");
    assert_eq!(
        app.vitals.jobs_due,
        Some(engine),
        "the engine's slow cadence"
    );
    assert_eq!(
        app.vitals_deadline(),
        Some(asked + HIDDEN_PERIOD),
        "the hidden deck's next reading comes first"
    );

    for (seq, at) in [(2, asked + HIDDEN_PERIOD), (3, asked + 2 * HIDDEN_PERIOD)] {
        app.service_vitals(at);
        assert_eq!(kinds(&rx), [true], "a hidden reading at {seq}");
        let held = app.vitals.snapshot.as_ref().and_then(|s| s.jobs.clone());
        app.vitals.latest.lock().unwrap().snap = Some(Arc::new(Snapshot {
            seq,
            jobs: held,
            machine_only: true,
            ..Snapshot::default()
        }));
        app.on_vitals_wake();
        assert_eq!(app.vitals.jobs_due, Some(engine), "not pushed out at {seq}");
    }
    app.service_vitals(engine);
    assert_eq!(kinds(&rx), [false], "the engine's demand reads whole");
}

#[test]
fn output_with_no_window_reads_and_never_faster_than_the_floor() {
    static BOARD: JobsBoard = JobsBoard::new();
    let mut app = crate::App::headless_for_test();
    let rx = with_reader(&mut app, &BOARD);
    let t0 = Instant::now();
    app.service_vitals(t0);
    assert_eq!(asked(&rx), 0, "no output: nothing is read");
    app.vitals.note_edge();
    assert_eq!(app.vitals.edge_at(t0), Some(t0));
    app.service_vitals(t0);
    assert_eq!(asked(&rx), 1, "an edge: one reading");
    app.vitals.note_edge();
    app.service_vitals(t0 + Duration::from_millis(500));
    assert_eq!(asked(&rx), 0, "within the floor: nothing more");
    assert_eq!(app.vitals.edge_at(t0), Some(t0 + JOBS_FLOOR));
    app.service_vitals(t0 + JOBS_FLOOR);
    assert_eq!(asked(&rx), 1, "the floor passed: the owed reading");

    land(&app, 1, Vec::new(), Demand::Idle);
    app.on_vitals_wake();
    assert_eq!(app.vitals_deadline(), None, "an idle view: nothing due");
    app.vitals.note_edge();
    app.service_vitals(t0 + 3 * JOBS_FLOOR);
    assert_eq!(asked(&rx), 1, "output reads");
}

#[test]
fn a_job_with_no_window_is_read_until_it_ends_and_its_end_is_announced() {
    use aterm_vitals::events::{Event, EventKind};
    use aterm_vitals::jobs::{Class, Verdict};
    static BOARD: JobsBoard = JobsBoard::new();
    let mut app = crate::App::headless_for_test();
    let rx = with_reader(&mut app, &BOARD);
    let t0 = Instant::now();
    app.vitals.note_edge();
    app.service_vitals(t0);
    assert_eq!(asked(&rx), 1, "the typed command's output reads");
    let job = |verdict, class| JobRow {
        tab: 0,
        class,
        ..crate::stats_paint::tests::job_row("j41", verdict, None, Some(1_000))
    };
    land(
        &app,
        1,
        vec![job(Verdict::Fine, Class::Running)],
        Demand::Fast,
    );
    app.on_vitals_wake();
    assert!(
        app.status_job_fields(0)
            .starts_with("job=fine job_quiet_ms="),
        "{}",
        app.status_job_fields(0)
    );
    assert!(
        app.status_job_fields(0).ends_with(NONE_HELD),
        "{}",
        app.status_job_fields(0)
    );
    let adopted = app.vitals.adopted_at.expect("adopted");
    assert_eq!(
        app.vitals_deadline(),
        Some(adopted + Duration::from_secs(2)),
        "a running job is read at the engine's cadence"
    );
    app.service_vitals(adopted + Duration::from_secs(1));
    assert_eq!(asked(&rx), 0, "not before its deadline");
    app.service_vitals(adopted + Duration::from_secs(2));
    assert_eq!(asked(&rx), 1, "no output, no pull: the job demand reads");

    land(
        &app,
        2,
        vec![job(Verdict::Exited, Class::Exited)],
        Demand::Idle,
    );
    app.vitals.latest.lock().unwrap().job_events.push(Event {
        seq: 2,
        at_ns: 0,
        kind: EventKind::Job {
            tab: 0,
            id: "j41".into(),
            from: Some(Verdict::Fine),
            to: Verdict::Exited,
            class: Class::Exited,
            task: None,
            exit: Some(0),
        },
    });
    app.on_vitals_wake();
    assert_eq!(
        app.status_job_fields(0),
        format!("job=none job_quiet_ms=- {NONE_HELD}")
    );
    let recorded: Vec<String> = {
        let tl = Arc::clone(&app.pool.get(0).expect("tab 0's session").ctx.timeline);
        let tl = tl.lock().unwrap();
        tl.view_since(None)
            .rows
            .iter()
            .filter(|e| e.kind == crate::sys_host::JOB_CHANGE)
            .map(|e| e.payload.to_string())
            .collect()
    };
    assert_eq!(recorded, ["exited job=j41 class=exited rev=1 code=0"]);
    assert_eq!(app.vitals_deadline(), None, "nothing live: nothing due");
}

/// `status`'s `program_check=` with no program held: `0` on macOS, `-` elsewhere.
const NONE_HELD: &str = if cfg!(target_os = "macos") {
    "program_check=0"
} else {
    "program_check=-"
};

/// `status` counts the programs macOS's program check holds in its session's tab, from the view
/// its `job=` reads, the oldest counted on to now; another tab's held program is not its.
#[test]
fn status_counts_the_programs_held_in_its_sessions_tab() {
    use aterm_vitals::jobs::{Findings, Named};
    static BOARD: JobsBoard = JobsBoard::new();
    let mut app = crate::App::headless_for_test();
    let _rx = with_reader(&mut app, &BOARD);
    let held = |pid, tab| Named {
        pid,
        name: "git-remote-https".into(),
        tab: Some(tab),
        for_ms: 1_140_000,
    };
    app.vitals.latest.lock().unwrap().snap = Some(Arc::new(Snapshot {
        seq: 1,
        jobs: Some(Arc::new(JobsView {
            findings: Findings {
                program_check: vec![held(63454, 0), held(63455, 9)],
                ..Findings::default()
            },
            ..JobsView::default()
        })),
        ..Snapshot::default()
    }));
    app.on_vitals_wake();
    let fields = app.status_job_fields(0);
    if cfg!(target_os = "macos") {
        let held = fields
            .strip_prefix("job=none job_quiet_ms=- program_check=1:")
            .unwrap_or_else(|| panic!("one program held in tab 0: {fields}"));
        assert!(
            held.parse::<u64>().is_ok_and(|ms| ms >= 1_140_000),
            "the oldest, to now: {fields}"
        );
    } else {
        assert_eq!(fields, "job=none job_quiet_ms=- program_check=-");
    }
}

#[test]
fn output_with_no_window_is_read_in_its_own_wake_and_arms_nothing() {
    static BOARD: JobsBoard = JobsBoard::new();
    let mut app = crate::App::headless_for_test();
    let rx = with_reader(&mut app, &BOARD);
    let t0 = Instant::now();
    land(&app, 1, Vec::new(), Demand::Idle);
    app.on_vitals_wake();
    app.service_vitals(t0);
    assert_eq!(asked(&rx), 0, "parked: nothing is read");
    feed(&app, b"$ ");
    app.on_output_wake(0, t0);
    assert_eq!(asked(&rx), 1, "the output's reading, in its wake");
    assert_eq!(app.vitals_deadline(), None, "and nothing armed for it");
    feed(&app, b"ls");
    app.on_output_wake(0, t0 + Duration::from_millis(500));
    assert_eq!(asked(&rx), 0, "within the floor: owed");
    assert_eq!(app.vitals.edge_at(t0), Some(t0 + JOBS_FLOOR));
}

/// A screen generation `seq` within epoch 0, for a feed that drew.
#[cfg(target_os = "macos")]
fn drawn(seq: u64) -> crate::control::ScreenGen {
    crate::control::ScreenGen { epoch: 0, seq }
}

fn feed(app: &crate::App, bytes: &[u8]) {
    feed_tab(app, 0, bytes);
}

/// Feeds `bytes` to tab `tab`'s engine as its PTY reader does, counting the move and feeding the
/// jobs feed.
fn feed_tab(app: &crate::App, tab: u64, bytes: &[u8]) {
    feed_tab_at(app, tab, bytes, Instant::now);
}

/// [`feed_tab`] with the feed stamped by `clock`.
fn feed_tab_at(app: &crate::App, tab: u64, bytes: &[u8], clock: impl FnOnce() -> Instant) {
    let s = app.pool.get(tab).expect("the tab");
    s.ctx.job_feed.read(bytes);
    let mut t = s.term.lock().unwrap_or_else(PoisonError::into_inner);
    t.process(bytes);
    let moved = s.ctx.screen_moves.fed(crate::control::screen_gen(&t));
    s.ctx.job_feed.fed(&t, bytes, moved, clock);
}

const MUSE_RESET: &[u8] = b"\x1b[39m\x1b[49m\x1b[59m\x1b[0m\x1b[5;3H";

const GEMINI_QUERY: &[u8] = b"\x1b]11;?\x1b\\";

#[test]
fn a_write_that_moves_no_cell_is_no_edge_and_arms_nothing() {
    static BOARD: JobsBoard = JobsBoard::new();
    let mut app = crate::App::headless_for_test();
    let rx = with_reader(&mut app, &BOARD);
    let t0 = Instant::now();
    land(&app, 1, Vec::new(), Demand::Idle);
    app.on_vitals_wake();
    app.service_vitals(t0);
    assert_eq!(asked(&rx), 0, "parked: nothing is read");
    feed(&app, b"$ ");
    app.on_output_wake(0, t0);
    assert_eq!(
        asked(&rx),
        1,
        "the prompt moved the screen: read in its wake"
    );

    for (n, bytes) in (1_u64..).zip([MUSE_RESET, GEMINI_QUERY, MUSE_RESET]) {
        let at = t0 + Duration::from_millis(400 * n);
        feed(&app, bytes);
        app.on_output_wake(0, at);
        assert_eq!(asked(&rx), 0, "write {n} moved no cell: nothing read");
        assert_eq!(app.vitals.edge_at(at), None, "write {n}: no edge owed");
        assert_eq!(app.vitals_deadline(), None, "write {n}: nothing armed");
    }

    feed(&app, b"x");
    app.on_output_wake(0, t0 + Duration::from_millis(1_800));
    assert_eq!(asked(&rx), 0, "a moved screen inside the floor: owed");
    assert_eq!(app.vitals.edge_at(t0), Some(t0 + JOBS_FLOOR));
    app.service_vitals(t0 + JOBS_FLOOR);
    assert!(read_req(&rx, "the owed reading").jobs_now);

    let later = t0 + 3 * JOBS_FLOOR;
    assert_eq!(
        app.vitals_output_kind(0, later),
        OutputEdge::Quiet,
        "nothing fed since: where it was"
    );
}

#[test]
fn a_write_whose_wake_meets_the_term_lock_held_is_judged_by_what_the_reader_fed() {
    static BOARD: JobsBoard = JobsBoard::new();
    let mut app = crate::App::headless_for_test();
    let rx = with_reader(&mut app, &BOARD);
    let t0 = Instant::now();
    land(&app, 1, Vec::new(), Demand::Idle);
    app.on_vitals_wake();
    app.service_vitals(t0);
    feed(&app, b"$ ");
    app.on_output_wake(0, t0);
    assert_eq!(
        asked(&rx),
        1,
        "the prompt moved the screen: read in its wake"
    );
    let term = Arc::clone(&app.pool.get(0).expect("tab 0").term);
    for (n, bytes) in (1_u64..).zip([MUSE_RESET, GEMINI_QUERY, MUSE_RESET]) {
        let at = t0 + Duration::from_millis(400 * n);
        feed(&app, bytes);
        let held = term.lock().unwrap_or_else(PoisonError::into_inner);
        app.vitals_output_edge(0, at);
        drop(held);
        assert_eq!(asked(&rx), 0, "write {n} moved no cell: nothing read");
        assert_eq!(app.vitals.edge_at(at), None, "write {n}: no edge owed");
        assert_eq!(app.vitals_deadline(), None, "write {n}: nothing armed");
    }
    let at = t0 + Duration::from_millis(1_800);
    feed(&app, b"x");
    let held = term.lock().unwrap_or_else(PoisonError::into_inner);
    app.vitals_output_edge(0, at);
    drop(held);
    assert_eq!(asked(&rx), 0, "a moved screen inside the floor: owed");
    assert_eq!(app.vitals.edge_at(at), Some(t0 + JOBS_FLOOR));
    assert!(app.vitals_deadline().is_some(), "armed for the floor");
}

#[test]
fn idle_writes_of_two_tabs_inside_each_others_floor_ask_nothing() {
    static BOARD: JobsBoard = JobsBoard::new();
    let mut app = crate::App::headless_for_test();
    app.pool.insert(crate::stub_session_with_sink(
        1,
        Arc::new(aterm_session::sink::SinkWriter::new(-1)),
    ));
    let rx = with_reader(&mut app, &BOARD);
    let t0 = Instant::now();
    land(&app, 1, Vec::new(), Demand::Idle);
    app.on_vitals_wake();
    app.service_vitals(t0);
    feed_tab(&app, 0, b"$ ");
    app.on_output_wake(0, t0);
    assert_eq!(asked(&rx), 1, "tab 0's prompt: read at once");
    land(&app, 2, Vec::new(), Demand::Idle);
    app.on_vitals_wake();
    feed_tab(&app, 1, b"% ");
    app.on_output_wake(1, t0 + Duration::from_millis(100));
    assert_eq!(asked(&rx), 0, "tab 1's prompt inside the floor: owed");
    app.service_vitals(t0 + JOBS_FLOOR);
    assert_eq!(asked(&rx), 1, "tab 1's prompt: read at the floor");
    land(&app, 3, Vec::new(), Demand::Idle);
    app.on_vitals_wake();

    let base = t0 + 2 * JOBS_FLOOR;
    feed_tab(&app, 0, b"x");
    app.on_output_wake(0, base);
    assert_eq!(asked(&rx), 1, "tab 0 moved: read, and its floor stands");
    land(&app, 4, Vec::new(), Demand::Idle);
    app.on_vitals_wake();
    for k in 1..=6_u64 {
        let tab = k % 2;
        let at = base + Duration::from_millis(250 * k);
        feed_tab(&app, tab, MUSE_RESET);
        app.on_output_wake(tab, at);
        assert_eq!(asked(&rx), 0, "write {k} (tab {tab}): nothing read");
        assert_eq!(app.vitals.edge_at(at), None, "write {k}: nothing owed");
        assert_eq!(app.vitals_deadline(), None, "write {k}: nothing armed");
    }
    let moved = base + Duration::from_millis(1_800);
    feed_tab(&app, 1, b"y");
    app.on_output_wake(1, moved);
    assert_eq!(asked(&rx), 0, "tab 1 moved inside tab 0's floor: owed");
    assert_eq!(app.vitals.edge_at(moved), Some(base + JOBS_FLOOR));
}

#[test]
fn a_prompt_fed_before_the_latch_clears_is_read_in_that_pass() {
    static BOARD: JobsBoard = JobsBoard::new();
    let mut app = crate::App::headless_for_test();
    let rx = with_reader(&mut app, &BOARD);
    let t0 = Instant::now();
    land(&app, 1, Vec::new(), Demand::Idle);
    app.on_vitals_wake();
    app.service_vitals(t0);
    feed(&app, b"$ sleep 1\r\n");
    app.on_output_wake(0, t0);
    assert_eq!(asked(&rx), 1, "the command's echo: read");

    let t1 = t0 + JOBS_FLOOR;
    feed(&app, MUSE_RESET);
    app.on_output_wake(0, t1);
    assert_eq!(asked(&rx), 0, "a reset alone moves no cell: nothing");

    let t2 = t1 + JOBS_FLOOR;
    feed(&app, MUSE_RESET);
    BEFORE_LATCH_CLEAR.set(Some(Box::new(|app, session| {
        assert_eq!(session, 0);
        feed(app, b"$ ");
    })));
    app.on_output_wake(0, t2);
    BEFORE_LATCH_CLEAR.set(None);
    assert_eq!(asked(&rx), 1, "the prompt fed as the pass began: read");
}

#[test]
fn a_process_event_with_no_window_asks_one_reading_the_engine_looks_at() {
    use std::sync::atomic::AtomicUsize;
    static BOARD: JobsBoard = JobsBoard::new();
    let mut app = crate::App::headless_for_test();
    let rx = with_reader(&mut app, &BOARD);
    let posts = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&posts);
    app.vitals.proc_edge.install(move || {
        counted.fetch_add(1, Ordering::AcqRel);
    });
    let t0 = Instant::now();
    land(&app, 1, Vec::new(), Demand::Idle);
    app.on_vitals_wake();
    app.service_vitals(t0);
    assert_eq!(asked(&rx), 0, "parked, no event: nothing is read");
    assert_eq!(app.vitals_deadline(), None, "nothing armed at rest");

    assert!(app.vitals.proc_edge.heard(), "armed: the event posts");
    assert!(!app.vitals.proc_edge.heard(), "one wake an edge");
    assert_eq!(posts.load(Ordering::Acquire), 1);
    app.service_vitals(t0);
    let req = read_req(&rx, "the kernel edge's reading");
    assert!(req.jobs_now, "the engine looks at the edge's reading");

    assert!(
        app.vitals.proc_edge.heard(),
        "armed again once that reading was asked"
    );
    app.service_vitals(t0 + Duration::from_millis(500));
    assert_eq!(asked(&rx), 0, "within the floor: owed");
    assert!(!app.vitals.proc_edge.heard(), "a reading owed: disarmed");
    assert_eq!(app.vitals.edge_at(t0), Some(t0 + JOBS_FLOOR));
    app.service_vitals(t0 + JOBS_FLOOR);
    assert!(read_req(&rx, "the owed reading").jobs_now);
    assert_eq!(posts.load(Ordering::Acquire), 2);

    app.vitals.windowed = true;
    app.vitals.arm_proc_edge();
    assert!(!app.vitals.proc_edge.heard(), "the bars read: disarmed");
}

#[test]
fn only_a_windowless_whole_reading_makes_the_engine_look() {
    static BOARD: JobsBoard = JobsBoard::new();
    let mut app = crate::App::headless_for_test();
    let rx = with_reader(&mut app, &BOARD);
    let t0 = Instant::now();
    app.vitals.disk_pull = true;
    app.serve_windowless_vitals(Audience::None, t0, false);
    assert!(read_req(&rx, "the disk look's reading").jobs_now);
    app.ask_reading(t0, FOCUSED_PERIOD, false, false);
    assert!(!read_req(&rx, "the bars' reading").jobs_now);
}

/// A job at rest on a real terminal. An in-place helper owns and reaps the
/// job, as its shell would.
#[cfg(target_os = "macos")]
struct JobAtRest {
    master: Option<std::os::fd::OwnedFd>,
    leader: Option<aterm_uds::spawnfd::Child>,
    stop: Option<std::io::PipeWriter>,
    stdout: Option<std::process::ChildStdout>,
    said: String,
    eof: bool,
    job: Option<i32>,
    ended: bool,
}

#[cfg(target_os = "macos")]
impl JobAtRest {
    fn new(raw: bool) -> Self {
        Self::starting(raw, |_| {})
    }

    /// A job at rest that writes: `cat` on the terminal, which echoes each line [`Self::poke`]
    /// types to it, as a program in front prints.
    fn writer() -> Self {
        Self::running(false, "/bin/cat", |_| {})
    }

    /// Types a line to the job, which a [`Self::writer`] reads and writes back.
    fn poke(&self) {
        // SAFETY: the owned live master is written once with a short line.
        let n = unsafe { libc::write(self.master(), b"x\n".as_ptr().cast(), 2) };
        assert_eq!(n, 2, "typed a line");
    }

    fn starting(raw: bool, before_ready: impl FnOnce(&mut Self)) -> Self {
        Self::running(raw, "/bin/sleep", before_ready)
    }

    fn running(raw: bool, program: &str, before_ready: impl FnOnce(&mut Self)) -> Self {
        use std::os::fd::{FromRawFd as _, OwnedFd};
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        // SAFETY: both fresh descriptors are transferred to their only
        // owners before any assertion or other fallible setup.
        let master = unsafe { OwnedFd::from_raw_fd(master) };
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        if raw {
            use std::os::fd::AsRawFd as _;
            // SAFETY: This test owns the live PTY and local termios.
            unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                assert_eq!(libc::tcgetattr(master.as_raw_fd(), &mut t), 0, "tcgetattr");
                t.c_lflag &= !(libc::ICANON | libc::ECHO);
                assert_eq!(
                    libc::tcsetattr(master.as_raw_fd(), libc::TCSANOW, &t),
                    0,
                    "tcsetattr"
                );
            }
        }
        let (stop_read, stop) = std::io::pipe().expect("stop pipe");
        let module = module_path!().split_once("::").expect("crate and module").1;
        let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test image"));
        cmd.args([
            "--exact",
            &format!("{module}::job_at_rest_leader"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("ATERM_TEST_VITALS_JOB_LEADER", "1")
        .env("ATERM_TEST_VITALS_JOB_PROGRAM", program)
        .setsid(true)
        .stdin(slave)
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(stop_read);
        let mut leader = cmd.spawn().expect("spawn the session leader");
        let stdout = leader.stdout.take();
        let mut guard = Self {
            master: Some(master),
            leader: Some(leader),
            stop: Some(stop),
            stdout,
            said: String::new(),
            eof: false,
            job: None,
            ended: false,
        };
        before_ready(&mut guard);
        guard.ready();
        guard
    }

    fn master(&self) -> i32 {
        use std::os::fd::AsRawFd as _;
        self.master.as_ref().expect("owned master").as_raw_fd()
    }

    fn message(&self, name: &str) -> Option<i32> {
        self.said
            .lines()
            .find_map(|line| line.strip_prefix(name)?.parse().ok())
    }

    fn drain(&mut self) -> std::io::Result<()> {
        use std::io::Read as _;
        use std::os::fd::AsRawFd as _;
        let stdout = self.stdout.as_mut().expect("helper's stdout");
        while !self.eof {
            let mut p = libc::pollfd {
                fd: stdout.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: Poll reads and fills this one owned descriptor's slot.
            let n = unsafe { libc::poll(&mut p, 1, 0) };
            if n == 0 {
                break;
            }
            if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            let mut bytes = [0; 512];
            match stdout.read(&mut bytes) {
                Ok(0) => self.eof = true,
                Ok(n) => self.said.push_str(&String::from_utf8_lossy(&bytes[..n])),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn ready(&mut self) {
        let leader = self.leader.as_ref().expect("owned leader").id();
        self.job = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            Duration::from_millis(10),
            || {
                self.drain().expect("read the helper");
                let job = self.message("VITALS_JOB_READY ")?;
                (crate::quit_safety::foreground_pgrp(self.master()) == job
                    && aterm_uds::process::read_process_parent(job) == Some(leader))
                .then_some(job)
            },
        );
        assert!(
            self.job.is_some(),
            "the owned job leads the foreground: {}",
            self.said
        );
    }

    fn stop(&mut self) -> bool {
        if self.ended {
            return true;
        }
        drop(self.stop.take());
        let ended = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            Duration::from_millis(10),
            || {
                self.drain().ok()?;
                let status = self.leader.as_mut()?.try_wait().ok()??;
                (self.eof
                    && status.success()
                    && self
                        .message("VITALS_JOB_REAPED ")
                        .is_some_and(|pid| self.job.is_none_or(|j| j == pid)))
                .then_some(())
            },
        );
        self.ended = ended.is_some();
        self.ended
    }

    fn app(&self, board: &'static JobsBoard) -> (crate::App, Receiver<Msg>) {
        let sink = Arc::new(aterm_session::sink::SinkWriter::new(self.master()));
        let mut app = crate::App::headless_for_test_with_sink(sink);
        app.pool.sessions.get_mut(&0).expect("tab 0").session.pid =
            i32::try_from(self.leader.as_ref().expect("owned leader").id()).expect("pid");
        let rx = with_reader(&mut app, board);
        (app, rx)
    }
}

#[cfg(target_os = "macos")]
impl Drop for JobAtRest {
    fn drop(&mut self) {
        if !self.stop() {
            eprintln!(
                "retaining vitals helper {:?}, master and stdout: {}",
                self.leader.as_ref().map(|c| c.id()),
                self.said
            );
            std::mem::forget(self.leader.take());
            std::mem::forget(self.master.take());
            std::mem::forget(self.stdout.take());
            if !std::thread::panicking() {
                panic!("vitals fixture cleanup was not confirmed; handles retained");
            }
        }
    }
}

/// Only the owned helper reexec runs this body. It owns the unreaped job's
/// handle, so signals never adopt an arbitrary foreground pid/group.
#[cfg(target_os = "macos")]
#[test]
fn job_at_rest_leader() {
    use std::io::Write as _;
    use std::os::fd::BorrowedFd;
    if std::env::var_os("ATERM_TEST_VITALS_JOB_LEADER").as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        return;
    }
    // SAFETY: this helper is a new session; fd 0 is its explicitly handed
    // slave. It alone changes its signal policy and controlling terminal.
    unsafe {
        assert_eq!(
            libc::ioctl(0, libc::TIOCSCTTY as _, 0),
            0,
            "controlling terminal"
        );
        libc::signal(libc::SIGTTOU, libc::SIG_IGN);
    }
    // SAFETY: The live slave at 0 is borrowed, never transferred away.
    let slave = unsafe { BorrowedFd::borrow_raw(0) };
    let program = std::env::var("ATERM_TEST_VITALS_JOB_PROGRAM").expect("the job's program");
    let mut cmd = aterm_uds::spawnfd::command(&program);
    if program == "/bin/sleep" {
        cmd.arg(aterm_spec::wait::PARKED_SECS.to_string());
    }
    cmd.process_group(0)
        .stdin(slave.try_clone_to_owned().expect("dup slave"))
        .stdout(slave.try_clone_to_owned().expect("dup slave"))
        .stderr(slave.try_clone_to_owned().expect("dup slave"));
    struct Job(aterm_uds::spawnfd::Child);
    impl Drop for Job {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let reaped = aterm_spec::wait::within(
                aterm_spec::wait::MUST_HAPPEN,
                Duration::from_millis(10),
                || self.0.try_wait().ok().flatten(),
            );
            if reaped.is_none() {
                loop {
                    if self.0.try_wait().ok().flatten().is_some() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            if self.0.try_wait().ok().flatten().is_some() {
                println!("\nVITALS_JOB_REAPED {}", self.0.id());
                let _ = std::io::stdout().flush();
            }
        }
    }
    let job = Job(cmd.spawn().expect("spawn the owned job"));
    let pid = i32::try_from(job.0.id()).expect("pid");
    // SAFETY: This session owns the live slave and its own child group.
    assert_eq!(unsafe { libc::tcsetpgrp(0, pid) }, 0, "foreground job");
    println!("\nVITALS_JOB_READY {pid}");
    std::io::stdout().flush().expect("ready written");
    let mut p = libc::pollfd {
        fd: 2,
        events: libc::POLLIN,
        revents: 0,
    };
    let until = Instant::now() + Duration::from_secs(aterm_spec::wait::PARKED_SECS);
    loop {
        let left = i32::try_from(until.saturating_duration_since(Instant::now()).as_millis())
            .expect("small");
        // SAFETY: fd 2 is the parent's explicitly handed stop pipe. EOF
        // ends the fixture; the timeout is only its fallback parked life.
        if unsafe { libc::poll(&mut p, 1, left) } >= 0
            || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
        {
            break;
        }
    }
    drop(job);
}

/// The probe thread's part on this machine: drive `app`, no window in view, through 20 s of its
/// clock in the loop's 100 ms turns.
#[cfg(target_os = "macos")]
fn readings_at_rest(app: &mut crate::App, rx: &Receiver<Msg>, words: bool) -> Vec<u64> {
    readings_at_rest_with(app, rx, words, &mut crate::vitals_jobs::JobsPass::new())
}

/// [`readings_at_rest`] with the engine's `pass`, which the caller keeps for
/// readings after it ([`engine_reads`]).
#[cfg(target_os = "macos")]
fn readings_at_rest_with(
    app: &mut crate::App,
    rx: &Receiver<Msg>,
    words: bool,
    pass: &mut crate::vitals_jobs::JobsPass,
) -> Vec<u64> {
    let t0 = Instant::now();
    let mut asked = Vec::new();
    app.vitals.note_edge();
    for turn in 0..=200_u64 {
        let ms = turn * 100;
        let now = t0 + Duration::from_millis(ms);
        app.adopt_vitals(Audience::None, now, false);
        app.serve_windowless_vitals(Audience::None, now, false);
        for msg in rx.try_iter() {
            let Msg::Read(mut req) = msg else {
                continue;
            };
            asked.push(ms);
            if !words {
                req.ttys.clear();
            }
            let seq = u64::try_from(asked.len()).expect("few");
            engine_reads(pass, app, &req, 1_000 + ms, seq);
        }
    }
    asked
}

/// The probe thread's reading of `req` as reading `seq`, at the engine's
/// clock `at_ms`: this machine's process table through the thread's own step
/// ([`jobs_step`]), its view left for the loop with the request's words
/// ([`hand_over`]).
#[cfg(target_os = "macos")]
fn engine_reads(
    pass: &mut crate::vitals_jobs::JobsPass,
    app: &crate::App,
    req: &Request,
    at_ms: u64,
    seq: u64,
) {
    let sweep = aterm_sysprobe::processes().expect("this machine's process table");
    let jobs = jobs_step(
        pass,
        &req.tab_reads(epoch_of(at_ms), Instant::now),
        Some(&sweep.procs),
        req,
        false,
    );
    let snap = Arc::new(Snapshot {
        seq,
        jobs,
        ..Snapshot::default()
    });
    hand_over(
        &app.vitals.latest,
        &snap,
        req.ttys.iter().map(TtyWord::seen).collect(),
        Instant::now(),
    );
}

/// The probe thread's zero that puts now at `at_ms` on its clock, which the request's feeds are
/// read against.
#[cfg(target_os = "macos")]
fn epoch_of(at_ms: u64) -> Instant {
    let now = Instant::now();
    now.checked_sub(Duration::from_millis(at_ms)).unwrap_or(now)
}

/// The wait the held view names for tab 0's job, and its demand.
#[cfg(target_os = "macos")]
fn held_wait(app: &crate::App) -> (Option<aterm_vitals::jobs::WaitName>, Demand) {
    let view = app
        .vitals
        .snapshot
        .as_ref()
        .and_then(|s| s.jobs.clone())
        .expect("a jobs view");
    let wait = view
        .rows
        .iter()
        .find(|r| r.tab == 0 && r.verdict != aterm_vitals::jobs::Verdict::Exited)
        .and_then(|r| r.wait.as_ref().map(|w| w.name));
    (wait, view.demand)
}

/// With no window, a raw program at its prompt is named `tty-raw` from the loop's terminal word,
/// read once at the edge and not again in the 20 s (design §6.1, "What arms nothing").
#[cfg(target_os = "macos")]
#[test]
fn a_raw_program_at_its_prompt_is_read_once_and_then_arms_nothing() {
    use aterm_vitals::jobs::WaitName;
    static BOARD: JobsBoard = JobsBoard::new();
    let job = JobAtRest::new(true);
    let (mut app, rx) = job.app(&BOARD);
    let asked = readings_at_rest(&mut app, &rx, true);
    assert_eq!(held_wait(&app), (Some(WaitName::TtyRaw), Demand::Idle));
    assert!(
        asked.iter().all(|ms| *ms < 5_000),
        "after the edge, nothing at rest: {asked:?}"
    );
    assert_eq!(app.vitals_deadline(), None, "nothing armed at rest");

    static MAIN: JobsBoard = JobsBoard::new();
    let (mut app, rx) = job.app(&MAIN);
    let asked = readings_at_rest(&mut app, &rx, false);
    assert_eq!(held_wait(&app), (Some(WaitName::Unnamed), Demand::Fast));
    assert!(
        asked.len() >= 10 && asked.last().is_some_and(|ms| *ms >= 18_000),
        "no word: read every 2 s to the end: {asked:?}"
    );
}

/// With no window, a parked raw program that redraws each second is read at most once a floor,
/// never on a deadline, while the terminal says it still waits (design §6.1).
#[cfg(target_os = "macos")]
#[test]
fn a_parked_screens_redraw_is_read_once_a_floor_and_never_on_a_deadline() {
    static BOARD: JobsBoard = JobsBoard::new();
    let mut job = JobAtRest::new(true);
    let (mut app, rx) = job.app(&BOARD);
    let mut pass = crate::vitals_jobs::JobsPass::new();
    let _ = readings_at_rest_with(&mut app, &rx, true, &mut pass);
    let park = *app
        .vitals
        .parked
        .get(&0)
        .expect("the held view parks tab 0");
    assert_eq!(
        i32::try_from(park.pgid).ok(),
        job.job,
        "the job's own group"
    );
    let mut tick = 0_u64;
    let mut redraw = |app: &mut crate::App, at: Instant| {
        tick += 1;
        feed(app, format!("\r{tick:>6}").as_bytes());
        app.vitals_output_edge(0, at);
    };

    let mut read = Vec::new();
    for s in 1..=20 {
        let at = park.read_at + Duration::from_secs(s);
        redraw(&mut app, at);
        let reads = rx
            .try_iter()
            .filter_map(|m| match m {
                Msg::Read(r) => Some(r.jobs_now),
                _ => None,
            })
            .collect::<Vec<_>>();
        match reads.as_slice() {
            [] => {}
            [true] => read.push(s),
            other => panic!("redraw {s}: one reading that makes the engine look: {other:?}"),
        }
        assert_eq!(app.vitals.edge_at(at), None, "redraw {s}: no edge owed");
        assert_eq!(app.vitals_deadline(), None, "redraw {s}: nothing armed");
    }
    assert_eq!(
        read,
        [2, 4, 6, 8, 10, 12, 14, 16, 18, 20],
        "once a floor, each in its redraw's own wake"
    );
    let last = park.read_at + Duration::from_secs(20);
    assert_eq!(app.vitals.parked.get(&0).map(|p| p.read_at), Some(last));

    app.vitals.proc_edge.install(|| {});
    app.vitals.arm_proc_edge();
    assert!(
        app.vitals.proc_edge.heard(),
        "a process event: a fork, an exec or an exit the kernel watch hears"
    );
    let heard = last + JOBS_FLOOR;
    app.service_vitals(heard);
    assert!(read_req(&rx, "past the floor: read at once").jobs_now);
    let inside = heard + Duration::from_secs(1);
    redraw(&mut app, inside);
    assert_eq!(
        asked(&rx),
        0,
        "a recheck inside another edge's floor: nothing read"
    );
    assert_eq!(app.vitals.edge_at(inside), None, "and nothing owed");
    assert_eq!(app.vitals_deadline(), None, "or armed");
    let past = heard + JOBS_FLOOR;
    redraw(&mut app, past);
    assert!(read_req(&rx, "the recheck, in its own wake").jobs_now);
    assert_eq!(app.vitals.parked.get(&0).map(|p| p.read_at), Some(past));

    let held = app.vitals.parked.clone();
    let sink = Arc::clone(&app.pool.get(0).expect("tab 0").ctx.sink);
    let mut at = past;
    let mut control = |app: &mut crate::App, why: &str, change: &dyn Fn(&mut crate::App)| {
        at += JOBS_FLOOR;
        app.vitals.parked.clone_from(&held);
        if let Some(p) = app.vitals.parked.get_mut(&0) {
            p.read_at = at;
        }
        change(app);
        redraw(app, at);
        assert!(read_req(&rx, why).jobs_now, "{why}");
        assert!(
            !app.vitals.parked.contains_key(&0),
            "{why}: the park dropped"
        );
    };
    control(&mut app, "another group in front", &|app| {
        app.vitals.parked.get_mut(&0).expect("parked").pgid += 1;
    });
    let master = job.master();
    let set_mode = |canonical: bool, echo: bool| {
        // SAFETY: This test owns the live PTY and its termios.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(master, &mut t), 0, "tcgetattr");
            t.c_lflag &= !(libc::ICANON | libc::ECHO);
            if canonical {
                t.c_lflag |= libc::ICANON;
            }
            if echo {
                t.c_lflag |= libc::ECHO;
            }
            assert_eq!(libc::tcsetattr(master, libc::TCSANOW, &t), 0);
        }
    };
    control(&mut app, "the terminal canonical again", &|_| {
        set_mode(true, true)
    });
    control(&mut app, "echo on again, the read still raw", &|_| {
        set_mode(false, true)
    });
    set_mode(false, false);
    control(&mut app, "a key driven since the word", &|_| {
        assert_eq!(sink.write_frame(b"k").expect("typed"), 1);
    });

    app.vitals.parked.clone_from(&held);
    assert!(job.stop(), "the job ended and was reaped");
    at += JOBS_FLOOR;
    if let Some(p) = app.vitals.parked.get_mut(&0) {
        p.read_at = at;
    }
    feed(&app, b"\r\n$ ");
    app.vitals_output_edge(0, at);
    let req = read_req(&rx, "the end's prompt: read at once");
    assert!(req.jobs_now);
    assert!(!app.vitals.parked.contains_key(&0), "the park dropped");
    engine_reads(&mut pass, &app, &req, 1_000 + 600_000, 1_000);
    app.adopt_vitals(Audience::None, at, false);
    let view = app
        .vitals
        .snapshot
        .as_ref()
        .and_then(|s| s.jobs.clone())
        .expect("the end's view");
    assert!(
        view.rows
            .iter()
            .filter(|r| r.tab == 0)
            .all(|r| r.verdict == aterm_vitals::jobs::Verdict::Exited),
        "the reading finds the job exited: {:?}",
        view.rows
    );
    assert!(app.vitals.parked.is_empty(), "and parks nothing");

    static CANONICAL: JobsBoard = JobsBoard::new();
    let canonical = JobAtRest::new(false);
    let (mut app, rx) = canonical.app(&CANONICAL);
    let _ = readings_at_rest(&mut app, &rx, true);
    assert!(app.vitals.parked.is_empty(), "a canonical job: no park");
    let at = Instant::now() + 30 * JOBS_FLOOR;
    feed(&app, b"\r tick");
    app.vitals_output_edge(0, at);
    assert!(read_req(&rx, "its redraw is an edge").jobs_now);
}

/// What a [`ThreadEngine`] reading saw: whether a worker's pull asked it, the wait its view names
/// for tab 0's live job, and whether the view parks tab 0 at the person's read.
#[cfg(target_os = "macos")]
type EngineSaw = (bool, Option<aterm_vitals::jobs::WaitName>, bool);

/// What a [`ThreadEngine`] does once, on the probe thread, right after the first reading a pull
/// asks of it: a write between a cold pull's pair of readings.
#[cfg(target_os = "macos")]
type Between = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;

/// A probe thread's source that steps the stuck engine over this machine's process table, as the
/// thread's [`Reader`] does, and keeps what each reading saw. Its clock runs a minute ahead at
/// each reading, so the engine, which looks again only once its own cadence is due (a pull asks
/// with `jobs_now` off), looks at every one.
#[cfg(target_os = "macos")]
struct ThreadEngine {
    vitals: aterm_vitals::Vitals,
    pass: crate::vitals_jobs::JobsPass,
    t0: Instant,
    seq: u64,
    saw: Arc<Mutex<Vec<EngineSaw>>>,
    /// Whether every pull is cold ([`Source::split_cold`]): read as a pair, [`COLD_PAIR`] apart.
    cold: bool,
    between: Between,
}

#[cfg(target_os = "macos")]
impl ThreadEngine {
    fn new(saw: Arc<Mutex<Vec<EngineSaw>>>) -> Self {
        Self {
            vitals: aterm_vitals::Vitals::new(),
            pass: crate::vitals_jobs::JobsPass::new(),
            t0: Instant::now(),
            seq: 0,
            saw,
            cold: false,
            between: Between::default(),
        }
    }

    /// An engine whose every pull is a cold pair, running `between` after the first reading a pull
    /// asks of it.
    fn cold(saw: Arc<Mutex<Vec<EngineSaw>>>, between: Between) -> Self {
        Self {
            cold: true,
            between,
            ..Self::new(saw)
        }
    }
}

#[cfg(target_os = "macos")]
impl Source for ThreadEngine {
    fn reading(&mut self, req: &Request) -> Snapshot {
        self.seq += 1;
        let sweep = aterm_sysprobe::processes().expect("this machine's process table");
        let at_ms = self.seq * 60_000 + u64::try_from(self.t0.elapsed().as_millis()).unwrap_or(0);
        let jobs = jobs_step(
            &mut self.pass,
            &req.tab_reads(epoch_of(at_ms), Instant::now),
            Some(&sweep.procs),
            req,
            false,
        );
        let wait = jobs.as_ref().and_then(|view| {
            view.rows
                .iter()
                .find(|r| r.tab == 0 && r.verdict != aterm_vitals::jobs::Verdict::Exited)
                .and_then(|r| r.wait.as_ref().map(|w| w.name))
        });
        let parks = jobs
            .as_ref()
            .is_some_and(|view| view.parked_tabs.contains(&0));
        self.saw
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((req.pull, wait, parks));
        if req.pull {
            let between = self
                .between
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            if let Some(between) = between {
                between();
            }
        }
        Snapshot {
            seq: self.seq,
            jobs,
            ..Snapshot::default()
        }
    }
    fn model(&mut self) -> &mut aterm_vitals::Vitals {
        &mut self.vitals
    }
    fn split_cold(&self) -> bool {
        self.cold
    }
}

/// An app over `job`'s terminal whose real probe thread ([`run`]) reads with a [`ThreadEngine`]
/// keeping what it saw in `saw`, and the park its first reading at an edge makes of tab 0.
#[cfg(target_os = "macos")]
fn parked_by_the_thread(
    job: &JobAtRest,
    board: &'static JobsBoard,
    saw: &Arc<Mutex<Vec<EngineSaw>>>,
) -> (crate::App, Parked) {
    let saw = Arc::clone(saw);
    parked_by_the_thread_with(job, board, move || ThreadEngine::new(saw))
}

/// [`parked_by_the_thread`] with the thread's engine `engine` makes.
#[cfg(target_os = "macos")]
fn parked_by_the_thread_with(
    job: &JobAtRest,
    board: &'static JobsBoard,
    engine: impl FnOnce() -> ThreadEngine + Send + 'static,
) -> (crate::App, Parked) {
    let sink = Arc::new(aterm_session::sink::SinkWriter::new(job.master()));
    let mut app = crate::App::headless_for_test_with_sink(sink);
    app.pool.sessions.get_mut(&0).expect("tab 0").session.pid =
        i32::try_from(job.leader.as_ref().expect("owned leader").id()).expect("pid");
    app.vitals.board = board;
    app.vitals.start(engine, || true, None, None);
    assert!(app.vitals.parked.is_empty(), "nothing read: nothing parked");
    app.vitals.note_edge();
    let park = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(10),
        || {
            let now = Instant::now();
            app.adopt_vitals(Audience::None, now, false);
            app.serve_windowless_vitals(Audience::None, now, false);
            app.vitals.parked.get(&0).copied()
        },
    )
    .expect("the thread's reading parks tab 0");
    (app, park)
}

/// The real probe thread ([`run`]) reads a raw program at the person's read and hands its view
/// back with the request's words; the loop names the park from them (the job's group, the word's
/// raw mode and the input driven before it).
#[cfg(target_os = "macos")]
#[test]
fn the_thread_hands_back_the_words_it_read_with_and_the_loop_parks_from_them() {
    static BOARD: JobsBoard = JobsBoard::new();
    let job = JobAtRest::new(true);
    let (mut app, park) = parked_by_the_thread(&job, &BOARD, &Arc::default());
    let input = app.pool.get(0).expect("tab 0").ctx.sink.driven_input();
    assert_eq!(
        i32::try_from(park.pgid).ok(),
        job.job,
        "the job's own group"
    );
    assert_eq!(
        park.word,
        WordSeen {
            key: 0,
            canonical: false,
            echo: false,
            input,
        },
        "the word the request carried"
    );
    assert!(
        app.vitals.stop_at_exit(aterm_spec::wait::MUST_HAPPEN),
        "the thread ended"
    );
}

/// A worker's pull reads each terminal's word afresh. The loop parks a raw program from the
/// reading that named its wait and builds no request after it, so a redraw past that reading (an
/// agent's last of its turn) left the word the thread held stamped before that output: the first
/// `sys jobs` after a turn named no wait, and the loop read again 2 s later before it rested.
#[cfg(target_os = "macos")]
#[test]
fn a_pull_after_the_last_redraw_reads_the_word_afresh_and_arms_nothing() {
    use aterm_vitals::jobs::WaitName;
    static BOARD: JobsBoard = JobsBoard::new();
    let job = JobAtRest::new(true);
    let saw = Arc::default();
    let (mut app, _) = parked_by_the_thread(&job, &BOARD, &saw);
    assert_eq!(held_wait(&app), (Some(WaitName::TtyRaw), Demand::Idle));
    // The redraw after the reading that parked it, stamped as long before the pull as a pull a
    // minute after the turn finds it: the program has been quiet since.
    feed_tab_at(&app, 0, b"x", || {
        let now = Instant::now();
        now.checked_sub(Duration::from_secs(50)).unwrap_or(now)
    });
    let before = saw.lock().unwrap_or_else(PoisonError::into_inner).len();
    assert!(app.vitals.offer(Msg::Pull), "a worker's pull");
    aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(10),
        || {
            app.adopt_vitals(Audience::None, Instant::now(), false)
                .then_some(())
        },
    )
    .expect("the pull's view lands");
    let pulled = saw.lock().unwrap_or_else(PoisonError::into_inner)[before..].to_vec();
    assert!(
        !pulled.is_empty()
            && pulled
                .iter()
                .all(|r| *r == (true, Some(WaitName::TtyRaw), true)),
        "the pull parks the program's read from a word read after the redraw: {pulled:?}"
    );
    assert_eq!(held_wait(&app), (Some(WaitName::TtyRaw), Demand::Idle));
    assert!(app.vitals.parked.contains_key(&0), "still parked");
    assert_eq!(app.vitals.jobs_due, None, "no reading armed after the pull");
    assert!(
        app.vitals.stop_at_exit(aterm_spec::wait::MUST_HAPPEN),
        "the thread ended"
    );
}

/// A cold pull reads twice, [`COLD_PAIR`] apart, and a redraw between its two readings is past
/// every word read when the pull began: each reading of a pull reads the words afresh, so its last
/// still names the person's read, the view it hands back parks it, and nothing is armed.
#[cfg(target_os = "macos")]
#[test]
fn a_redraw_between_a_cold_pulls_pair_is_read_past_by_its_last_reading() {
    use aterm_vitals::jobs::WaitName;
    static BOARD: JobsBoard = JobsBoard::new();
    let job = JobAtRest::new(true);
    let saw: Arc<Mutex<Vec<EngineSaw>>> = Arc::default();
    let between = Between::default();
    let (mut app, _) = parked_by_the_thread_with(&job, &BOARD, {
        let (saw, between) = (Arc::clone(&saw), Arc::clone(&between));
        move || ThreadEngine::cold(saw, between)
    });
    assert_eq!(held_wait(&app), (Some(WaitName::TtyRaw), Demand::Idle));
    let s = app.pool.get(0).expect("tab 0");
    let (term, feed, moves) = (
        Arc::clone(&s.term),
        Arc::clone(&s.ctx.job_feed),
        Arc::clone(&s.ctx.screen_moves),
    );
    // The redraw lands on the probe thread between the pull's warm reading and its last, stamped
    // as old as a quiet program's: only the word's own reading can say it is past it.
    *between.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(move || {
        feed.read(b"x");
        let mut t = term.lock().unwrap_or_else(PoisonError::into_inner);
        t.process(b"x");
        let moved = moves.fed(crate::control::screen_gen(&t));
        assert!(moved, "the redraw moves the screen");
        feed.fed(&t, b"x", moved, || {
            let now = Instant::now();
            now.checked_sub(Duration::from_secs(50)).unwrap_or(now)
        });
    }));
    let before = saw.lock().unwrap_or_else(PoisonError::into_inner).len();
    assert!(app.vitals.offer(Msg::Pull), "a worker's pull");
    aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(10),
        || {
            app.adopt_vitals(Audience::None, Instant::now(), false)
                .then_some(())
        },
    )
    .expect("the pull's view lands");
    let pulled = saw.lock().unwrap_or_else(PoisonError::into_inner)[before..].to_vec();
    assert!(
        between
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none(),
        "the redraw ran inside the pull"
    );
    assert_eq!(
        pulled,
        [
            (true, Some(WaitName::TtyRaw), true),
            (true, Some(WaitName::TtyRaw), true)
        ],
        "both readings of the cold pull name the read; the last one reads the word past the \
         redraw"
    );
    assert_eq!(held_wait(&app), (Some(WaitName::TtyRaw), Demand::Idle));
    assert!(app.vitals.parked.contains_key(&0), "still parked");
    assert_eq!(app.vitals.jobs_due, None, "no reading armed after the pull");
    assert!(
        app.vitals.stop_at_exit(aterm_spec::wait::MUST_HAPPEN),
        "the thread ended"
    );
}

/// [`Parked::holds`] only while the same group is in front, `ICANON` and `ECHO` are unchanged,
/// and no input was driven since the word.
#[cfg(unix)]
#[test]
fn a_park_holds_only_while_the_terminal_says_the_program_still_waits() {
    let (_read, write) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let sink = aterm_session::sink::SinkWriter::new(std::os::fd::AsRawFd::as_raw_fd(&write));
    let input = sink.driven_input();
    let park = Parked {
        pgid: 42,
        word: WordSeen {
            key: 0,
            canonical: false,
            echo: false,
            input,
        },
        read_at: Instant::now(),
    };
    assert!(
        park.holds(42, Some((false, false)), input),
        "it still waits"
    );
    assert!(
        !park.holds(43, Some((false, false)), input),
        "another group"
    );
    assert!(
        !park.holds(42, Some((true, false)), input),
        "canonical again"
    );
    assert!(!park.holds(42, Some((false, true)), input), "echo on again");
    assert!(!park.holds(42, None, input), "no terminal to ask");
    assert_eq!(sink.write_frame(b"k").expect("a key"), 1);
    assert!(
        !park.holds(42, Some((false, false)), sink.driven_input()),
        "a key driven since"
    );
}

#[test]
fn a_park_is_named_from_its_own_tabs_word() {
    use aterm_vitals::jobs::Verdict;
    let input = aterm_session::sink::SinkWriter::new(-1).driven_input();
    let word = |key, canonical| WordSeen {
        key,
        canonical,
        echo: canonical,
        input,
    };
    let row = |tab, pgid, verdict| JobRow {
        tab,
        pgid,
        ..crate::stats_paint::tests::job_row("j", verdict, None, Some(1_000))
    };
    let view = JobsView {
        rows: vec![
            row(1, 7, Verdict::Fine),
            row(0, 42, Verdict::Fine),
            row(0, 41, Verdict::Exited),
            row(2, 9, Verdict::Fine),
            row(2, 10, Verdict::Fine),
            row(3, 11, Verdict::Fine),
        ],
        parked_tabs: vec![0, 2, 3],
        ..JobsView::default()
    };
    let words = [word(1, true), word(2, false), word(0, false)];
    let now = Instant::now();
    let parks = Parked::of(&view, &words, now);
    assert_eq!(
        parks.keys().copied().collect::<Vec<_>>(),
        [0],
        "tab 0 alone"
    );
    assert_eq!(
        parks.get(&0),
        Some(&Parked {
            pgid: 42,
            word: word(0, false),
            read_at: now,
        }),
        "its own word and its live job's group"
    );
}

/// The word silences only a person's own wait: a job that keeps the
/// terminal canonical with echo (`sleep` typed at a prompt) names no tty
/// wait from it, stays suspect (unnamed, §11.5), and is read every 2 s with
/// no window, word or no word (§6.1 kind 3).
#[cfg(target_os = "macos")]
#[test]
fn a_canonical_job_at_rest_is_still_read_at_the_engines_cadence() {
    use aterm_vitals::jobs::WaitName;
    static BOARD: JobsBoard = JobsBoard::new();
    let job = JobAtRest::new(false);
    let (mut app, rx) = job.app(&BOARD);
    let asked = readings_at_rest(&mut app, &rx, true);
    assert_eq!(held_wait(&app), (Some(WaitName::Unnamed), Demand::Fast));
    assert!(
        asked.len() >= 10 && asked.last().is_some_and(|ms| *ms >= 18_000),
        "a suspect job is read to the end: {asked:?}"
    );
}

/// Read through the sink, the word is stamped newer than every output until the session's next
/// output, then stamped 0 (mode kept, no wait named); a key unread yields no word (§6.6).
#[cfg(target_os = "macos")]
#[test]
fn a_terminal_word_holds_until_output_and_never_over_an_unread_key() {
    let mut failed = None;
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = JobAtRest::starting(true, |guard| {
            guard.ready();
            let leader = i32::try_from(guard.leader.as_ref().expect("leader").id()).expect("pid");
            let job = guard.job.expect("ready job");
            failed = Some([
                (
                    leader,
                    aterm_uds::process::read_process_start(leader).expect("leader start"),
                ),
                (
                    job,
                    aterm_uds::process::read_process_start(job).expect("job start"),
                ),
            ]);
            panic!("injected failure before constructor admission");
        });
    }));
    assert!(failure.is_err(), "the failure seam ran");
    for (pid, start) in failed.expect("both original identities captured") {
        assert_ne!(
            aterm_uds::process::read_process_start(pid),
            Some(start),
            "original process {pid} survived failed construction"
        );
    }

    let mut job = JobAtRest::new(true);
    let sink = aterm_session::sink::SinkWriter::new(job.master());
    let output = Arc::new(ScreenMoves::default());
    output.fed(drawn(5));
    let word = TtyWord::read(3, &sink, &output).expect("a terminal's word");
    assert_eq!((word.key, word.canonical, word.echo), (3, false, false));
    assert_eq!(
        word.current()
            .map(|(key, m)| (key, m.canonical, m.echo, m.stamped_ms)),
        Some((3, false, false, u64::MAX))
    );
    output.fed(drawn(6));
    assert_eq!(
        word.current()
            .map(|(key, m)| (key, m.canonical, m.echo, m.stamped_ms)),
        Some((3, false, false, 0)),
        "output since the read: the mode holds, stamped too old to name a wait"
    );

    // SAFETY: Writes one byte of a local buffer to this test's master.
    let n = unsafe { libc::write(job.master(), b"x".as_ptr().cast(), 1) };
    assert_eq!(n, 1, "typed");
    aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(10),
        || aterm_pty::input_queue_len(job.master()).filter(|n| *n > 0),
    )
    .expect("the key waits unread");
    assert!(
        TtyWord::read(3, &sink, &output).is_none(),
        "a key unread: no word"
    );
    let none = aterm_session::sink::SinkWriter::new(-1);
    assert!(
        TtyWord::read(3, &none, &output).is_none(),
        "no terminal: no word"
    );
    assert!(
        job.stop(),
        "the parent reaped its job, the leader was reaped and stdout reached EOF: {}",
        job.said
    );
}

/// The wait a whole [`Reader::read`] of `req`, by a fresh reader (as the
/// probe thread reads one, and a worker's pull with the request it holds),
/// names for tab 0's live job, and the engine's demand.
#[cfg(target_os = "macos")]
fn read_wait(req: &Request) -> (Option<aterm_vitals::jobs::WaitName>, Demand) {
    let snap = Reader::new(Arc::default()).read(req);
    let view = snap.jobs.expect("a jobs view");
    let wait = view
        .rows
        .iter()
        .find(|r| r.tab == 0 && r.verdict != aterm_vitals::jobs::Verdict::Exited)
        .and_then(|r| r.wait.as_ref().map(|w| w.name));
    (wait, view.demand)
}

/// One [`Reader::read`] of the request the loop builds for a raw program at rest names it
/// `tty-raw` and parks the engine.
#[cfg(target_os = "macos")]
#[test]
fn the_readers_whole_reading_names_a_raw_program_from_the_loops_word() {
    use aterm_vitals::jobs::WaitName;
    static BOARD: JobsBoard = JobsBoard::new();
    let job = JobAtRest::new(true);
    let (app, _rx) = job.app(&BOARD);
    let req = app.vitals_request(Duration::ZERO, false);
    assert_eq!(
        req.ttys.iter().map(|w| w.key).collect::<Vec<_>>(),
        [0],
        "tab 0's word"
    );
    assert_eq!(read_wait(&req), (Some(WaitName::TtyRaw), Demand::Idle));
    feed(&app, b"x");
    assert_eq!(read_wait(&req), (Some(WaitName::Unnamed), Demand::Fast));
}

/// A line `ping` prints, fed on a terminal holding no block.
#[cfg(target_os = "macos")]
const PING_LINE: &[u8] = b"64 bytes from 127.0.0.1: icmp_seq=1\r\n";

/// A line the tab printed between two passes is its foreground job's progress on the probe
/// thread's clock (§6.6): the job runs for 5 s after it, then is quiet only since it.
#[cfg(target_os = "macos")]
#[test]
fn a_line_the_tab_printed_between_passes_is_its_jobs_progress() {
    use aterm_vitals::jobs::Verdict;
    static BOARD: JobsBoard = JobsBoard::new();
    let job = JobAtRest::writer();
    let (app, _rx) = job.app(&BOARD);
    let mut req = app.vitals_request(Duration::ZERO, false);
    req.jobs_now = true;
    let epoch = Instant::now();
    let ms = |ms| epoch + Duration::from_millis(ms);
    let sweep = aterm_sysprobe::processes().expect("this machine's process table");
    let mut pass = crate::vitals_jobs::JobsPass::new();
    let mut quiet = |at_ms| {
        let view = jobs_step(
            &mut pass,
            &req.tab_reads(epoch, || ms(at_ms)),
            Some(&sweep.procs),
            &req,
            false,
        )
        .expect("a jobs view");
        view.rows
            .iter()
            .find(|r| r.tab == 0 && r.verdict != Verdict::Exited)
            .and_then(|r| r.quiet_ms)
    };
    let line = |at_ms| {
        app.pool.get(0).expect("tab 0").ctx.job_feed.fed(
            &aterm_core::terminal::Terminal::new(24, 80),
            PING_LINE,
            true,
            || ms(at_ms),
        );
    };
    assert_eq!(quiet(1_000), Some(0), "found at rest");
    job.poke();
    line(5_000);
    assert_eq!(quiet(8_000), None, "running: the line is 3 s old");
    assert_eq!(quiet(12_000), Some(7_000), "quiet since the line");
}

/// Keys driven into a tab reach the engine as `JobTab::typed_ms`, stamped with the clock of the
/// pass that found them, which a session nested in the tab waits on before its terminal's word is
/// the person's (§6.6).
#[cfg(target_os = "macos")]
#[test]
fn a_pass_that_finds_keys_driven_into_a_tab_stamps_its_feed() {
    static BOARD: JobsBoard = JobsBoard::new();
    let job = JobAtRest::writer();
    let (app, _rx) = job.app(&BOARD);
    let req = app.vitals_request(Duration::ZERO, false);
    let epoch = Instant::now();
    let typed = |at_ms| {
        req.tab_reads(epoch, || epoch + Duration::from_millis(at_ms))
            .feeds
            .iter()
            .find(|(key, _)| *key == 0)
            .and_then(|(_, f)| f.typed_ms)
    };
    assert_eq!(typed(1_000), None, "no key yet");
    let sink = &app.pool.get(0).expect("tab 0").ctx.sink;
    assert_eq!(sink.write_frame(b"x\n").expect("a key"), 2);
    assert_eq!(typed(3_000), Some(3_000), "the pass that found it");
}

#[cfg(target_os = "macos")]
thread_local! {
    /// What [`MID_SWEEP`]'s sweep does before it sweeps, once.
    static AT_SWEEP: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// The kernel's probe, with a sweep that first runs [`AT_SWEEP`]'s hook.
#[cfg(target_os = "macos")]
const MID_SWEEP: CoalProbe = CoalProbe {
    sweep: |opts, ids| {
        if let Some(hook) = AT_SWEEP.with(|h| h.borrow_mut().take()) {
            hook();
        }
        aterm_sysprobe::processes_keeping(opts, ids)
    },
    ..CoalProbe::KERNEL
};

/// What a tab's reader reads while the sweep runs is the next reading's (§6.6), so a `D` and its
/// command's exit read mid-sweep never meet as a closed block over a live member, and a line
/// printed then counts at the next reading.
#[cfg(target_os = "macos")]
#[test]
fn a_reading_takes_its_tabs_before_its_sweep() {
    use aterm_vitals::jobs::Verdict;
    static BOARD: JobsBoard = JobsBoard::new();
    let job = JobAtRest::writer();
    let (app, _rx) = job.app(&BOARD);
    let mut req = app.vitals_request(Duration::ZERO, false);
    req.jobs_now = true;
    let mut reader = Reader::new(Arc::default());
    reader.split = SplitReader::new(MID_SWEEP, Box::leak(Box::new(Arming::new())));
    let mut quiet = || {
        let view = reader.read(&req).jobs.expect("a jobs view");
        view.rows
            .iter()
            .find(|r| r.tab == 0 && r.verdict != Verdict::Exited)
            .map(|r| r.quiet_ms.is_some())
    };
    assert_eq!(quiet(), Some(true), "found at rest");
    let feed = Arc::clone(&app.pool.get(0).expect("tab 0").ctx.job_feed);
    job.poke();
    AT_SWEEP.with(|h| {
        *h.borrow_mut() = Some(Box::new(move || {
            feed.fed(
                &aterm_core::terminal::Terminal::new(24, 80),
                PING_LINE,
                true,
                Instant::now,
            );
        }));
    });
    assert_eq!(
        quiet(),
        Some(true),
        "the line came after the tabs were read"
    );
    assert_eq!(quiet(), Some(false), "the next reading's progress");
}

/// A write that moves no cell (Muse Code's colour reset every ~9 s at rest,
/// `docs/measured/vitals-3-2026-10-22.md`) leaves the word standing, so no 2 s `Demand::Fast`
/// reading arms; a write that draws leaves the word no wait to name.
#[cfg(target_os = "macos")]
#[test]
fn a_write_that_moves_no_cell_leaves_the_word_standing() {
    use aterm_vitals::jobs::WaitName;
    static BOARD: JobsBoard = JobsBoard::new();
    let job = JobAtRest::new(true);
    let (app, _rx) = job.app(&BOARD);
    feed(&app, b"$ ");
    let req = app.vitals_request(Duration::ZERO, false);
    assert_eq!(read_wait(&req), (Some(WaitName::TtyRaw), Demand::Idle));
    for n in 1..=3 {
        feed(&app, MUSE_RESET);
        app.pool
            .get(0)
            .expect("tab 0")
            .latest_output_activity_ns
            .fetch_add(1, Ordering::AcqRel);
        assert_eq!(
            read_wait(&req),
            (Some(WaitName::TtyRaw), Demand::Idle),
            "reset {n}: the word stands, nothing asked"
        );
    }
    feed(&app, b"x");
    assert_eq!(
        read_wait(&req),
        (Some(WaitName::Unnamed), Demand::Fast),
        "a write that drew: the word names no wait"
    );
}

/// A key typed into the session's sink after the word was read voids the word for a worker's
/// pull: the wait reads unnamed, never `tty-raw` (§11.5: the tty names need an empty queue).
#[cfg(target_os = "macos")]
#[test]
fn a_key_driven_after_the_word_was_read_voids_it_for_a_pull() {
    use aterm_vitals::jobs::WaitName;
    static BOARD: JobsBoard = JobsBoard::new();
    let job = JobAtRest::new(true);
    let (app, _rx) = job.app(&BOARD);
    let held = app.vitals_request(Duration::ZERO, false);
    assert!(held.ttys[0].current().is_some(), "the word holds");
    assert_eq!(read_wait(&held), (Some(WaitName::TtyRaw), Demand::Idle));
    let sink = Arc::clone(&app.pool.get(0).expect("tab 0").ctx.sink);
    assert_eq!(sink.write_frame(b"k").expect("typed"), 1);
    aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(10),
        || aterm_pty::input_queue_len(job.master()).filter(|n| *n > 0),
    )
    .expect("the key waits unread");
    assert_eq!(held.ttys[0].current(), None, "a key since: no word");
    assert_eq!(read_wait(&held), (Some(WaitName::Unnamed), Demand::Fast));
}

/// A terminal whose unread input nothing measures (a platform with no count, a slave that cannot
/// be opened for one) gives no word, as one with a key waiting does: an empty queue is what lets
/// the tty name a wait for the person, so an unknown one never stands in for it, and the engine
/// keeps reading the job.
#[cfg(target_os = "macos")]
#[test]
fn a_terminal_whose_unread_input_is_unknown_gives_no_word() {
    let job = JobAtRest::new(true);
    let sink = aterm_session::sink::SinkWriter::new(job.master());
    let output = Arc::new(ScreenMoves::default());
    let read = |unread: Option<usize>| {
        TtyWord::read_seamed(
            3,
            &sink,
            &output,
            || {},
            |read: &aterm_session::sink::SinkWriter| {
                assert_eq!(read.master(), job.master(), "the session's own sink");
                unread
            },
        )
        .map(|w| (w.canonical, w.echo))
    };
    assert_eq!(
        read(Some(0)),
        Some((false, false)),
        "nothing unread: the word"
    );
    assert_eq!(read(Some(1)), None, "a key unread: no word");
    assert_eq!(read(None), None, "unread input unknown: no word");
}

/// A request holds each session's sink weakly ([`TtySource`]). The probe thread keeps its last
/// request until the next one lands, and a closed tab's master closes with its session, so the
/// program's terminal hangs up then, not when that request goes; the request reads no word of it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_request_the_thread_keeps_never_holds_a_closed_sessions_master_open() {
    use std::os::fd::FromRawFd as _;
    let (master, slave) = aterm_pty::open_pty_pair().expect("a pty");
    // SAFETY: `master` is the pair's fresh master and the sink below its only owner.
    let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(master) };
    let mut session = crate::stub_session_with_sink(
        1,
        Arc::new(aterm_session::sink::SinkWriter::new_owned(owned)),
    );
    session.pid = i32::try_from(std::process::id()).expect("pid");
    let mut app = crate::App::headless_for_test();
    crate::App::register_session(&app.store, &session, None);
    app.pool.insert(session);
    let hung_up = || {
        let mut fd = libc::pollfd {
            fd: slave,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one live `pollfd` of this test's own slave, and no wait.
        let rc = unsafe { libc::poll(&raw mut fd, 1, 0) };
        assert!(rc >= 0, "poll the slave");
        fd.revents & libc::POLLHUP != 0
    };
    let req = app.vitals_request(Duration::from_secs(2), false);
    let source = req
        .tty_sources
        .iter()
        .find(|s| s.key == 1)
        .expect("the session's source");
    assert!(source.read().is_some(), "the live session's word");
    assert!(!hung_up(), "the master is open while its session lives");
    let _ = app
        .store
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .deregister_local(1);
    drop(app.pool.sessions.remove(&1));
    // A child another test forks holds a copy of every descriptor until it execs, so the hangup
    // may come a moment later; nothing in the request may hold it off.
    assert!(
        aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            Duration::from_millis(5),
            || hung_up().then_some(())
        )
        .is_some(),
        "the closed session's master closed under the request still held"
    );
    assert!(source.read().is_none(), "a gone session reads no word");
    drop(req);
    // SAFETY: `slave` is this test's own descriptor, closed once.
    assert_eq!(unsafe { libc::close(slave) }, 0);
}

/// Output landing between the word's stamps and its `tcgetattr` leaves the word its mode but
/// stamps it before all output, so it names no wait (§6.6).
#[cfg(target_os = "macos")]
#[test]
fn the_word_takes_its_stamps_before_it_reads_the_terminal() {
    let job = JobAtRest::new(true);
    let sink = aterm_session::sink::SinkWriter::new(job.master());
    let output = Arc::new(ScreenMoves::default());
    output.fed(drawn(5));
    let word = TtyWord::read_seamed(
        3,
        &sink,
        &output,
        || {
            output.fed(drawn(6));
        },
        aterm_session::sink::SinkWriter::unread_input,
    )
    .expect("a terminal's word");
    let stamp = |w: &TtyWord| w.current().map(|(_, m)| m.stamped_ms);
    assert_eq!(stamp(&word), Some(0), "output mid-read: no wait from it");
    output.fed(drawn(7));
    let word = TtyWord::read_seamed(
        3,
        &sink,
        &output,
        || {},
        aterm_session::sink::SinkWriter::unread_input,
    )
    .expect("a terminal's word");
    assert_eq!(
        stamp(&word),
        Some(u64::MAX),
        "output before the read: newer than all of it"
    );
}

#[test]
fn a_cold_pulls_first_reading_hands_its_events_to_the_published_one() {
    use aterm_vitals::events::{Event, EventKind};
    use aterm_vitals::jobs::{Class, Verdict};
    let job = |seq: u64, to| Event {
        seq,
        at_ns: 0,
        kind: EventKind::Job {
            tab: 1,
            id: "j7".into(),
            from: Some(Verdict::Fine),
            to,
            class: Class::Exited,
            task: None,
            exit: Some(0),
        },
    };
    let warm = Snapshot {
        seq: 4,
        events: vec![job(4, Verdict::Exited)],
        ..Snapshot::default()
    };
    let mut snap = Snapshot {
        seq: 5,
        events: vec![job(5, Verdict::Stuck)],
        ..Snapshot::default()
    };
    keep_warm_events(warm, &mut snap);
    let seqs: Vec<u64> = snap.events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![4, 5], "the warm reading's first, none lost");
    let slot = Mutex::new(Handover::default());
    assert!(
        hand_over(&slot, &Arc::new(snap), Vec::new(), Instant::now()),
        "the loop hears of it"
    );
}

#[test]
fn the_hand_over_keeps_job_events_for_the_loop_and_says_when_one_moved() {
    use aterm_vitals::events::{Event, EventKind};
    use aterm_vitals::jobs::{Class, Verdict};
    let slot = Mutex::new(Handover::default());
    let snap = |seq: u64, job_event: bool| {
        let mut s = Snapshot {
            seq,
            ..Snapshot::default()
        };
        if job_event {
            s.events.push(Event {
                seq,
                at_ns: 0,
                kind: EventKind::Job {
                    tab: 1,
                    id: format!("a{seq}"),
                    from: Some(Verdict::Fine),
                    to: Verdict::Stuck,
                    class: Class::Waiting,
                    task: None,
                    exit: None,
                },
            });
        }
        Arc::new(s)
    };
    assert!(
        !hand_over(&slot, &snap(1, false), Vec::new(), Instant::now()),
        "nothing moved"
    );
    assert!(hand_over(&slot, &snap(2, true), Vec::new(), Instant::now()));
    assert!(hand_over(&slot, &snap(3, true), Vec::new(), Instant::now()));
    let mut v = VitalsHost {
        latest: Arc::new(slot),
        ..VitalsHost::default()
    };
    assert!(v.adopt(Instant::now()));
    assert_eq!(v.snapshot.as_ref().map(|s| s.seq), Some(3));
    let events = v.take_job_events();
    assert_eq!(events.len(), 2, "both snapshots' events: {events:?}");
    assert_eq!(
        (v.next_job_rev(1), v.next_job_rev(1), v.next_job_rev(2)),
        (1, 2, 1)
    );
}

#[test]
fn a_pull_reads_whole_and_the_quit_takes_the_workers_sender_back() {
    static COPY: std::sync::LazyLock<crate::sys_host::Shared> =
        std::sync::LazyLock::new(crate::sys_host::Shared::new);
    struct Whole {
        vitals: aterm_vitals::Vitals,
        seen: Arc<Mutex<Vec<(bool, bool)>>>,
        seq: u64,
    }
    impl Source for Whole {
        fn reading(&mut self, req: &Request) -> Snapshot {
            self.seen
                .lock()
                .unwrap()
                .push((!req.machine_only, req.jobs_now));
            self.seq += 1;
            Snapshot {
                seq: self.seq,
                ..Snapshot::default()
            }
        }
        fn model(&mut self) -> &mut aterm_vitals::Vitals {
            &mut self.vitals
        }
    }
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut host = VitalsHost::default();
    let held = Arc::clone(&seen);
    host.start(
        move || Whole {
            vitals: aterm_vitals::Vitals::new(),
            seen: held,
            seq: 0,
        },
        || true,
        None,
        Some(&COPY),
    );
    assert!(host.offer(Msg::Read(Box::new(Request {
        machine_only: true,
        cadence_ms: 10_000,
        jobs_now: true,
        ..Request::default()
    }))));
    let hang = Instant::now() + Duration::from_secs(60);
    while !host.adopt(Instant::now()) {
        assert!(Instant::now() < hang, "the hidden reading never landed");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        *seen.lock().unwrap(),
        [(false, true)],
        "the deck's: machine only, as asked"
    );
    seen.lock().unwrap().clear();
    let (got, _) = COPY.fresh(Duration::ZERO, true);
    assert!(
        got.is_some_and(|h| h.snap.seq > 1),
        "the pull's reading was published"
    );
    let seen_now = seen.lock().unwrap().clone();
    assert!(!seen_now.is_empty(), "the pull read");
    assert!(
        seen_now.iter().all(|seen| *seen == (true, false)),
        "whole, at the engine's cadence: {seen_now:?}"
    );
    assert!(
        host.stop_at_exit(Duration::from_secs(60)),
        "the thread ended"
    );
}

#[test]
fn the_counter_readers_level_is_a_thermal_state_that_says_it_estimates() {
    let t = |level, exact| {
        counter_thermal(Ok(aterm_sysprobe::Throttle { level, exact })).expect("a level")
    };
    let heavy = t(aterm_sysprobe::ThrottleLevel::Heavy, false);
    assert_eq!(
        (heavy.level, heavy.raw, heavy.exact),
        (Throttle::Heavy, 2, false)
    );
    assert_eq!(heavy.low_power, None);
    assert_eq!(
        t(aterm_sysprobe::ThrottleLevel::Nominal, false).level,
        Throttle::None
    );
    assert_eq!(
        t(aterm_sysprobe::ThrottleLevel::Severe, true).level,
        Throttle::Severe
    );
    assert_eq!(counter_thermal(Err(aterm_sysprobe::Why::NotRead)), None);
    assert_eq!(counter_thermal(Err(aterm_sysprobe::Why::Absent)), None);
}

#[test]
fn the_thermal_state_maps_onto_the_portable_levels() {
    assert_eq!(thermal_of(0, Some(false)).level, Throttle::None);
    assert_eq!(thermal_of(1, None).level, Throttle::Light);
    assert_eq!(thermal_of(2, None).level, Throttle::Heavy);
    assert_eq!(thermal_of(3, None).level, Throttle::Severe);
    assert_eq!(thermal_of(9, None).level, Throttle::Severe);
    assert_eq!(thermal_of(-1, None).raw, 0);
    assert_eq!(thermal_of(2, Some(true)).low_power, Some(true));
}

#[test]
fn the_test_reader_never_reads_the_thermal_state() {
    let mut reader = Reader::new(Arc::default());
    let snap = reader.read(&Request {
        self_pid: std::process::id(),
        ..Request::default()
    });
    assert!((reader.thermal)().is_none());
    if !cfg!(target_os = "linux") {
        assert!(snap.thermal.is_none());
    }
}

/// With no window, an idle view holding a program at macOS's program check, or a backed-up
/// episode still owing its closing reading, is read again after 30 s; a live job's faster cadence
/// stands beside it, and with neither nothing is due before the disk watch's next look.
#[test]
fn a_held_finding_keeps_the_windowless_reading_going() {
    use aterm_vitals::jobs::{Findings, HeldEpisode, Named};
    let view = |demand, rows, findings| JobsView {
        demand,
        rows,
        findings,
        ..JobsView::default()
    };
    let held = Findings {
        program_check: vec![Named {
            pid: 25311,
            name: "gh".into(),
            tab: None,
            for_ms: 35_940_000,
        }],
        ..Findings::default()
    };
    assert_eq!(
        job_demand_after(&view(Demand::Idle, Vec::new(), held.clone())),
        Some(Duration::from_secs(30))
    );
    assert_eq!(
        job_demand_after(&view(Demand::Fast, vec![hung_call()], held)),
        Some(Duration::from_secs(2)),
        "a live job's own cadence"
    );
    let closing = Findings {
        program_check_episode: Some(HeldEpisode {
            opened_ms: 1,
            for_ms: 60_000,
            peak: 1,
            oldest_ms: 35_940_000,
        }),
        ..Findings::default()
    };
    assert_eq!(
        job_demand_after(&view(Demand::Idle, Vec::new(), closing)),
        Some(Duration::from_secs(30)),
        "the episode's closing reading"
    );
    assert_eq!(
        job_demand_after(&view(Demand::Idle, Vec::new(), Findings::default())),
        None
    );
}

/// An episode standing where none or another stood opens; the view naming the drained episode, or
/// one standing in none after one that stood (the drained view itself skipped), drains; the same
/// view again, an episode still standing, or nothing either side makes no edge.
#[test]
fn an_episode_edge_is_read_against_the_view_before() {
    use crate::message_reporters::HeldEdge;
    use aterm_vitals::jobs::{Findings, HeldEpisode};
    let episode = |opened_ms| HeldEpisode {
        opened_ms,
        for_ms: 60_000,
        peak: 2,
        oldest_ms: 90_000,
    };
    let open = |opened_ms| Findings {
        program_check_episode: Some(episode(opened_ms)),
        ..Findings::default()
    };
    let drained = Findings {
        program_check_drained: Some(episode(1)),
        ..Findings::default()
    };
    let none = Findings::default();
    for (before, after, want) in [
        (None, &open(1), Some(HeldEdge::Opened(episode(1)))),
        (Some(&none), &open(1), Some(HeldEdge::Opened(episode(1)))),
        (Some(&open(1)), &open(9), Some(HeldEdge::Opened(episode(9)))),
        (Some(&open(1)), &open(1), None),
        (
            Some(&open(1)),
            &drained,
            Some(HeldEdge::Drained(episode(1))),
        ),
        (Some(&none), &drained, Some(HeldEdge::Drained(episode(1)))),
        (Some(&drained), &drained, None),
        (Some(&open(1)), &none, Some(HeldEdge::Drained(episode(1)))),
        (Some(&drained), &none, None),
        (None, &none, None),
    ] {
        assert_eq!(held_edge(before, after), want, "{before:?} -> {after:?}");
    }
}

/// A translating row's first reading is one `program-check.not-started` record ("translating
/// (Rosetta)") and the next view's none; a row on a volume not proven local, new later, is one
/// more. No record gives an age, an Entry or a Now (leftover-processes design §6.5).
#[test]
fn a_program_first_named_as_not_started_is_recorded_once() {
    use crate::message_reporters as mr;
    use aterm_vitals::jobs::{HeldProc, JobTick, Jobs, StartWait};
    static BOARD: JobsBoard = JobsBoard::new();
    let row = |pid, name: &str, wait| HeldProc {
        pid,
        exec: u64::from(pid),
        name: name.into(),
        aterm: true,
        wait,
    };
    let x86 = row(7010, "x86tool", StartWait::Translating);
    let nfs = row(7011, "nfsprog", StartWait::NotStarted);
    let mut engine = Jobs::new();
    let mut adopt = |app: &mut crate::App, seq, held: &[HeldProc]| {
        let at_ms = seq * 2_000;
        let view = engine.absorb(&JobTick {
            at_ms,
            read_ms: at_ms,
            self_uid: Some(501),
            held: held.to_vec(),
            ..JobTick::default()
        });
        app.vitals.latest.lock().unwrap().snap = Some(Arc::new(Snapshot {
            seq,
            jobs: Some(Arc::new(view)),
            ..Snapshot::default()
        }));
        app.on_vitals_wake();
    };
    let mut app = crate::App::headless_for_test();
    app.vitals.board = &BOARD;
    adopt(&mut app, 1, std::slice::from_ref(&x86));
    adopt(&mut app, 2, std::slice::from_ref(&x86));
    adopt(&mut app, 3, &[x86.clone(), nfs]);
    let records: Vec<(String, Vec<String>)> = app
        .messages
        .log()
        .records()
        .filter(|r| r.key.as_deref() == Some(mr::NOT_STARTED_KEY))
        .map(|r| (r.title.clone(), r.detail.clone()))
        .collect();
    let one = |line: &str| {
        (
            "1 program has not started".to_owned(),
            vec![line.to_owned()],
        )
    };
    assert_eq!(
        records,
        [
            one(
                "pid 7010 x86tool: translating (Rosetta), which aterm's Developer Tools grant \
                 does not reach"
            ),
            one(
                "pid 7011 nfsprog: it has not started, and its program is on a volume aterm \
                 cannot prove local, so nothing read says why"
            ),
        ],
        "each program once, when a view first names it"
    );
    assert!(
        app.messages
            .digest()
            .entries
            .iter()
            .all(|e| !e.key.starts_with("program-check")),
        "a record only"
    );
}

/// A held episode is one `program-check.` record at each edge (the opening says when aterm started
/// none of the held programs) and files nothing in the digest, whatever Developer Tools reads.
#[test]
fn a_held_episode_is_recorded_at_its_edges_and_raises_nothing() {
    use crate::devtools_helper::test_arms;
    use crate::message_reporters as mr;
    use aterm_containment::DevToolGrant;
    use aterm_vitals::jobs::{Findings, HeldEpisode, Named};
    static BOARD: JobsBoard = JobsBoard::new();
    let episode = HeldEpisode {
        opened_ms: 60_000,
        for_ms: 36_300_000,
        peak: 3,
        oldest_ms: 35_940_000,
    };
    let held = |aterm: (u32, u64)| Findings {
        program_check: vec![Named {
            pid: 25311,
            name: "gh".into(),
            tab: None,
            for_ms: 35_940_000,
        }],
        program_check_aterm: aterm,
        program_check_episode: Some(episode),
        ..Findings::default()
    };
    let drained = Findings {
        program_check_drained: Some(episode),
        ..Findings::default()
    };
    let adopt = |app: &mut crate::App, seq, findings: &Findings| {
        app.vitals.latest.lock().unwrap().snap = Some(Arc::new(Snapshot {
            seq,
            jobs: Some(Arc::new(JobsView {
                findings: findings.clone(),
                ..JobsView::default()
            })),
            ..Snapshot::default()
        }));
        app.on_vitals_wake();
    };
    let records = |app: &crate::App| -> Vec<(String, Vec<String>)> {
        app.messages
            .log()
            .records()
            .filter(|r| r.key.as_deref() == Some(mr::PROGRAM_CHECK_EPISODE_KEY))
            .map(|r| (r.title.clone(), r.detail.clone()))
            .collect()
    };
    let opened = |outside: bool| {
        let mut detail =
            vec!["the oldest for 9 h 59 m; only this user's programs are counted".to_owned()];
        if outside {
            detail.push("they started outside aterm, so aterm's grant does not reach them".into());
        }
        (
            "macOS's program check is holding 3 programs".to_owned(),
            detail,
        )
    };
    let ended = (
        "macOS's program check caught up".to_owned(),
        vec!["drained after 10 h 05 m, at most 3 programs held at once".to_owned()],
    );
    let reading = |app: &mut crate::App, grant| {
        app.vitals.board = &BOARD;
        let list = if grant == DevToolGrant::Granted {
            test_arms::on()
        } else {
            test_arms::absent(1)
        };
        app.devtools_helper
            .note_reading(test_arms::reading(grant, list), Instant::now());
    };

    let mut app = crate::App::headless_for_test();
    reading(&mut app, DevToolGrant::NotGranted);
    adopt(&mut app, 1, &held((0, 0)));
    assert_eq!(records(&app), [opened(true)]);
    adopt(&mut app, 2, &held((2, 35_940_000)));
    assert_eq!(records(&app), [opened(true)], "one episode, one opening");
    adopt(&mut app, 3, &drained);
    assert_eq!(records(&app), [opened(true), ended.clone()]);
    assert!(app.messages.digest().is_empty(), "not granted: no line");

    for grant in [DevToolGrant::NotGranted, DevToolGrant::Granted] {
        let mut app = crate::App::headless_for_test();
        reading(&mut app, grant);
        adopt(&mut app, 1, &held((2, 35_940_000)));
        adopt(&mut app, 2, &drained);
        assert_eq!(records(&app), [opened(false), ended.clone()], "{grant:?}");
        assert!(app.messages.digest().is_empty(), "{grant:?}: no line");
    }
}

/// A verdict refresh's prime is a new program and would wait in the same line, so a switch-on read
/// while the adopted view stands in a backed-up episode starts none; the next view that stands in
/// none starts it, once (leftover-processes design §13).
#[test]
fn the_verdict_refresh_waits_out_a_backed_up_episode() {
    use crate::devtools_helper::test_arms;
    use aterm_containment::DevToolGrant;
    use aterm_vitals::jobs::{Findings, HeldEpisode};
    static BOARD: JobsBoard = JobsBoard::new();
    let adopt = |app: &mut crate::App, seq, findings: Findings| {
        app.vitals.latest.lock().unwrap().snap = Some(Arc::new(Snapshot {
            seq,
            jobs: Some(Arc::new(JobsView {
                findings,
                ..JobsView::default()
            })),
            ..Snapshot::default()
        }));
        app.on_vitals_wake();
    };
    let backed_up = || Findings {
        program_check_episode: Some(HeldEpisode {
            opened_ms: 60_000,
            for_ms: 30_000,
            peak: 3,
            oldest_ms: 50_000,
        }),
        ..Findings::default()
    };
    let mut app = crate::App::headless_for_test();
    app.vitals.board = &BOARD;
    test_arms::stand_in(&mut app, test_arms::opening());
    adopt(&mut app, 1, backed_up());
    let _ = app.devtools_watch.observe(DevToolGrant::NotGranted, true);
    let step = app.devtools_watch.observe(DevToolGrant::Granted, true);
    let _ = app.apply_devtools_step(step);
    assert_eq!(
        app.devtools_helper.refreshes_started(),
        0,
        "switched on inside an episode: macOS is asked nothing yet"
    );
    adopt(&mut app, 2, backed_up());
    assert_eq!(
        app.devtools_helper.refreshes_started(),
        0,
        "still backed up"
    );
    adopt(&mut app, 3, Findings::default());
    assert_eq!(
        app.devtools_helper.refreshes_started(),
        1,
        "the first view in no episode asks macOS again"
    );
    adopt(&mut app, 4, Findings::default());
    assert_eq!(app.devtools_helper.refreshes_started(), 1, "once");
}

/// With no deck scene the loop adopts the very reading the thread handed over.
#[test]
fn the_scene_seam_is_inert_unset() {
    let slot = Arc::new(Mutex::new(Handover::default()));
    let mut v = VitalsHost {
        latest: Arc::clone(&slot),
        deck_scene: None,
        ..VitalsHost::default()
    };
    let live = Arc::new(Snapshot {
        seq: 4,
        at_unix_ms: 1_800_000_000_000,
        ..Snapshot::default()
    });
    let _ = hand_over(&slot, &live, Vec::new(), Instant::now());
    assert!(v.adopt(Instant::now()));
    assert!(
        v.snapshot.as_ref().is_some_and(|s| Arc::ptr_eq(s, &live)),
        "the reading as read"
    );
}

/// A deck scene's readings are the scene's facts under each reading's sequence number: two
/// readings adopt with increasing `seq` and the same facts, the scene's clock among them.
#[test]
fn the_deck_scene_ticks() {
    use aterm_vitals::fixtures::{self, DeckScene};
    let facts = |s: &Snapshot| Snapshot {
        seq: 0,
        ..s.clone()
    };
    for scene in DeckScene::ALL {
        let slot = Arc::new(Mutex::new(Handover::default()));
        let mut v = VitalsHost {
            latest: Arc::clone(&slot),
            deck_scene: Some(scene),
            ..VitalsHost::default()
        };
        let mut held = Vec::new();
        for seq in [7, 8] {
            let live = Arc::new(Snapshot {
                seq,
                at_unix_ms: 1_800_000_000_000 + i64::try_from(seq).unwrap_or(0) * 1000,
                self_pid: 42,
                ..Snapshot::default()
            });
            let _ = hand_over(&slot, &live, Vec::new(), Instant::now());
            assert!(v.adopt(Instant::now()), "{scene:?}: reading {seq} is new");
            held.push(v.snapshot.clone().expect("adopted"));
        }
        assert_eq!(held.iter().map(|s| s.seq).collect::<Vec<_>>(), [7, 8]);
        let want = facts(&fixtures::snapshot(scene));
        for s in &held {
            assert_eq!(facts(s), want, "{scene:?}: the scene's facts");
        }
        assert_eq!(
            held[1].at_unix_ms,
            fixtures::AT,
            "{scene:?}: the scene's clock"
        );
    }
}
