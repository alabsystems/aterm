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

/// The production admission uses the table without touching the tab's path.
/// Refused and incomplete readings never reach even an injected reader.
#[test]
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

/// Linux's and Windows' readings reach the model: shared memory counts
/// as in use, a class the OS does not report adds nothing, and commit
/// keeps whether its limit is enforced. A NEGATIVE CONTROL: macOS's
/// purgeable pages are still cache.
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

/// The GPU's memory, resets and unit figures, every link's counters and
/// kind, and the process limits reach the model as read. A link the probe
/// calls other is a VPN or a bridge by its name, and only links whose bytes
/// are the machine's own count. NEGATIVE CONTROL: an unread GPU figure stays
/// unread.
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

/// Two full readings give this process a scope and the machine its cores,
/// with the clocks, the load, the process counts and limits and the disk
/// facts read. A hidden reading then reads the machine alone: the cores
/// still move, and no process sweep means no aterm scope.
#[test]
fn two_readings_give_this_process_a_scope_and_the_machine_its_cores() {
    let mut reader = Reader::new(Arc::default());
    let req = Request {
        self_pid: std::process::id(),
        cadence_ms: 1_000,
        ..Request::default()
    };
    let _ = reader.read(&req);
    // Spend a little CPU so the second reading has a delta to show.
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

/// The production producer retains its pending messages while a platform
/// read is busy. Later ticks forgo theirs instead of accumulating work or
/// waiting for the reader (and say so, so the layout is offered again);
/// room and disconnect behave normally.
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

/// v2 §5.3: a deck every window of which is hidden is read every ten
/// seconds, machine counters only, lens or none; in view it is read at the
/// focused, background or lens cadence; nothing draws it, nothing is read.
/// Coming back from hidden, the next reading is due at once, not ten seconds
/// after the last; a lens's ask for one now is kept.
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
    // Hidden: ten seconds apart.
    assert_eq!(
        due_at(Some(t0), Some(t0 + HIDDEN_PERIOD), HIDDEN_PERIOD, t0 + s(3)),
        t0 + HIDDEN_PERIOD
    );
    // Back in view three seconds after a hidden reading: due already.
    let back = t0 + s(3);
    assert!(due_at(Some(t0), Some(t0 + HIDDEN_PERIOD), FOCUSED_PERIOD, back) <= back);
    // A lens asked for one now.
    assert_eq!(
        due_at(Some(t0), Some(t0), FOCUSED_PERIOD, t0 + s(1) / 10),
        t0
    );
}

/// v2 §8.2: the peaks go to the JSON file and come back equal; they are
/// written at most every ten minutes while they change, and at once when
/// the thread stops. NEGATIVE CONTROLS: a missing, damaged or other-layout
/// file reads as no peaks, and peaks that did not change are not written.
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
    // A file past the bound is not read, though it parses: the peaks
    // padded with spaces.
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

/// A probe thread's stand-in: each reading moves the peaks to `peaks`.
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

/// v2 §8.2: the quit stops the probe thread, which writes the peaks that
/// moved since the file was read, though ten minutes have not passed (the
/// App is never dropped, so the quit is the only stop it gets). The thread's
/// readings reach the loop's slot first. NEGATIVE CONTROL: a host whose
/// thread never started stops at once and writes nothing.
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

    // A reader still being constructed cannot have installed its workers'
    // sender yet. Finish the quit first, then let it run: that late sender
    // must not keep its own receiver alive and prevent the final peaks write.
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

/// v2 §8.3: each agent session's totals reach the model once, whichever tabs
/// show it, summed over every model, with what its fold left unread and
/// whether it was complete. NEGATIVE CONTROLS: facts with no usage, or with
/// no session id to key a baseline by, give no row.
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
    // On the model's clock: after its origin, and before it.
    let epoch = read + Duration::from_millis(1500);
    assert_eq!(mono_ms_of(read + Duration::from_secs(3), read), 3_000);
    assert_eq!(mono_ms_of(read, epoch), -1_500);
}

/// Session `session`'s Claude Code footer: agent session `id` with
/// `output` tokens and `plan`, read at `read`.
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

