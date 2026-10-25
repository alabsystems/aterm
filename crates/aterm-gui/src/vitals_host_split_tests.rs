// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for split-set sampling and `sys why` cold reads on [`crate::vitals_host`].

use std::cell::{Cell, RefCell};

use aterm_sysprobe::coalitions::{CoalitionRead, CoalitionUsage};
use aterm_sysprobe::identity::{CS_ADHOC, CS_PLATFORM_BINARY, CS_VALID, Identity};
use aterm_sysprobe::timeline::Timeline;
use aterm_vitals::explain::{ActivityId, CauseId, Via, Voucher};

use super::*;

/// The stand-in kernel's reads on one thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Reads {
    coalitions: usize,
    /// Sweeps that read each process's coalition.
    swept: usize,
    /// Sweeps that read the pid version of each process whose start they
    /// cannot read.
    versioned: usize,
    births: usize,
    tables: usize,
    identities: usize,
    timelines: usize,
}

thread_local! {
    static KERNEL: RefCell<HashMap<u32, (String, i64, bool)>> = RefCell::new(HashMap::new());
    static WAKE: Cell<Option<i64>> = const { Cell::new(None) };
    static MISSES: Cell<Option<u32>> = const { Cell::new(None) };
    /// Execs so far, which every pid version reads moved by.
    static EXECS: Cell<i32> = const { Cell::new(0) };
    /// A pid that execs while its identity is read.
    static RACES: Cell<Option<u32>> = const { Cell::new(None) };
    /// A pid whose pid version reads nothing.
    static UNVERSIONED: Cell<Option<u32>> = const { Cell::new(None) };
    /// New processes so far at each pid, which its pid version moves by.
    static REBORN: RefCell<HashMap<u32, i32>> = RefCell::new(HashMap::new());
    static READS: Cell<Reads> = const {
        Cell::new(Reads {
            coalitions: 0,
            swept: 0,
            versioned: 0,
            births: 0,
            tables: 0,
            identities: 0,
            timelines: 0,
        })
    };
}

fn count(read: impl FnOnce(&mut Reads)) {
    let mut reads = READS.get();
    read(&mut reads);
    READS.set(reads);
}

/// The stand-in kernel: a process's image and signature change only by an
/// exec, which moves its pid version, so a pid's version here moves with its
/// row's name and signature, with each exec ([`EXECS`]) and with each new
/// process at the pid ([`reborn`]).
const STAND_IN: CoalProbe = CoalProbe {
    self_check: |_| Ok(1),
    sweep: |opts, ids| {
        if opts.coalitions {
            count(|r| r.swept += 1);
        }
        aterm_sysprobe::processes_keeping(opts, ids)
    },
    coalitions: || {
        count(|r| r.coalitions += 1);
        Some(CoalitionRead {
            listed: 1,
            newest: Some(1),
            truncated: false,
            rows: vec![CoalitionUsage {
                id: 1,
                tasks_started: 1,
                ..CoalitionUsage::default()
            }],
        })
    },
    birth: |pid| {
        count(|r| r.births += 1);
        KERNEL.with(|k| {
            k.borrow()
                .get(&pid)
                .map(|(name, ms, _)| (name.clone(), *ms))
        })
    },
    births: || {
        count(|r| r.tables += 1);
        Some(KERNEL.with(|k| {
            k.borrow()
                .iter()
                .map(|(pid, (name, ms, _))| (*pid, (name.clone(), *ms)))
                .collect()
        }))
    },
    identity: |pid| {
        count(|r| r.identities += 1);
        if RACES.get() == Some(pid) {
            RACES.set(None);
            EXECS.set(EXECS.get() + 1);
        }
        if MISSES.get() == Some(pid) {
            MISSES.set(None);
            return None;
        }
        KERNEL.with(|k| {
            k.borrow().get(&pid).map(|(name, _, apple)| Identity {
                exe: name.clone(),
                cs_status: Some(if *apple {
                    CS_VALID | CS_PLATFORM_BINARY
                } else {
                    CS_VALID
                }),
                category: Some(if *apple { 1 } else { 6 }),
                ..Identity::default()
            })
        })
    },
    version: |pid| {
        if UNVERSIONED.get() == Some(pid) {
            return None;
        }
        let reborn = REBORN.with(|r| r.borrow().get(&pid).copied().unwrap_or(0));
        KERNEL.with(|k| {
            k.borrow().get(&pid).map(|(name, _, apple)| {
                name.bytes()
                    .fold(EXECS.get() + i32::from(*apple), |v, b| {
                        v.wrapping_mul(31).wrapping_add(i32::from(b))
                    })
                    .wrapping_add(reborn.wrapping_mul(1_000_003))
            })
        })
    },
    timeline: || {
        count(|r| r.timelines += 1);
        Some(Timeline {
            boot_ms: 1,
            wake_ms: WAKE.get(),
            ..Timeline::default()
        })
    },
    waketime_ms: || WAKE.get(),
};

/// A new process at `pid`: its pid version moves.
fn reborn(pid: u32) {
    REBORN.with(|r| *r.borrow_mut().entry(pid).or_default() += 1);
}

fn kernel(rows: &[(u32, &str, i64, bool)]) {
    KERNEL.with(|k| {
        *k.borrow_mut() = rows
            .iter()
            .map(|&(pid, name, ms, apple)| (pid, (name.to_owned(), ms, apple)))
            .collect();
    });
}

fn gauge() -> &'static Arming {
    Box::leak(Box::new(Arming::new()))
}

fn stand_in_reader() -> (Reader, &'static Arming) {
    let armed = gauge();
    let mut reader = Reader::new(Arc::default());
    reader.split = SplitReader::new(STAND_IN, armed);
    (reader, armed)
}

fn bars() -> Request {
    Request {
        self_pid: std::process::id(),
        cadence_ms: 1_000,
        deck: true,
        ..Request::default()
    }
}

fn just_read(reader: &mut Reader) {
    reader.split.last = Some(Instant::now());
}

#[test]
fn split_ticks_ride_the_bars_and_an_armed_pull_only() {
    let t0 = Instant::now();
    let ticks = |req: &Request, period_ms: u64, readings: u64, armed: bool| {
        let mut last = None;
        let mut n = 0;
        for k in 0..readings {
            let late = if k % 2 == 0 { 3 } else { 0 };
            let now = t0 + Duration::from_millis(k * period_ms + late);
            if split_due(last, req, armed, now) {
                last = Some(now);
                n += 1;
            }
        }
        n
    };
    let with = |cadence_ms| Request {
        cadence_ms,
        deck: true,
        ..Request::default()
    };
    assert_eq!(
        ticks(&with(1_000), 1_000, 21, false),
        5,
        "0, 5, 10, 15, 20 s"
    );
    assert_eq!(ticks(&with(500), 500, 21, false), 11, "once a second");
    assert_eq!(ticks(&with(5_000), 5_000, 5, false), 5, "every reading");
    let engine = Request {
        jobs_now: true,
        ..with(1_000)
    };
    let hidden = Request {
        machine_only: true,
        ..with(10_000)
    };
    let monitor = Request {
        deck: false,
        ..with(1_000)
    };
    let pulled = Request {
        pull: true,
        ..with(3_000)
    };
    for armed in [false, true] {
        assert_eq!(ticks(&engine, 1_000, 21, armed), 0, "the engine's");
        assert_eq!(ticks(&hidden, 10_000, 5, armed), 0, "a hidden deck's");
        assert_eq!(ticks(&monitor, 1_000, 21, armed), 0, "the Monitor tab's");
    }
    assert_eq!(ticks(&pulled, 1_000, 21, false), 0, "an unarmed pull");
    assert_eq!(ticks(&pulled, 1_000, 21, true), 21, "each armed pull");
    assert!(split_due(None, &pulled, true, t0), "the first at once");
    assert!(!split_due(Some(t0), &pulled, true, t0 + MIN_WINDOW / 2));
    assert!(split_due(Some(t0), &pulled, true, t0 + MIN_WINDOW));
}

