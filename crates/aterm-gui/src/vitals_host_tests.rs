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
            counts: true,
            rx: 5_000,
            tx: 700,
            rate_bps: Some(183_000_000),
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

/// A FULL READING ATTRIBUTES THE SLOTS' HOLDERS TO TABS, A HIDDEN ONE DOES
/// NOT ([`Reader::read`], [`crate::occupancy_host::slot_tabs`]): over a slot
/// directory of one free slot, the full reading's sweep finds no holder in
/// any tab (`Some` of none), while the hidden reading, which sweeps no
/// process, attributes nothing (`None`).
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
    file.keep(&peaks, t0 + PEAKS_EVERY / 2, false);
    assert!(!path.exists(), "within ten minutes of the start: not yet");
    file.keep(&peaks, t0 + PEAKS_EVERY, false);
    assert_eq!(reopen(), peaks, "the file gives the peaks back");

    let mut more = peaks.clone();
    more.tokens.note_bucket(20_382, 12_000.0);
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
    let front = req.plan.expect("the first window's plan");
    assert_eq!((front.used_x10, front.account), (340, AccountIx(0)));

    app.frontmost_window = Some(w1);
    let front = app
        .vitals_request(FOCUSED_PERIOD, false)
        .plan
        .expect("the frontmost window's plan");
    assert_eq!((front.used_x10, front.account), (610, AccountIx(1)));
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

/// A VIEW IS AS OLD AS THE READING THAT MADE IT: the board counts a new view's age from when the
/// probe began the oldest reading handed over since the loop last adopted one that carries it (a
/// later reading may carry the same view), not from the adoption, so the facts gate sees the
/// probe's read and the wait for the loop.
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

/// AN EARLIER READING OF AN OLDER VIEW DOES NOT AGE A NEW ONE: readings the loop never adopted
/// (a `sys` pull's wakes no one) pile up while it sleeps, and the board counts a new view's age
/// from the reading that made it, not from the first one handed over since the last adoption.
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

/// A WINDOWLESS ASK A FULL QUEUE DROPS IS ASKED AGAIN AT THE FLOOR: with no window showing the
/// bars and a hung call standing, the reading `jobs_due` asks finds the thread's queue full (a
/// `sys` pull's reading and an ask behind it, neither of which wakes the loop); it is due again
/// `JOBS_FLOOR` on, not at the next edge or disk look, and once the thread is gone nothing is.
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

/// A DROPPED ASK OUTLIVES A READING THAT DID NOT LOOK: the disk look's ask over a parked view (no
/// cadence of its own) finds the queue full; the in-flight pull's whole reading, adopted before
/// the retry, ran no engine pass (it carries the held view), so the ask is still due at the floor;
/// a view a reading begun since made is the look, and nothing more is due.
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

/// WITH A WINDOW SHOWING THE BARS NOTHING CHANGES: the bars' cadence
/// reads at once and then a period on; the disk watch's look asks
/// nothing, and a jobs view whose engine is fast moves no deadline.
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

/// A HIDDEN DECK AND THE ENGINE TOGETHER: a deck every window of which is
/// occluded reads the machine counters every ten seconds (v2 §5.3), and with
/// no window in view the disk watch's look asks one WHOLE reading for the
/// jobs engine (the hung-call design's D1).
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
    assert_eq!(app.status_job_fields(0), "job=none job_quiet_ms=-");
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