/// v2 §8.1, §8.4: token rows come from every pane of every tab, a pane
/// without the focus too, each keyed to the tab that holds it and carrying
/// when its footer last read it; the plan is the front tab's, and the front
/// tab is the frontmost window's whether or not aterm has the focus.
/// NEGATIVE CONTROL: with the first window frontmost, the plan is its tab's,
/// though the other window was opened last.
#[test]
fn every_pane_counts_and_the_plan_is_the_frontmost_windows() {
    let mut app = crate::App::headless_for_test();
    let w0 = crate::WindowId(0);
    let v0 = app.windows[&w0].tab_set.active().expect("a tab").focus;
    // An agent in a split pane that does not have the focus.
    let split = app.split_active_stub_tab(w0);
    // The focus back on the first pane, as a person's chord moves it.
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
    // A second window, its agent on another account.
    let s1 = app.next_session_id;
    let w1 = app.insert_logical_window(crate::stub_session(s1), 24, 80);
    publish(&app, s1, "s-other", Some(plan("bbbbbbbb-0000", 610)), read);
    // aterm in the background: no window has the focus.
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

/// v2 §8.4: the front tab's plan is a dated fact naming its binding window,
/// its share, when it was read (ms) and its account's number among the tabs'
/// accounts, while the reading is current. NEGATIVE CONTROL: a reading over
/// fifteen minutes old, or whose window reset, gives none.
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

/// A board whose watch judged the home volume low, with a stuck session,
/// a freed tally and a pass's hog, and the home volume's key.
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

/// T-G13, the host's part: with the harness on (the board is watched), the
/// board's facts reach the model in its own types (the level, the lines, the
/// stuck count, today's tally, the pass's hogs under `~`, times in ms). With
/// the harness off the host no longer watches the board, and the same board
/// gives nothing: no level, no lines, no stuck count, no freed tally,
/// `watch` false, and no clock is read; the row's growth and free figure
/// still come from the readings, Details says "disk watch off" and the lens
/// has no "Wrong?".
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

    // The harness host's tick with the harness off.
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

    // Ten minutes of readings, the disk filling 260 MB a minute, with the
    // harness off.
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

/// v2 §7.4: the survey's rows reach the disk facts with the harness on or
/// off, and whether a survey runs; the last pass's own rows win unless the
/// survey is newer and whole. A partial size prints as a floor. NEGATIVE
/// CONTROLS: a newer survey that sized nothing (a home it could not
/// resolve, a search that spent the budget) or ran out of time keeps the
/// pass's rows; with no pass rows, even a cut survey's are shown.
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

    // The harness off: no pass, no watch, the survey's rows still.
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

/// A test App with a reader in hand instead of the probe thread (none
/// starts: a test App has no loop), posting to `board`. The receiver is
/// the thread's end: each message the App sends lands there, in a queue as
/// deep as the product's.
fn with_reader(app: &mut crate::App, board: &'static JobsBoard) -> Receiver<Msg> {
    let (tx, rx) = std::sync::mpsc::sync_channel(2);
    app.vitals.tx = Some(tx);
    app.vitals.tried = true;
    app.vitals.board = board;
    rx
}

/// The first reading asked since the last look (a layout sent alone, for
/// `sys`, is no reading); `why` names the one expected.
fn read_req(rx: &Receiver<Msg>, why: &str) -> Box<Request> {
    rx.try_iter()
        .find_map(|m| match m {
            Msg::Read(r) => Some(r),
            _ => None,
        })
        .expect(why)
}

/// The readings asked since the last look (a layout sent alone, for `sys`,
/// is no reading).
fn asked(rx: &Receiver<Msg>) -> usize {
    rx.try_iter().filter(|m| matches!(m, Msg::Read(_))).count()
}

/// The probe thread's part: reading `seq` lands with a jobs view of
/// `rows` whose engine asked `demand`.
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

/// Tab 0's (the pool session's local id 0) session id.
fn sid_of_tab0(app: &crate::App) -> String {
    let store = app.store.read().unwrap();
    store
        .by_local(0)
        .expect("tab 0's session")
        .sid
        .as_str()
        .to_owned()
}

/// I4's call: tab 0's agent's background call, suspect (capped: an
/// unnamed wait) and at no progress for 31 minutes, waiting in pid 27098.
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

/// READINGS WITH NO WINDOW (the hung-call design's D1): no window shows
/// the bars, so nothing is read and nothing is due, until the disk
/// watch's look, which asks for ONE reading and nothing more while it is
/// read. Landed, its jobs view is on the board, where the canary's
/// evidence for tab 0's session names the hung call. While that suspect
/// agent call stands, the engine's own demand (fast: 2 s from the
/// reading's adoption) names the next reading, and the loop asks it then,
/// not before; once a reading shows no agent call, nothing is due until
/// the next look. NEGATIVE CONTROL: before the look, a wake asks nothing.
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
/// nothing, and a jobs view whose engine is fast moves no deadline. The
/// bars hidden, the held view's demand goes on (slow, 30 s: a deadline,
/// no reading yet); shown again, the bars read at once.
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
/// jobs engine (the hung-call design's D1). A machine reading carries the
/// engine's last view, so it leaves the engine's demand where the view's
/// adoption put it: a slow call's next whole reading is due 30 s after that
/// adoption, not pushed out by each hidden reading. NEGATIVE CONTROL: before
/// the look, every reading asked is the machine counters alone.
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

    // Two hidden readings land carrying the view already held: the engine's
    // demand stays where the view's adoption put it.
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

/// EDGES (module docs): with no window, output asks one whole reading at
/// once and the next no sooner than the floor, whatever the held view says
/// (a job may have started or ended); a window showing the bars clears the
/// edge (its cadence reads). NEGATIVE CONTROL: with no output and no live
/// job, nothing is read.
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

    // A view whose jobs wait for the person arms nothing, but output still
    // reads: a job may have started.
    land(&app, 1, Vec::new(), Demand::Idle);
    app.on_vitals_wake();
    assert_eq!(app.vitals_deadline(), None, "an idle view: nothing due");
    app.vitals.note_edge();
    app.service_vitals(t0 + 3 * JOBS_FLOOR);
    assert_eq!(asked(&rx), 1, "output reads");
}