#[test]
fn the_coalitions_are_read_on_split_ticks_only() {
    let (mut reader, armed) = stand_in_reader();
    let reads = || {
        let r = READS.get();
        (r.coalitions, r.swept)
    };
    READS.take();
    reader.read_machine(&Request {
        machine_only: true,
        cadence_ms: 10_000,
        ..bars()
    });
    assert_eq!(reads(), (0, 0), "a hidden deck's machine counters");
    reader.read(&Request {
        jobs_now: true,
        ..bars()
    });
    assert_eq!(reads(), (0, 0), "the jobs engine's reading");
    reader.read(&Request {
        deck: false,
        ..bars()
    });
    assert_eq!(reads(), (0, 0), "the Fleet Monitor tab's");
    let _ = pull(&mut reader, &mut bars(), Some(Instant::now()));
    assert_eq!(reads(), (0, 0), "an unarmed pull");
    reader.read(&bars());
    assert_eq!(reads(), (1, 1), "the bars' first reading");
    let kept = reader.split.coalition_ids.len();
    assert_eq!(
        kept > 0,
        cfg!(target_os = "macos"),
        "its coalitions kept: {kept}"
    );
    just_read(&mut reader);
    reader.read(&bars());
    assert_eq!(reads(), (1, 1), "the bars' next, at once");
    assert_eq!(
        reader.split.coalition_ids.len(),
        kept,
        "a plain sweep leaves them"
    );
    {
        let _armed = armed.arm();
        assert!(!reader.split_cold(), "the last split tick is fresh");
        let _ = pull(&mut reader, &mut bars(), Some(Instant::now()));
    }
    assert_eq!(reads(), (2, 2), "a warm pull while armed");
}

#[test]
fn the_loop_asks_for_split_ticks_only_while_a_window_in_view_shows_the_bars() {
    let mut app = crate::App::headless_for_test();
    let deck = |app: &crate::App| app.vitals_request(FOCUSED_PERIOD, false).deck;
    let form = |app: &mut crate::App, word: &str| {
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_bars = Some(word.into());
    };
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Tabs));
    assert!(app.tabs_page_shown());
    assert!(!deck(&app), "the Fleet Monitor tab alone");
    let _bars = crate::stats_deck::tests::attach_stats();
    for (word, shown) in [("deck", true), ("sill", true), ("off", false)] {
        form(&mut app, word);
        assert_eq!(deck(&app), shown, "{word}");
    }
    form(&mut app, "deck");
    for ws in app.windows.values_mut() {
        ws.occluded = true;
    }
    assert!(!deck(&app), "every window occluded");
}

fn assert_window(snap: &Snapshot) {
    if cfg!(unix) {
        let why = snap.why.as_deref().expect("the split's facts");
        assert_eq!(why.at_ns, snap.at_ns, "this reading's split");
        assert_eq!(why.split.via, Via::Coalitions);
        let window = Duration::from_nanos(why.window_ns);
        assert!(
            window >= COLD_PAIR && window <= SPLIT_WINDOW_MAX,
            "{window:?}"
        );
    }
}

#[test]
fn an_armed_pull_with_no_fresh_split_tick_reads_a_pair_and_has_a_window() {
    let (mut reader, armed) = stand_in_reader();
    let arming = armed.arm();
    READS.take();
    assert!(reader.split_cold(), "no split tick yet");
    let snap = pull(&mut reader, &mut bars(), None);
    assert_eq!(
        READS.take().coalitions,
        2,
        "the base, then the window's end"
    );
    assert_window(&snap);

    let gap = SPLIT_WINDOW_MAX + Duration::from_secs(1);
    let stale = || Instant::now().checked_sub(gap);
    reader.epoch = reader.epoch.checked_sub(gap).expect("a minute since boot");
    reader.split.last = stale();
    assert!(reader.split_cold(), "a base too old for a window");
    let snap = pull(&mut reader, &mut bars(), Some(Instant::now()));
    assert_eq!(READS.take().coalitions, 2, "cold for the split alone");
    assert_window(&snap);

    drop(arming);
    reader.split.last = stale();
    assert!(!reader.split_cold(), "no `sys why` arms the split");
    let _ = pull(&mut reader, &mut bars(), Some(Instant::now()));
    assert_eq!(READS.take().coalitions, 0);
}

struct Paced {
    vitals: aterm_vitals::Vitals,
    cold: bool,
    taken: Vec<Instant>,
}

impl Source for Paced {
    fn reading(&mut self, _req: &Request) -> Snapshot {
        self.taken.push(Instant::now());
        Snapshot::default()
    }

    fn model(&mut self) -> &mut aterm_vitals::Vitals {
        &mut self.vitals
    }

    fn split_cold(&self) -> bool {
        self.cold
    }
}

#[test]
fn a_pull_cold_for_the_split_alone_keeps_the_floor() {
    for (cold, readings) in [(true, 2), (false, 1)] {
        let mut source = Paced {
            vitals: aterm_vitals::Vitals::new(),
            cold,
            taken: Vec::new(),
        };
        let last = Instant::now();
        let _ = pull(&mut source, &mut bars(), Some(last));
        assert_eq!(source.taken.len(), readings, "cold: {cold}");
        assert!(
            source.taken[0].saturating_duration_since(last) >= MIN_WINDOW,
            "cold: {cold}"
        );
    }
}

#[test]
fn a_pull_is_cold_for_the_split_a_margin_inside_the_longest_window() {
    let armed = gauge();
    let mut split = SplitReader::new(STAND_IN, armed);
    let t0 = Instant::now();
    split.last = Some(t0);
    let hair = t0 + SPLIT_WINDOW_MAX - Duration::from_millis(100);
    let inside = t0 + SPLIT_WINDOW_MAX - SPLIT_COLD_MARGIN - Duration::from_millis(100);
    assert!(!split.cold(hair), "unarmed");
    let _armed = armed.arm();
    assert!(split.cold(hair), "a hair inside the limit");
    assert!(!split.cold(inside), "the margin and more inside");
}

#[test]
fn a_kernel_name_is_the_swept_one_whole_or_cut_by_one() {
    assert!(names_the_same("mds_stores", "mds_stores"));
    assert!(names_the_same("SetStoreUpdateSe", "SetStoreUpdateS"));
    let swept = String::from_utf8_lossy(&"Приложен".as_bytes()[..15]);
    assert_eq!(swept, "Приложе\u{FFFD}");
    assert!(names_the_same("Приложен", &swept));
    assert!(!names_the_same("Приложен", "Приложе"));
    assert!(!names_the_same("mds_stores", "mds"));
    assert!(!names_the_same("mds", "mds_stores"));
}