fn feed_tab(app: &crate::App, tab: u64, bytes: &[u8]) {
    let s = app.pool.get(tab).expect("the tab");
    let mut t = s.term.lock().unwrap_or_else(PoisonError::into_inner);
    t.process(bytes);
    s.ctx.screen_moves.fed(crate::control::screen_gen(&t));
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

    fn starting(raw: bool, before_ready: impl FnOnce(&mut Self)) -> Self {
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
    let mut cmd = aterm_uds::spawnfd::command("/bin/sleep");
    cmd.arg(aterm_spec::wait::PARKED_SECS.to_string())
        .process_group(0)
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

/// THE PROBE THREAD'S PART, ON THIS MACHINE: drive `app`, no window in
/// view, through 20 s of its clock in the loop's 100 ms turns.
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
    let jobs = jobs_step(pass, at_ms, Some(&sweep.procs), req, false);
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

/// A PROGRAM WAITING FOR THE PERSON AT ITS PROMPT ARMS NOTHING WITH NO
/// WINDOW (design §6.1, "What arms nothing"; Autopilot Land's END, which saw 9
/// to 10 timer wakes in its 20 s at rest on main, each a `system_vitals`
/// fire, for Claude Code 2.1.306 and 2.1.307 and Gemini CLI): a raw program
/// at rest on a real terminal, read through the probe thread's own step by
/// the real engine on this machine's process table, is named `tty-raw` from
/// the terminal word the loop hands it, and after the edge's reading nothing
/// more is read in the 20 s.
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

/// A PARKED SCREEN'S REDRAW IS READ AT MOST ONCE A FLOOR, AND NEVER ON A
/// DEADLINE, WHILE THE TERMINAL SAYS IT STILL WAITS (design §6.1, "What
/// brings job demand back"): a raw program at the person's read on a real
/// terminal, parked by the real engine from the word its reading carried,
/// redraws each second (a clock ticking) with no window.
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

/// THE THREAD HANDS BACK THE WORDS IT READ WITH, AND THE LOOP PARKS FROM
/// THEM: the real probe thread ([`run`], started as the product starts it)
/// reads a raw program at the person's read on a real terminal through the
/// engine's own step over this machine's process table, and hands its view
/// back beside the words of the request it read; the loop names the park
/// from them: the job's group, the word's raw mode and the driven input
/// before it.
#[cfg(target_os = "macos")]
#[test]
fn the_thread_hands_back_the_words_it_read_with_and_the_loop_parks_from_them() {
    struct Engine {
        vitals: aterm_vitals::Vitals,
        pass: crate::vitals_jobs::JobsPass,
        t0: Instant,
        seq: u64,
    }
    impl Source for Engine {
        fn reading(&mut self, req: &Request) -> Snapshot {
            self.seq += 1;
            let sweep = aterm_sysprobe::processes().expect("this machine's process table");
            let at_ms = 1_000 + u64::try_from(self.t0.elapsed().as_millis()).unwrap_or(0);
            Snapshot {
                seq: self.seq,
                jobs: jobs_step(&mut self.pass, at_ms, Some(&sweep.procs), req, false),
                ..Snapshot::default()
            }
        }
        fn model(&mut self) -> &mut aterm_vitals::Vitals {
            &mut self.vitals
        }
    }
    static BOARD: JobsBoard = JobsBoard::new();
    let job = JobAtRest::new(true);
    let sink = Arc::new(aterm_session::sink::SinkWriter::new(job.master()));
    let input = sink.driven_input();
    let mut app = crate::App::headless_for_test_with_sink(sink);
    app.pool.sessions.get_mut(&0).expect("tab 0").session.pid =
        i32::try_from(job.leader.as_ref().expect("owned leader").id()).expect("pid");
    app.vitals.board = &BOARD;
    app.vitals.start(
        || Engine {
            vitals: aterm_vitals::Vitals::new(),
            pass: crate::vitals_jobs::JobsPass::new(),
            t0: Instant::now(),
            seq: 0,
        },
        || true,
        None,
        None,
    );
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

/// A PARK HOLDS ONLY WHILE THE TERMINAL SAYS THE PROGRAM STILL WAITS
/// ([`Parked::holds`]): the same group in front, the same `ICANON` and
/// `ECHO`, and no input driven since the word.
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

/// THE WORD'S FRESHNESS (§6.6): read through the sink, it holds for the
/// engine until the session's next output, and is stamped at the tick that
/// uses it.
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
        word.current(40)
            .map(|(key, m)| (key, m.canonical, m.echo, m.stamped_ms)),
        Some((3, false, false, 40))
    );
    output.fed(drawn(6));
    assert_eq!(word.current(41), None, "output since the read: no word");

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

/// THE PROBE THREAD'S WHOLE READING NAMES A RAW PROGRAM FROM THE LOOP'S
/// WORD: one [`Reader::read`] of the request the loop builds for a raw
/// program at rest names it `tty-raw` and parks the engine.
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

/// A WRITE THAT MOVES NO CELL LEAVES THE WORD STANDING (`docs/measured/vitals-3-2026-10-22.md`,
/// V1): Muse Code at rest writes a colour reset every ~9 s a tab, and the output stamp moves with
/// it. A word voided by it would name the raw program's wait `unnamed` and ask the next reading in
/// 2 s (`Demand::Fast`), keeping `system_vitals` armed at rest; the word follows the reader's
/// moving feeds instead, so three resets leave it standing and a write that draws voids it.
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
        "a write that drew: the word is void"
    );
}