/// THE P2a PIECES WITH NO DECK AND NO PULL (§6.1 kind 3): output starts a
/// job's first reading; while the view holds it live, the engine's cadence
/// reads again with no output and no `sys` pull; the reading that sees it end
/// hands its `exited` event to the session's timeline (`subscribe … events`
/// pushes it as `EVENT … job exited`) and `status` says `job=none`. Then
/// nothing more is due. NEGATIVE CONTROL: before its deadline the live job
/// asks nothing.
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
        tl.since(None)
            .filter(|e| e.kind == crate::sys_host::JOB_CHANGE)
            .map(|e| e.payload.to_string())
            .collect()
    };
    assert_eq!(recorded, ["exited job=j41 class=exited rev=1 code=0"]);
    assert_eq!(app.vitals_deadline(), None, "nothing live: nothing due");
}

/// OUTPUT IS SERVED IN ITS OWN WAKE: with no window, the output wake's edge
/// is asked right there, so it arms no deadline (left to the next turn, the
/// loop's deadline at now fires a `system_vitals` timer wake for every write
/// a program makes at rest). NEGATIVE CONTROL: output within the floor asks
/// nothing and leaves the owed reading on the deadline at the floor.
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
    app.on_output_wake(0, t0);
    assert_eq!(asked(&rx), 1, "the output's reading, in its wake");
    assert_eq!(app.vitals_deadline(), None, "and nothing armed for it");
    app.on_output_wake(0, t0 + Duration::from_millis(500));
    assert_eq!(asked(&rx), 0, "within the floor: owed");
    assert_eq!(app.vitals.edge_at(t0), Some(t0 + JOBS_FLOOR));
}

/// THE KERNEL EDGE (module docs, [`ProcEdge`]): with no window, the loop
/// arms the latch; a session's process event (a fork, an `exec`, an exit)
/// posts the loop one wake, and the loop takes it as an edge even with the
/// held view parked: one whole reading the engine must look at (`jobs_now`),
/// where output alone misses a job forked after its echo or started with no
/// output. Another event before that reading is asked posts nothing more;
/// the latch is armed again once it is, and an event then owes a reading at
/// the floor, during which further events post nothing. NEGATIVE CONTROLS:
/// parked with no event, nothing is read or armed; a window showing the bars
/// leaves the latch disarmed (its cadence reads).
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