/// THE KERNEL'S READERS READ THIS PROCESS AS A SPLIT TICK TAKES IT (macOS,
/// live, [`CoalProbe::KERNEL`]): the self-check passes and launchd reads as
/// Apple's, so the lane is on and vouches; the sweep asked for coalitions
/// reads this process's, the one the self-check named, and its pid version,
/// and keeps the coalition under that version for the next sweep; its pid
/// version and launchd's read, its own the same twice; and its birth reads
/// the same in unix ms through its record, through the table and through
/// its BSD info (`aterm_uds::process::read_process_birth`), under a name
/// that is the sweep's for it, which the sweep holds cut (the test binary,
/// `aterm_gui-<hash>`, is named past the short info's 15 bytes). NEGATIVE
/// CONTROL: launchd's record names another process than this one's row.
#[cfg(target_os = "macos")]
#[test]
fn the_kernels_readers_read_this_process_as_a_split_tick_takes_it() {
    let me = std::process::id();
    let kernel = CoalProbe::KERNEL;
    let split = SplitReader::new(kernel, gauge());
    assert_eq!((split.lane, split.vouches), (CoalLane::On, true));
    let coalition = (kernel.self_check)(me).expect("the self-check");
    let mut ids = aterm_sysprobe::coalitions::CoalitionIds::default();
    let opts = aterm_sysprobe::SweepOpts {
        coalitions: true,
        ..aterm_sysprobe::SweepOpts::default()
    };
    let sweep = (kernel.sweep)(opts, &mut ids).expect("a sweep");
    let row = sweep
        .procs
        .iter()
        .find(|p| p.pid == me)
        .expect("this process swept");
    assert_eq!(row.coalition, Some(coalition));
    let swept_version = row.version.expect("its pid version, swept");
    assert_eq!(
        ids.kept(me),
        Some((swept_version, coalition)),
        "kept for the next"
    );
    let again = (kernel.sweep)(opts, &mut ids).expect("a second sweep");
    let row_again = again.procs.iter().find(|p| p.pid == me).expect("swept");
    assert_eq!(
        (row_again.coalition, row_again.version),
        (Some(coalition), Some(swept_version))
    );
    let (name, ms) = (kernel.birth)(me).expect("this process's record");
    let bsd = aterm_uds::process::read_process_birth(i32::try_from(me).expect("a pid"))
        .expect("its BSD info");
    let bsd_ms = bsd.seconds * 1000 + bsd.microseconds / 1000;
    assert_eq!(u64::try_from(ms).ok(), Some(bsd_ms));
    assert_eq!(
        (kernel.births)().and_then(|table| table.get(&me).cloned()),
        Some((name.clone(), ms))
    );
    assert!(row.name.len() < name.len(), "{name:?} swept {:?}", row.name);
    assert!(
        names_the_same(&name, &row.name),
        "{name:?} swept {:?}",
        row.name
    );
    let (launchd, _) = (kernel.birth)(1).expect("launchd's record");
    assert!(!names_the_same(&launchd, &row.name), "{launchd:?}");
    let version = (kernel.version)(me).expect("this process's pid version");
    assert_eq!((kernel.version)(me), Some(version), "no exec between");
    assert_eq!(version, swept_version, "the sweep's");
    assert!((kernel.version)(1).is_some(), "launchd's, another user's");
}

#[test]
fn after_a_long_gap_every_birth_and_identity_is_read_again() {
    let now = Cell::new(Instant::now());
    let tick = |split: &mut SplitReader, procs| tick_at(split, procs, now.get());
    kernel(&[(1, "launchd", 1_000, true), (387, "mds", 2_000, true)]);
    let mut split = SplitReader::new(STAND_IN, gauge());
    let mut procs = vec![swept(387, 307, "mds", 20), swept(1, 1, "launchd", 10)];
    let _ = tick(&mut split, procs.clone());
    kernel(&[(1, "launchd", 1_000, true), (387, "mds", 9_000, true)]);
    READS.take();
    let s = tick(&mut split, procs.clone());
    assert_eq!(births(&s), [Some(2_000), Some(1_000)], "inside a window");
    assert_eq!(sent(&s).len(), 2, "every lead is sent");
    now.set(now.get() + SPLIT_WINDOW_MAX + Duration::from_secs(1));
    let s = tick(&mut split, procs.clone());
    assert_eq!(births(&s), [Some(9_000), Some(1_000)]);
    assert_eq!(
        sent(&s),
        [
            (1, 1_000, "launchd", Voucher::Apple),
            (387, 9_000, "mds", Voucher::Apple),
        ]
    );
    let r = READS.take();
    assert_eq!(
        (r.births, r.identities),
        (2, 2),
        "kept inside the window, read again after the gap"
    );

    for p in &mut procs {
        p.start = None;
    }
    for born in [11_000, 12_000] {
        kernel(&[(1, "launchd", 1_000, true), (387, "mds", born, true)]);
        reborn(387);
        let s = tick(&mut split, procs.clone());
        assert_eq!(births(&s), [Some(born), Some(1_000)], "no start: {born}");
    }
    let r = READS.take();
    assert_eq!(
        (r.births, r.identities),
        (2, 2),
        "with no start, a birth is kept under its pid version: a lead born anew (a new \
         version) is read again; launchd's is kept"
    );
    UNVERSIONED.set(Some(1));
    for _ in 0..2 {
        let s = tick(&mut split, procs.clone());
        assert_eq!(births(&s), [Some(12_000), Some(1_000)]);
    }
    UNVERSIONED.set(None);
    assert_eq!(
        READS.take().births,
        2,
        "NEGATIVE CONTROL: with neither a start nor a version, read at each tick"
    );
}