/// A KEY DRIVEN AFTER THE WORD WAS READ VOIDS IT (§11.5: the tty names need
/// an empty queue): a key typed into the session's own sink, which the raw
/// program leaves unread and answers with nothing, and a worker's pull,
/// which reads with the request the thread last held, names the wait
/// unnamed, never `tty-raw` over that key.
#[cfg(target_os = "macos")]
#[test]
fn a_key_driven_after_the_word_was_read_voids_it_for_a_pull() {
    use aterm_vitals::jobs::WaitName;
    static BOARD: JobsBoard = JobsBoard::new();
    let job = JobAtRest::new(true);
    let (app, _rx) = job.app(&BOARD);
    let held = app.vitals_request(Duration::ZERO, false);
    assert!(held.ttys[0].current(1).is_some(), "the word holds");
    assert_eq!(read_wait(&held), (Some(WaitName::TtyRaw), Demand::Idle));
    let sink = Arc::clone(&app.pool.get(0).expect("tab 0").ctx.sink);
    assert_eq!(sink.write_frame(b"k").expect("typed"), 1);
    aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(10),
        || aterm_pty::input_queue_len(job.master()).filter(|n| *n > 0),
    )
    .expect("the key waits unread");
    assert_eq!(held.ttys[0].current(2), None, "a key since: no word");
    assert_eq!(read_wait(&held), (Some(WaitName::Unnamed), Demand::Fast));
}

/// THE WORD TAKES ITS STAMPS BEFORE IT READS THE TERMINAL (§6.6): output
/// that lands between them and the `tcgetattr` leaves the word unused.
#[cfg(target_os = "macos")]
#[test]
fn the_word_takes_its_stamps_before_it_reads_the_terminal() {
    let job = JobAtRest::new(true);
    let sink = aterm_session::sink::SinkWriter::new(job.master());
    let output = Arc::new(ScreenMoves::default());
    output.fed(drawn(5));
    let word = TtyWord::read_seamed(3, &sink, &output, || {
        output.fed(drawn(6));
    })
    .expect("a terminal's word");
    assert_eq!(word.current(9), None, "output mid-read: unused");
    output.fed(drawn(7));
    let word = TtyWord::read_seamed(3, &sink, &output, || {}).expect("a terminal's word");
    assert!(word.current(9).is_some(), "output before the read: its own");
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

/// A HELD PROGRAM KEEPS THE READING WITH NO WINDOW GOING (leftover §5.1):
/// a view with no job (the engine idle) but one program of this user held at
/// macOS's program check is read again after 30 s, and so is one whose
/// backed-up episode still owes its closing reading; a live job's own faster
/// cadence stands beside it, and with neither nothing is due before the disk
/// watch's next look.
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