/// A whole reading with no window (here the disk watch's look) is one the
/// engine looks at. NEGATIVE CONTROL: the bars' readings keep the engine's
/// own cadence (a worker's pull too:
/// `a_pull_reads_whole_and_the_quit_takes_the_workers_sender_back`).
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
/// job, as its shell would. Closing `stop` tells it to end the job BEFORE it
/// exits; the parent never kills the process that must reap it. Every owned
/// resource is guarded before readiness (including a failed constructor).
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

    /// The failure seam runs only after custody is armed, before the
    /// constructor hands that custody to its caller.
    fn starting(raw: bool, before_ready: impl FnOnce(&mut Self)) -> Self {
        use std::os::fd::{FromRawFd as _, OwnedFd};
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        // SAFETY: both fresh descriptors are transferred to their only
        // owners before any assertion or other fallible setup.
        let master = unsafe { OwnedFd::from_raw_fd(master) };
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        if raw {
            use std::os::fd::AsRawFd as _;
            // SAFETY: this test owns the live PTY and local termios.
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
        // fd 2 is the private stop channel, not a diagnostic stream. The
        // helper's job gets only the PTY, never this channel or stdout.
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

    /// Drain only immediately readable bytes; `eof` means read returned 0,
    /// never that the helper merely exited or its output was quiet.
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
            // SAFETY: poll reads and fills this one owned descriptor's slot.
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

    /// A test App whose sink borrows the master. Its session's own master
    /// stays the stub's -1, so App's drop signals nothing.
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
            // Do not kill the reaping parent, close its terminal or make its
            // completion write hit a closed pipe while custody is unknown.
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
    // SAFETY: the live slave at 0 is borrowed, never transferred away.
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
            // The child remains this handle's unreaped child; even a child
            // that exited already retains its pid until this wait collects it.
            let _ = self.0.kill();
            let reaped = aterm_spec::wait::within(
                aterm_spec::wait::MUST_HAPPEN,
                Duration::from_millis(10),
                || self.0.try_wait().ok().flatten(),
            );
            if reaped.is_none() {
                // The outer test has a bounded cleanup and retains this
                // helper on failure. Keep its reaping custody until the
                // child actually ends, rather than orphaning it to exit.
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
    // SAFETY: this session owns the live slave and its own child group.
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
/// view, through 20 s of its clock in the loop's 100 ms turns. Each reading
/// it asks is read here as the thread reads one, through the thread's own
/// step ([`jobs_step`], which [`Reader::read`] takes): this machine's
/// process table, the request as the loop built it (its words emptied when
/// `words` is false: main's host built none), and the stuck engine at the
/// turn's clock; its view lands as the thread's snapshot does, for the next
/// turn to adopt. The job's screen came first: its output asks the edge's
/// reading. The instants (ms into the 20 s) of the readings asked, each one
/// main thread wake and one `system_vitals` fire in the product.
#[cfg(target_os = "macos")]
fn readings_at_rest(app: &mut crate::App, rx: &Receiver<Msg>, words: bool) -> Vec<u64> {
    let t0 = Instant::now();
    let mut pass = crate::vitals_jobs::JobsPass::new();
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
            let sweep = aterm_sysprobe::processes().expect("this machine's process table");
            // The engine's clock: the turn's, past zero.
            let jobs = jobs_step(&mut pass, 1_000 + ms, Some(&sweep.procs), &req, false);
            app.vitals.latest.lock().unwrap().snap = Some(Arc::new(Snapshot {
                seq: u64::try_from(asked.len()).expect("few"),
                jobs,
                ..Snapshot::default()
            }));
        }
    }
    asked
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
/// more is read in the 20 s. NEGATIVE CONTROL, main's request (no word): the
/// same program reads as an unnamed wait, suspect, and the engine's demand
/// reads it every 2 s to the end of this model's 20 s (its 100 ms turns put
/// the readings 2.1 s apart, ten of them; END's own window saw 9 or 10).
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
/// uses it. NEGATIVE CONTROLS: output after the read leaves it unused; a
/// key typed and left unread, and a master that is no terminal, give none.
#[cfg(target_os = "macos")]
#[test]
fn a_terminal_word_holds_until_output_and_never_over_an_unread_key() {
    // Construction can fail AFTER both processes exist, before its caller
    // receives the guard. The already-armed guard must still let the leader
    // reap its own job, then reap the leader, even while unwinding.
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
    let output = Arc::new(AtomicU64::new(5));
    let word = TtyWord::read(3, &sink, &output).expect("a terminal's word");
    assert_eq!((word.key, word.canonical, word.echo), (3, false, false));
    assert_eq!(
        word.current(40)
            .map(|(key, m)| (key, m.canonical, m.echo, m.stamped_ms)),
        Some((3, false, false, 40))
    );
    output.store(6, Ordering::Release);
    assert_eq!(word.current(41), None, "output since the read: no word");

    // SAFETY: writes one byte of a local buffer to this test's master.
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
/// program at rest names it `tty-raw` and parks the engine. NEGATIVE
/// CONTROL: the same request read after the session printed (its own output
/// stamp moved, the one the loop bound the word to) names the wait unnamed,
/// suspect, read at the fast cadence.
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
    app.pool
        .get(0)
        .expect("tab 0")
        .latest_output_activity_ns
        .fetch_add(1, Ordering::AcqRel);
    assert_eq!(read_wait(&req), (Some(WaitName::Unnamed), Demand::Fast));
}

/// A KEY DRIVEN AFTER THE WORD WAS READ VOIDS IT (§11.5: the tty names need
/// an empty queue): a key typed into the session's own sink, which the raw
/// program leaves unread and answers with nothing, and a worker's pull,
/// which reads with the request the thread last held, names the wait
/// unnamed, never `tty-raw` over that key. NEGATIVE CONTROL: the same held
/// request before the key names `tty-raw`.
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
/// NEGATIVE CONTROL: output before the read is the word's own (the read saw
/// it), and the word holds.
#[cfg(target_os = "macos")]
#[test]
fn the_word_takes_its_stamps_before_it_reads_the_terminal() {
    let job = JobAtRest::new(true);
    let sink = aterm_session::sink::SinkWriter::new(job.master());
    let output = Arc::new(AtomicU64::new(5));
    let word = TtyWord::read_seamed(3, &sink, &output, || {
        output.fetch_add(1, Ordering::AcqRel);
    })
    .expect("a terminal's word");
    assert_eq!(word.current(9), None, "output mid-read: unused");
    output.fetch_add(1, Ordering::AcqRel);
    let word = TtyWord::read_seamed(3, &sink, &output, || {}).expect("a terminal's word");
    assert!(word.current(9).is_some(), "output before the read: its own");
}

/// A cold pull's unpublished first reading keeps its events: they ride the
/// published one, first (the live check lost a job's `exited` this way
/// before).
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
    assert!(hand_over(&slot, &Arc::new(snap)), "the loop hears of it");
}

/// The hand-over keeps every job event until the loop takes it (a later
/// snapshot does not drop an earlier one's), says when a verdict moved (a
/// pull's reading then wakes the loop), and the loop numbers each session's
/// events from 1.
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
    assert!(!hand_over(&slot, &snap(1, false)), "nothing moved");
    assert!(hand_over(&slot, &snap(2, true)));
    assert!(hand_over(&slot, &snap(3, true)));
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

/// A worker's pull through the workers' copy reaches the thread, which
/// takes a whole reading (a hidden deck's machine-only request
/// notwithstanding) and publishes it there; the quit takes the copy's sender
/// back, so the thread hears the hang-up and ends within the bound (a copy
/// still holding a sender would keep it blocked on its channel for good).
/// A pull keeps the stuck engine's cadence whatever the held request asked
/// of it. NEGATIVE CONTROL: the deck's own reading stays machine-only, and
/// keeps what it asked.
#[test]
fn a_pull_reads_whole_and_the_quit_takes_the_workers_sender_back() {
    static COPY: std::sync::LazyLock<crate::sys_host::Shared> =
        std::sync::LazyLock::new(crate::sys_host::Shared::new);
    /// Records whether each reading was whole, and made the engine look.
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
    // A hidden deck's reading first: once it lands, the thread has handed
    // the copy its sender.
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
    // NEGATIVE CONTROL: a reader that gives no level gives no state.
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

/// The reader's thermal probe is the inert arm under test: no unit test asks
/// AppKit. Linux's level comes from the counter reader (thermal zones and
/// throttle counters, [`counter_thermal`]), which asks no platform API, so
/// there the snapshot may carry one all the same.
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