/// A lead's identity is read once and kept while its birth and pid version
/// stay; an exec (its version moved) reads it again, a missed read is
/// unknown and asked again at the next tick, a read an exec raced is unknown
/// and asked again, and a lead whose version reads nothing is read at every
/// tick. NEGATIVE CONTROL: with nothing moved, nothing is read again.
#[test]
fn a_lead_identity_is_read_again_only_after_an_exec_or_a_miss() {
    let now = Instant::now();
    let tick = |split: &mut SplitReader, procs| tick_at(split, procs, now);
    kernel(&[(1, "launchd", 1_000, true), (387, "mds", 2_000, true)]);
    let mut split = SplitReader::new(STAND_IN, gauge());
    let procs = vec![swept(387, 307, "mds", 20)];
    MISSES.set(Some(387));
    READS.take();
    assert_eq!(
        sent(&tick(&mut split, procs.clone())),
        [(387, 2_000, "", Voucher::Unknown)],
        "missed"
    );
    assert_eq!(
        sent(&tick(&mut split, procs.clone())),
        [(387, 2_000, "mds", Voucher::Apple)]
    );
    assert_eq!(READS.take().identities, 2, "a miss is asked again");
    assert_eq!(
        sent(&tick(&mut split, procs.clone())),
        [(387, 2_000, "mds", Voucher::Apple)]
    );
    assert_eq!(READS.take().identities, 0, "kept: nothing moved");

    EXECS.set(EXECS.get() + 1);
    MISSES.set(Some(387));
    let unknown = tick(&mut split, procs.clone());
    assert_eq!(
        sent(&unknown),
        [(387, 2_000, "", Voucher::Unknown)],
        "after an exec, a miss is a current unknown"
    );
    assert_eq!(unknown.leads[0].signing_id, None);
    assert_eq!(unknown.leads[0].team, None);
    kernel(&[(1, "launchd", 1_000, true), (387, "mds", 2_000, false)]);
    assert_eq!(
        sent(&tick(&mut split, procs.clone())),
        [(387, 2_000, "mds", Voucher::DeveloperId)],
        "an exec into another signature of the same name and birth"
    );
    assert_eq!(READS.take().identities, 2);

    kernel(&[(1, "launchd", 1_000, true), (387, "mds", 2_000, true)]);
    RACES.set(Some(387));
    assert_eq!(
        sent(&tick(&mut split, procs.clone())),
        [(387, 2_000, "", Voucher::Unknown)],
        "an exec raced the read"
    );
    for _ in 0..2 {
        assert_eq!(
            sent(&tick(&mut split, procs.clone())),
            [(387, 2_000, "mds", Voucher::Apple)]
        );
    }
    assert_eq!(READS.take().identities, 2, "read again once, then kept");

    UNVERSIONED.set(Some(387));
    for _ in 0..2 {
        assert_eq!(
            sent(&tick(&mut split, procs.clone())),
            [(387, 2_000, "mds", Voucher::Apple)]
        );
    }
    UNVERSIONED.set(None);
    assert_eq!(READS.take().identities, 2, "no version: read at each tick");
}

#[test]
fn a_misread_turns_the_hosts_lane_off_for_good() {
    const MISREAD: CoalProbe = CoalProbe {
        coalitions: || {
            count(|r| r.coalitions += 1);
            Some(CoalitionRead {
                listed: 1,
                newest: Some(1),
                truncated: false,
                rows: vec![CoalitionUsage {
                    id: 1,
                    tasks_started: 1,
                    footprint: Some(u64::MAX / 2),
                    ..CoalitionUsage::default()
                }],
            })
        },
        ..STAND_IN
    };
    if !cfg!(unix) {
        return;
    }
    let armed = gauge();
    let mut reader = Reader::new(Arc::default());
    reader.split = SplitReader::new(MISREAD, armed);
    READS.take();
    let snap = reader.read(&bars());
    assert_eq!(READS.take().coalitions, 1);
    assert_eq!(
        (snap.coal_lane, reader.split.lane),
        (CoalLane::Off, CoalLane::Off)
    );
    let _armed = armed.arm();
    reader.split.last = None;
    let _ = reader.read(&bars());
    let _ = pull(&mut reader, &mut bars(), Some(Instant::now()));
    assert_eq!(READS.take().coalitions, 0, "never again");
}

#[test]
fn a_swept_processs_coalition_and_pid_version_reach_the_model() {
    let epoch = Instant::now();
    let read = |coalition, version| aterm_sysprobe::ProcessReading {
        pid: 9,
        coalition,
        version,
        ..aterm_sysprobe::ProcessReading::default()
    };
    let p = proc_sample(&read(Some(7), Some(5)), epoch);
    assert_eq!(
        (p.coalition, p.version, p.birth_ms),
        (Some(7), Some(5), None)
    );
    let p = proc_sample(&read(None, None), epoch);
    assert_eq!((p.coalition, p.version), (None, None));
}

/// EACH LEAD'S PID VERSION REACHES THE SPLIT FROM THE SWEEP (macOS, live): over the kernel's
/// sweep, this process leads its coalition, and its identity, read at the first split tick, is
/// kept for the next, since the version the sweep read for it (through [`proc_sample`]) is the
/// one read again after the identity. NEGATIVE CONTROL: with the sweep's versions taken away, it
/// is read at each split tick.
#[cfg(target_os = "macos")]
#[test]
fn a_leads_pid_version_reaches_the_split_from_the_sweep() {
    const LIVE: CoalProbe = CoalProbe {
        version: aterm_sysprobe::pid_version,
        ..STAND_IN
    };
    const NO_VERSION_SWEPT: CoalProbe = CoalProbe {
        sweep: |opts, ids| {
            let mut sweep = (LIVE.sweep)(opts, ids)?;
            for p in &mut sweep.procs {
                p.version = None;
            }
            Some(sweep)
        },
        ..LIVE
    };
    let me = std::process::id();
    let name = aterm_sysprobe::processes()
        .expect("a sweep")
        .procs
        .into_iter()
        .find(|p| p.pid == me)
        .expect("this process swept")
        .name;
    kernel(&[(me, &name, 5_000, false)]);
    for (probe, identities, what) in [
        (LIVE, 1, "read once, then kept under the swept version"),
        (NO_VERSION_SWEPT, 2, "no version swept: read at each tick"),
    ] {
        let mut reader = Reader::new(Arc::default());
        reader.split = SplitReader::new(probe, gauge());
        READS.take();
        for _ in 0..2 {
            reader.split.last = None;
            let _ = reader.read(&bars());
        }
        let r = READS.take();
        assert_eq!((r.swept, r.identities), (2, identities), "{what}");
    }
}

#[test]
fn the_coalitions_reach_the_model_field_for_field() {
    let s = coal_sample(CoalitionRead {
        listed: 9,
        newest: Some(12),
        truncated: true,
        rows: vec![CoalitionUsage {
            id: 7,
            tasks_started: 11,
            tasks_exited: 3,
            cpu_ns: 5_000,
            bytes_read: 6_000,
            eqos_ns: [1, 2, 3, 4, 5, 6, 7],
            footprint: Some(8_000),
        }],
    });
    assert_eq!((s.listed, s.newest, s.truncated), (9, Some(12), true));
    assert_eq!(
        s.rows,
        [CoalUsage {
            id: 7,
            tasks_started: 11,
            tasks_exited: 3,
            cpu_ns: 5_000,
            bytes_read: 6_000,
            eqos_ns: [1, 2, 3, 4, 5, 6, 7],
            footprint: Some(8_000),
        }]
    );
}

#[test]
fn each_kernel_voucher_is_the_models_of_the_same_name() {
    for (status, category, voucher) in [
        (CS_VALID | CS_PLATFORM_BINARY, Some(1), Voucher::Apple),
        (CS_VALID, Some(4), Voucher::AppStore),
        (CS_VALID, Some(6), Voucher::DeveloperId),
        (CS_VALID, Some(3), Voucher::Development),
        (CS_VALID, Some(5), Voucher::Enterprise),
        (CS_VALID | CS_ADHOC, Some(10), Voucher::AdHoc),
        (0, None, Voucher::Unsigned),
        (CS_VALID, None, Voucher::Unknown),
    ] {
        let id = Identity {
            exe: "lead".into(),
            cs_status: Some(status),
            category,
            ..Identity::default()
        };
        assert_eq!(lead_identity(9, 1, id.clone(), true).voucher, voucher);
        assert_eq!(lead_identity(9, 1, id, false).voucher, Voucher::Unknown);
    }
}

fn swept(pid: u32, coalition: u64, name: &str, start: u64) -> ProcSample {
    ProcSample {
        pid,
        coalition: Some(coalition),
        name: name.to_owned(),
        start: Some(start),
        uid: Some(501),
        ..ProcSample::default()
    }
}

fn tick(split: &mut SplitReader, procs: Vec<ProcSample>) -> Sample {
    tick_at(split, procs, Instant::now())
}

/// One split tick over `procs`, each process's pid version read as the
/// sweep reads it.
fn tick_at(split: &mut SplitReader, mut procs: Vec<ProcSample>, now: Instant) -> Sample {
    for p in &mut procs {
        p.version = (split.probe.version)(p.pid);
    }
    let mut sample = Sample {
        procs: Some(procs),
        ..Sample::default()
    };
    let read = (split.probe.coalitions)().expect("the stand-in's list");
    split.fill(&mut sample, read, now);
    sample
}

fn sent(s: &Sample) -> Vec<(u32, i64, &str, Voucher)> {
    s.leads
        .iter()
        .map(|l| (l.pid, l.birth_ms, l.exe.as_str(), l.voucher))
        .collect()
}

fn births(s: &Sample) -> Vec<Option<i64>> {
    s.procs.iter().flatten().map(|p| p.birth_ms).collect()
}

#[test]
fn known_start_births_and_lead_identities_are_kept_until_they_move() {
    let now = Instant::now();
    let tick = |split: &mut SplitReader, procs| tick_at(split, procs, now);
    kernel(&[
        (1, "launchd", 1_000, true),
        (387, "mds", 2_000, true),
        (388, "mdworker", 2_100, true),
        (900, "zsh", 3_000, false),
    ]);
    let mut split = SplitReader::new(STAND_IN, gauge());
    assert!(split.vouches, "launchd reads as Apple's");
    READS.take();
    let mut procs = vec![
        swept(900, 1342, "zsh", 30),
        swept(388, 307, "mdworker", 21),
        swept(387, 307, "mds", 20),
        swept(1, 1, "launchd", 10),
    ];
    let s = tick(&mut split, procs.clone());
    assert_eq!(
        births(&s),
        [Some(3_000), Some(2_100), Some(2_000), Some(1_000)]
    );
    assert_eq!(
        sent(&s),
        [
            (1, 1_000, "launchd", Voucher::Apple),
            (387, 2_000, "mds", Voucher::Apple),
            (900, 3_000, "zsh", Voucher::DeveloperId),
        ]
    );
    assert!(s.coalitions.is_some() && split.last.is_some());
    let r = READS.take();
    assert_eq!((r.births, r.identities), (4, 3));

    let s = tick(&mut split, procs.clone());
    assert_eq!(
        sent(&s),
        [
            (1, 1_000, "launchd", Voucher::Apple),
            (387, 2_000, "mds", Voucher::Apple),
            (900, 3_000, "zsh", Voucher::DeveloperId),
        ]
    );
    assert_eq!(
        births(&s),
        [Some(3_000), Some(2_100), Some(2_000), Some(1_000)]
    );
    let r = READS.take();
    assert_eq!((r.births, r.identities), (0, 0), "both are kept");

    kernel(&[
        (1, "launchd", 1_000, true),
        (387, "mds", 2_000, false),
        (388, "mdworker", 2_100, true),
        (900, "zsh", 3_000, false),
    ]);
    let s = tick(&mut split, procs.clone());
    assert_eq!(
        sent(&s),
        [
            (1, 1_000, "launchd", Voucher::Apple),
            (387, 2_000, "mds", Voucher::DeveloperId),
            (900, 3_000, "zsh", Voucher::DeveloperId),
        ],
        "an exec into another signature, the same name and birth"
    );
    let r = READS.take();
    assert_eq!((r.births, r.identities), (0, 1));

    kernel(&[
        (1, "launchd", 1_000, true),
        (387, "mds_stores", 2_000, true),
        (388, "mdworker", 2_100, true),
        (900, "zsh", 3_000, false),
    ]);
    procs[2].name = "mds_stores".into();
    let s = tick(&mut split, procs.clone());
    assert_eq!(
        sent(&s),
        [
            (1, 1_000, "launchd", Voucher::Apple),
            (387, 2_000, "mds_stores", Voucher::Apple),
            (900, 3_000, "zsh", Voucher::DeveloperId),
        ]
    );
    let r = READS.take();
    assert_eq!((r.births, r.identities), (1, 1));

    kernel(&[
        (1, "launchd", 1_000, true),
        (387, "mds_stores", 2_000, true),
        (388, "mdworker", 2_100, true),
        (900, "python3", 9_000, false),
    ]);
    procs[0] = swept(900, 1342, "python3", 90);
    let s = tick(&mut split, procs.clone());
    assert_eq!(births(&s)[0], Some(9_000));
    assert_eq!(
        sent(&s),
        [
            (1, 1_000, "launchd", Voucher::Apple),
            (387, 2_000, "mds_stores", Voucher::Apple),
            (900, 9_000, "python3", Voucher::DeveloperId),
        ]
    );
    let r = READS.take();
    assert_eq!((r.births, r.identities), (1, 1), "a lead born anew");

    let without: Vec<ProcSample> = procs.iter().filter(|p| p.pid != 387).cloned().collect();
    let s = tick(&mut split, without);
    assert_eq!(
        sent(&s),
        [
            (1, 1_000, "launchd", Voucher::Apple),
            (388, 2_100, "mdworker", Voucher::Apple),
            (900, 9_000, "python3", Voucher::DeveloperId),
        ]
    );
    assert!(!split.births.contains_key(&387));
    let s = tick(&mut split, procs.clone());
    assert_eq!(
        sent(&s),
        [
            (1, 1_000, "launchd", Voucher::Apple),
            (387, 2_000, "mds_stores", Voucher::Apple),
            (900, 9_000, "python3", Voucher::DeveloperId),
        ]
    );
    let r = READS.take();
    assert_eq!(
        (r.births, r.identities),
        (1, 2),
        "a lead that stopped leading is read again"
    );

    kernel(&[
        (1, "launchd", 1_000, true),
        (387, "mds_stores", 2_000, true),
        (388, "sharingd", 9_500, true),
        (900, "python3", 9_000, false),
    ]);
    procs[1].start = Some(22);
    let s = tick(&mut split, procs.clone());
    assert_eq!(births(&s)[1], None);
    let s = tick(&mut split, procs);
    assert_eq!(births(&s)[1], None);
    assert_eq!(READS.take().births, 2, "read at each tick");
}

#[test]
fn many_births_are_read_in_one_read_of_the_table() {
    for (n, tables, records) in [
        (BIRTHS_AT_ONCE + 1, 1, 0),
        (BIRTHS_AT_ONCE, 0, BIRTHS_AT_ONCE),
    ] {
        let pids = 1_000..1_000 + u32::try_from(n).expect("a few pids");
        let rows: Vec<(u32, String, i64)> = pids
            .clone()
            .map(|pid| (pid, format!("SetStoreUpdateSe{pid}"), i64::from(pid) * 10))
            .collect();
        kernel(
            &rows
                .iter()
                .map(|(pid, name, ms)| (*pid, &name[..16], *ms, false))
                .collect::<Vec<_>>(),
        );
        let mut split = SplitReader::new(STAND_IN, gauge());
        READS.take();
        let procs: Vec<ProcSample> = pids
            .map(|pid| swept(pid, u64::from(pid), "SetStoreUpdateS", u64::from(pid)))
            .collect();
        let s = tick(&mut split, procs.clone());
        assert!(
            births(&s).iter().all(Option::is_some),
            "{n}: {:?}",
            births(&s)
        );
        let r = READS.take();
        assert_eq!((r.tables, r.births), (tables, records), "{n}");
        let _ = tick(&mut split, procs);
        let r = READS.take();
        assert_eq!((r.tables, r.births), (0, 0), "{n}: each read once");
    }
}

thread_local! {
    /// [`FOREIGN`]'s processes by pid and name, each another user's.
    static FOREIGN_ROWS: RefCell<Vec<(u32, String)>> = const { RefCell::new(Vec::new()) };
}

/// The stand-in kernel with a sweep of [`FOREIGN_ROWS`] and this process's
/// own row (as the kernel's sweep reads it, so a delegated job another test
/// carried for this process still reads complete): each of the others
/// another user's, whose usage the kernel refuses, so the sweep reads no
/// start for it, and its pid version only where the options ask (as the
/// kernel's sweep reads it).
const FOREIGN: CoalProbe = CoalProbe {
    sweep: |opts, _| {
        count(|r| {
            r.swept += usize::from(opts.coalitions);
            r.versioned += usize::from(opts.versions);
        });
        let me = std::process::id();
        let own = aterm_sysprobe::processes()?
            .procs
            .into_iter()
            .find(|p| p.pid == me)?;
        let mut procs: Vec<aterm_sysprobe::ProcessReading> = FOREIGN_ROWS.with(|rows| {
            rows.borrow()
                .iter()
                .map(|(pid, name)| aterm_sysprobe::ProcessReading {
                    pid: *pid,
                    ppid: 1,
                    name: name.clone(),
                    uid: Some(0),
                    coalition: opts.coalitions.then_some(u64::from(*pid)),
                    version: (opts.coalitions || opts.versions)
                        .then(|| (STAND_IN.version)(*pid))
                        .flatten(),
                    ..aterm_sysprobe::ProcessReading::default()
                })
                .collect()
        });
        procs.push(own);
        Some(aterm_sysprobe::Sweep {
            procs,
            truncated: false,
        })
    },
    ..STAND_IN
};

/// ANOTHER USER'S PROCESS IS KNOWN FROM ONE WHOLE READING TO THE NEXT
/// (§7.2 item 1): with more than [`BIRTHS_AT_ONCE`] such processes, whose
/// start no sweep reads, the first reading reads the births table and the
/// plain readings and split ticks after it read none, since every sweep
/// reads their pid versions. NEGATIVE CONTROL: a new process at one of their
/// pids, under the same name, moves its version and is read again (its one
/// record), and its new birth is the one kept.
#[test]
fn another_users_births_are_read_once_across_plain_readings_and_split_ticks() {
    const BORN: i64 = 1_790_000_000_000;
    let me = std::process::id();
    let own = own_swept_name();
    let pids: Vec<u32> = (2_000..)
        .filter(|pid| *pid != me)
        .take(BIRTHS_AT_ONCE + 1)
        .collect();
    let names: Vec<String> = pids.iter().map(|pid| format!("rootd{pid}")).collect();
    let again = pids[5];
    let rows = |again_ms: Option<i64>| {
        let mut rows: Vec<(u32, &str, i64, bool)> = pids
            .iter()
            .zip(&names)
            .map(|(pid, name)| {
                let ms = again_ms
                    .filter(|_| *pid == again)
                    .unwrap_or(i64::from(*pid) * 10);
                (*pid, name.as_str(), ms, true)
            })
            .collect();
        rows.push((me, own.as_str(), BORN, false));
        kernel(&rows);
    };
    rows(None);
    FOREIGN_ROWS.with(|r| *r.borrow_mut() = pids.iter().copied().zip(names.clone()).collect());
    let mut reader = Reader::new(Arc::default());
    reader.split = SplitReader::new(FOREIGN, gauge());
    let born = |reader: &Reader, pid: u32| reader.split.births.get(&pid).map(|b| b.ms);
    let all_born = |reader: &Reader| {
        pids.iter()
            .all(|pid| born(reader, *pid) == Some(i64::from(*pid) * 10))
    };
    READS.take();
    let _ = reader.read(&bars());
    let r = READS.take();
    assert_eq!((r.tables, r.births), (1, 0), "the table, once");
    assert_eq!(r.swept, 1, "the first, a split tick");
    assert!(all_born(&reader), "{:?}", reader.split.births);

    for k in 1..=8 {
        if k % 4 == 0 {
            reader.split.last = Instant::now().checked_sub(SPLIT_EVERY);
        } else {
            just_read(&mut reader);
        }
        let _ = reader.read(&bars());
        assert!(all_born(&reader), "reading {k}: {:?}", reader.split.births);
    }
    let r = READS.take();
    assert_eq!((r.tables, r.births), (0, 0), "every birth kept");
    assert_eq!(
        (r.swept, r.versioned),
        (2, 8),
        "6 plain readings and 2 split ticks, each sweep reading versions"
    );

    rows(Some(99_000));
    reborn(again);
    just_read(&mut reader);
    let _ = reader.read(&bars());
    let r = READS.take();
    assert_eq!((r.swept, r.tables, r.births), (0, 0, 1), "NEGATIVE CONTROL");
    assert_eq!(
        born(&reader, again),
        Some(99_000),
        "the new process's birth"
    );
    just_read(&mut reader);
    let _ = reader.read(&bars());
    assert_eq!(born(&reader, again), Some(99_000));
    let r = READS.take();
    assert_eq!((r.tables, r.births), (0, 0), "kept under its new version");
}

#[test]
fn every_voucher_is_unknown_when_launchd_does_not_read_as_apples() {
    for (launchd, voucher) in [(false, Voucher::Unknown), (true, Voucher::Apple)] {
        kernel(&[(1, "launchd", 1_000, launchd), (387, "mds", 2_000, true)]);
        let mut split = SplitReader::new(STAND_IN, gauge());
        assert_eq!((split.lane, split.vouches), (CoalLane::On, launchd));
        let s = tick(&mut split, vec![swept(387, 307, "mds", 20)]);
        assert_eq!(sent(&s), [(387, 2_000, "mds", voucher)]);
    }
}

#[test]
fn the_timeline_is_read_again_only_when_the_machine_woke() {
    WAKE.set(Some(5_000));
    let mut split = SplitReader::new(STAND_IN, gauge());
    READS.take();
    let wake = |s: &Sample| s.timeline.as_ref().and_then(|t| t.wake_ms);
    for _ in 0..3 {
        assert_eq!(wake(&tick(&mut split, Vec::new())), Some(5_000));
    }
    assert_eq!(READS.take().timelines, 1);
    WAKE.set(Some(9_000));
    assert_eq!(wake(&tick(&mut split, Vec::new())), Some(9_000));
    assert_eq!(READS.take().timelines, 1);
}

#[test]
fn the_lane_is_the_self_checks_and_the_test_readers_is_inert() {
    let armed = gauge();
    let reader_of = |self_check: fn(u32) -> Result<u64, &'static str>| {
        SplitReader::new(
            CoalProbe {
                self_check,
                ..STAND_IN
            },
            armed,
        )
    };
    assert_eq!(reader_of(|_| Ok(7)).lane, CoalLane::On);
    assert_eq!(
        reader_of(|_| Err("unsupported")).lane,
        CoalLane::Unsupported
    );
    let off = reader_of(|_| Err("layout"));
    assert_eq!(off.lane, CoalLane::Off);
    let _armed = armed.arm();
    let now = Instant::now();
    assert!(!off.due(&bars(), now) && !off.cold(now));
    assert!(!off.due(
        &Request {
            pull: true,
            ..bars()
        },
        now
    ));

    let mut reader = Reader::new(Arc::default());
    assert_eq!(reader.split.lane, CoalLane::Unsupported);
    let snap = reader.read(&bars());
    assert_eq!(snap.coal_lane, CoalLane::Unsupported);
    assert!(reader.split.last.is_none(), "no split tick");
}

/// THE WHY FIXTURE READS ITS SCENE OVER THE HOST'S OWN SPLIT TICKS (the
/// `ATERM_DEV_WHY_FIXTURE` seam): RD's 10:32 scene, split at 0 and 5 s with a
/// sweep at 4 s or at any other spacing (0, 3.7, 6.2 s), names Spotlight
/// after start-up over the scene's 3.30 cores, while the machine's own
/// memory rides on where the scene has none. NEGATIVE CONTROL: the calm
/// scene names no covering cause, and its memory is its own.
#[cfg(debug_assertions)]
#[test]
fn the_why_fixture_reads_its_scene_over_the_hosts_own_split_ticks() {
    use aterm_vitals::explain::fixtures::{calm_1510, spotlight_1032};
    let live = Sample {
        procs: Some(Vec::new()),
        mem: Some(MemSample {
            total: 8 << 30,
            ..MemSample::default()
        }),
        cadence_ms: 1_000,
        ..Sample::default()
    };
    let read = |scene, at: [u64; 3]| {
        let fixture = WhyFixture { scene, t0: None };
        let mut vitals = aterm_vitals::Vitals::new();
        vitals.absorb(&fixture.at(&live, true, at[0]));
        vitals.absorb(&fixture.at(&live, false, at[1]));
        vitals.absorb(&fixture.at(&live, true, at[2]))
    };
    let spotlight = CauseId::Activity(ActivityId::Spotlight);
    for at in [[0, 4_000, 5_000], [0, 3_700, 6_200]] {
        let snap = read(spotlight_1032(), at);
        let why = snap.why.as_deref().expect("the scene's split");
        assert_eq!(why.split.via, Via::Coalitions, "{at:?}");
        let slack = if at == [0, 4_000, 5_000] { 0 } else { 5 };
        assert!(
            why.split.global_mc.abs_diff(3_300) <= slack,
            "{at:?}: {}",
            why.split.global_mc
        );
        let ids: Vec<CauseId> = why.causes.iter().map(|c| c.id).collect();
        assert_eq!(
            ids,
            [
                spotlight,
                CauseId::Context(aterm_vitals::explain::Context::Startup)
            ],
            "{at:?}"
        );
        assert_eq!(
            snap.mem.as_ref().map(|m| m.sample.total),
            Some(8 << 30),
            "the machine's"
        );
        assert_eq!(
            why.split.tabs,
            [(1, 200)],
            "{at:?}: no live tab, the scene's"
        );
    }
    let in_tab = Sample {
        tabs: vec![aterm_vitals::TabRoot {
            key: 7,
            tab: 7,
            shell_pid: 99_999,
            label: "Tab 1".into(),
            program: String::new(),
        }],
        ..live.clone()
    };
    let fixture = WhyFixture {
        scene: spotlight_1032(),
        t0: None,
    };
    let mut vitals = aterm_vitals::Vitals::new();
    let snap = [(0, true), (4_000, false), (5_000, true)]
        .map(|(at, split)| vitals.absorb(&fixture.at(&in_tab, split, at)));
    let why = snap[2].why.as_deref().expect("the scene's split");
    assert_eq!(why.split.tabs, [(7, 200)], "the live tab reads the scene's");
    let snap = read(calm_1510(), [0, 4_000, 5_000]);
    let why = snap.why.as_deref().expect("the calm scene's split");
    assert!(
        why.causes.iter().all(|c| !c.id.covering()),
        "{:?}",
        why.causes
    );
    assert_eq!(
        snap.mem.as_ref().map(|m| m.sample.total),
        Some(128 << 30),
        "the scene's"
    );
}

/// THE READER READS ITS SCENE ON ITS OWN SPLIT TICKS (the seam's wiring): a
/// scene installed on a reader whose machine has no lane reads under the
/// scene's lane; the bars' first reading is a split tick, the next, at once
/// after the last split tick (however long the first reading took), is none,
/// and one five seconds on (on the host's clock and the scene's) names
/// Spotlight over the scene's split. NEGATIVE CONTROL: the machine's
/// coalitions are never read.
#[cfg(debug_assertions)]
#[test]
fn the_reader_reads_its_scene_on_its_own_split_ticks() {
    let five = Duration::from_secs(5);
    let armed = gauge();
    let mut reader = Reader::new(Arc::default());
    reader.split = SplitReader::new(
        CoalProbe {
            self_check: |_| Err("unsupported"),
            ..STAND_IN
        },
        armed,
    );
    assert_eq!(reader.split.lane, CoalLane::Unsupported);
    reader.read_scene(Some(WhyFixture {
        scene: aterm_vitals::explain::fixtures::spotlight_1032(),
        t0: None,
    }));
    assert_eq!(reader.split.lane, CoalLane::On, "the scene's lane");
    READS.take();
    let _ = reader.read(&bars());
    assert!(reader.split.last.is_some(), "the first is a split tick");
    just_read(&mut reader);
    let first = reader.split.last.expect("just read");
    let _ = reader.read(&bars());
    assert_eq!(reader.split.last, Some(first), "the next, at once, is none");
    let fixture = reader.fixture.as_mut().expect("the scene");
    fixture.t0 = fixture.t0.and_then(|t| t.checked_sub(five));
    reader.split.last = first.checked_sub(five);
    let snap = reader.read(&bars());
    assert!(reader.split.last > Some(first), "five seconds on, one");
    let why = snap.why.as_deref().expect("the scene's split");
    assert_eq!(why.split.via, Via::Coalitions);
    assert_eq!(
        why.causes.first().map(|c| c.id),
        Some(CauseId::Activity(ActivityId::Spotlight))
    );
    let r = READS.take();
    assert_eq!((r.coalitions, r.swept), (0, 0), "the machine's");
}

/// This process's name as the sweep reads it, which the stand-in kernel's
/// record must name for its birth to be taken.
fn own_swept_name() -> String {
    let me = std::process::id();
    aterm_sysprobe::processes()
        .and_then(|s| s.procs.into_iter().find(|p| p.pid == me))
        .map(|p| p.name)
        .expect("this process swept")
}

/// This process's row and program in a whole reading.
#[cfg(target_os = "macos")]
fn own_birth(snap: &Snapshot) -> (Option<i64>, Option<i64>) {
    let me = std::process::id();
    let row = snap.procs.iter().find(|p| p.pid == me).expect("own row");
    let program = snap
        .programs
        .iter()
        .find(|g| g.scope == aterm_vitals::model::ProcScope::Aterm && g.name == row.name)
        .expect("own program");
    (row.birth_ms, program.born_ms)
}

/// BIRTHS RIDE EVERY WHOLE READING, split tick or not, so a program's age is
/// there between split ticks; a jobs-only edge's reading too, since `sys`
/// answers a pull from whichever whole reading is fresh, and a deck that no
/// window shows still leaves `sys procs` its ages. NEGATIVE CONTROL: a hidden
/// deck's machine-only reading sweeps nothing and reads no birth. (macOS: the
/// stand-in kernel answers only where the sweep gives no birth of its own.)
#[cfg(target_os = "macos")]
#[test]
fn births_ride_every_whole_reading() {
    const BORN: i64 = 1_790_000_000_000;
    let (mut reader, _) = stand_in_reader();
    kernel(&[(std::process::id(), &own_swept_name(), BORN, false)]);
    just_read(&mut reader);
    READS.take();
    let _ = reader.read_machine(&Request {
        machine_only: true,
        ..bars()
    });
    let r = READS.take();
    assert_eq!(
        (r.births, r.tables, r.coalitions),
        (0, 0, 0),
        "NEGATIVE CONTROL: a machine-only reading"
    );
    let unseen = Request {
        deck: false,
        ..bars()
    };
    let _ = reader.read(&unseen);
    READS.take();
    for (req, why) in [
        (
            Request {
                jobs_now: true,
                ..unseen.clone()
            },
            "a jobs-only edge, no window looking",
        ),
        (bars(), "the bars, between split ticks"),
        (
            Request {
                pull: true,
                ..unseen.clone()
            },
            "a pull, unarmed",
        ),
    ] {
        let snap = reader.read(&req);
        let r = READS.take();
        assert_eq!(r.coalitions, 0, "{why}: not a split tick");
        assert_eq!(own_birth(&snap), (Some(BORN), Some(BORN)), "{why}");
    }
}

/// A birth the sweep gave (Linux's boot clock, Windows' creation time)
/// reaches the model's sample as given.
#[test]
fn a_swept_birth_reaches_the_model() {
    let epoch = Instant::now();
    let read = |birth_ms| aterm_sysprobe::ProcessReading {
        pid: 9,
        birth_ms,
        ..aterm_sysprobe::ProcessReading::default()
    };
    assert_eq!(proc_sample(&read(Some(5_000)), epoch).birth_ms, Some(5_000));
    assert_eq!(proc_sample(&read(None), epoch).birth_ms, None);
}

/// A birth is read once and kept under its name and start; one the sweep
/// gave (Linux, Windows) is taken as given. NEGATIVE CONTROL: a new process
/// at the pid (another start) is read again.
#[test]
fn a_kept_birth_is_carried_without_a_read() {
    let now = Instant::now();
    kernel(&[(900, "zsh", 3_000, false), (901, "rustc", 4_000, false)]);
    let mut split = SplitReader::new(STAND_IN, gauge());
    let read = |split: &mut SplitReader, procs: &[ProcSample]| {
        let mut s = Sample {
            procs: Some(procs.to_vec()),
            ..Sample::default()
        };
        split.births(&mut s, now);
        (births(&s), READS.take().births)
    };
    let given = ProcSample {
        birth_ms: Some(5_000),
        ..swept(901, 1, "rustc", 31)
    };
    let mut procs = vec![swept(900, 1342, "zsh", 30), given];
    READS.take();
    assert_eq!(
        read(&mut split, &procs),
        (vec![Some(3_000), Some(5_000)], 1)
    );
    assert_eq!(
        read(&mut split, &procs),
        (vec![Some(3_000), Some(5_000)], 0)
    );
    kernel(&[(900, "zsh", 3_500, false)]);
    procs[0].start = Some(35);
    assert_eq!(
        read(&mut split, &procs),
        (vec![Some(3_500), Some(5_000)], 1)
    );
}

/// Births keep their own gap clock: read between split ticks, they stay
/// across a split gap that sends the leads' identities to be read again.
#[test]
fn births_read_between_split_ticks_outlast_the_split_gap() {
    let t0 = Instant::now();
    kernel(&[(1, "launchd", 1_000, true), (387, "mds", 2_000, true)]);
    let mut split = SplitReader::new(STAND_IN, gauge());
    let procs = vec![swept(387, 307, "mds", 20), swept(1, 1, "launchd", 10)];
    let _ = tick_at(&mut split, procs.clone(), t0);
    READS.take();
    let half = SPLIT_WINDOW_MAX / 2 + Duration::from_secs(1);
    let mut between = Sample {
        procs: Some(procs.clone()),
        ..Sample::default()
    };
    split.births(&mut between, t0 + half);
    let s = tick_at(&mut split, procs, t0 + half * 2);
    assert_eq!(births(&s), [Some(2_000), Some(1_000)]);
    let r = READS.take();
    assert_eq!(
        (r.births, r.identities),
        (0, 2),
        "the births were read inside their window; the leads' were not"
    );
}

/// A Mac whose coalitions lane is off still reads births: they need no
/// coalition.
#[cfg(target_os = "macos")]
#[test]
fn births_do_not_need_the_coalitions_lane() {
    const BORN: i64 = 1_790_000_000_000;
    let mut reader = Reader::new(Arc::default());
    reader.split = SplitReader::new(
        CoalProbe {
            self_check: |_| Err("missed"),
            ..STAND_IN
        },
        gauge(),
    );
    assert_eq!(reader.split.lane, CoalLane::Off);
    kernel(&[(std::process::id(), &own_swept_name(), BORN, false)]);
    READS.take();
    let _ = reader.read(&bars());
    let snap = reader.read(&bars());
    assert_eq!(own_birth(&snap), (Some(BORN), Some(BORN)));
    assert_eq!(READS.take().coalitions, 0, "no split tick");
}

/// Live (macOS): a whole reading that is no split tick carries this
/// process's birth as its BSD info gives it, through the kernel's readers.
#[cfg(target_os = "macos")]
#[test]
fn the_kernels_birth_reaches_a_reading_between_split_ticks() {
    let mut reader = Reader::new(Arc::default());
    reader.split = SplitReader::new(CoalProbe::KERNEL, gauge());
    just_read(&mut reader);
    let _ = reader.read(&bars());
    let snap = reader.read(&bars());
    assert!(reader.split.coalition_ids.is_empty(), "no split tick");
    let bsd =
        aterm_uds::process::read_process_birth(i32::try_from(std::process::id()).expect("a pid"))
            .expect("its BSD info");
    let ms = i64::try_from(bsd.seconds * 1000 + bsd.microseconds / 1000).expect("ms");
    assert_eq!(own_birth(&snap), (Some(ms), Some(ms)));
}
