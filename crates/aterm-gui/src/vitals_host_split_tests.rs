// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The split set's host side (`crate::vitals_host`, THE SPLIT SET): which
//! readings are split ticks, the cold pair a `sys why` pull takes, and what
//! is kept (known-start births and the timeline) or refreshed (lead identities), through a
//! stand-in kernel ([`STAND_IN`]) that counts its reads on the test's thread.
//! Each test's checks are of what was read, never of how long a read took.
//! RD is `docs/DESIGN-resource-details-2026-10-20.md`.

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
    births: usize,
    tables: usize,
    identities: usize,
    timelines: usize,
}

thread_local! {
    /// What the stand-in kernel holds on this thread: each pid's short name,
    /// birth (unix ms), and whether Apple signed it.
    static KERNEL: RefCell<HashMap<u32, (String, i64, bool)>> = RefCell::new(HashMap::new());
    /// Its `kern.waketime`.
    static WAKE: Cell<Option<i64>> = const { Cell::new(None) };
    /// A pid whose next identity read misses (gone, or an exec mid-read).
    static MISSES: Cell<Option<u32>> = const { Cell::new(None) };
    /// Its reads so far.
    static READS: Cell<Reads> = const {
        Cell::new(Reads {
            coalitions: 0,
            swept: 0,
            births: 0,
            tables: 0,
            identities: 0,
            timelines: 0,
        })
    };
}

/// Count one read.
fn count(read: impl FnOnce(&mut Reads)) {
    let mut reads = READS.get();
    read(&mut reads);
    READS.set(reads);
}

/// The stand-in kernel's readers: the self-check passes; the sweep is the
/// machine's, counted where it reads each process's coalition; the list holds
/// one coalition whose counters do not move; a birth (or the whole table), an
/// identity (Apple's, or a Developer ID; none once for [`MISSES`]) and the
/// timeline are the thread's [`KERNEL`] and [`WAKE`].
const STAND_IN: CoalProbe = CoalProbe {
    self_check: |_| Ok(1),
    sweep: |opts| {
        if opts.coalitions {
            count(|r| r.swept += 1);
        }
        aterm_sysprobe::processes_with(opts)
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

/// The stand-in kernel holds `rows`: pid, short name, birth, Apple's.
fn kernel(rows: &[(u32, &str, i64, bool)]) {
    KERNEL.with(|k| {
        *k.borrow_mut() = rows
            .iter()
            .map(|&(pid, name, ms, apple)| (pid, (name.to_owned(), ms, apple)))
            .collect();
    });
}

/// A gauge of its own for one test: arming it arms no other test's split.
fn gauge() -> &'static Arming {
    Box::leak(Box::new(Arming::new()))
}

/// A reader of this machine whose coalition lane is the stand-in's.
fn stand_in_reader() -> (Reader, &'static Arming) {
    let armed = gauge();
    let mut reader = Reader::new(Arc::default());
    reader.split = SplitReader::new(STAND_IN, armed);
    (reader, armed)
}

/// The bars' request at their focused cadence, a window in view drawing the
/// Deck.
fn bars() -> Request {
    Request {
        self_pid: std::process::id(),
        cadence_ms: 1_000,
        deck: true,
        ..Request::default()
    }
}

/// When the split tick before the next reading read: now, as the reading
/// just taken left it, however long that took.
fn just_read(reader: &mut Reader) {
    reader.split.last = Some(Instant::now());
}

/// THE SPLIT TICKS (RD §3.1): the bars' readings read the split set once
/// every five seconds at their 1 s cadence, once a second at a lens's 0.5 s,
/// and at each of the background's 5 s readings, a reading a few ms early
/// within half its cadence of the period; a pull only while a `sys why` arms
/// it, at least the shortest window after the last. NEGATIVE CONTROLS: the
/// engine's readings (an edge, the disk watch's look, its demand), a hidden
/// deck's machine counters, the Fleet Monitor tab's readings with no bars in
/// view and an unarmed pull (a plain `sys`, an `await sys` wait) read none
/// however long it has been, and an armed pull inside the shortest window
/// reads none.
#[test]
fn split_ticks_ride_the_bars_and_an_armed_pull_only() {
    let t0 = Instant::now();
    // Readings `period_ms` apart, every other one 3 ms late: how many are
    // split ticks.
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

/// THE READINGS THAT READ THE COALITIONS (RD §6.3, Host), through
/// a reader of this machine with the stand-in lane: the bars' first reading
/// reads the list and each process's coalition in its sweep, once, and the
/// next, taken at once, neither; a warm pull while a `sys why` arms the
/// split reads them once. NEGATIVE CONTROLS: a hidden deck's machine
/// counters, the jobs engine's reading, the Fleet Monitor tab's with no bars
/// in view and an unarmed pull read neither.
#[test]
fn the_coalitions_are_read_on_split_ticks_only() {
    let (mut reader, armed) = stand_in_reader();
    // The list's reads, and the sweeps that read each process's coalition.
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
    let _ = pull(&mut reader, &bars(), Some(Instant::now()));
    assert_eq!(reads(), (0, 0), "an unarmed pull");
    reader.read(&bars());
    assert_eq!(reads(), (1, 1), "the bars' first reading");
    just_read(&mut reader);
    reader.read(&bars());
    assert_eq!(reads(), (1, 1), "the bars' next, at once");
    {
        let _armed = armed.arm();
        assert!(!reader.split_cold(), "the last split tick is fresh");
        let _ = pull(&mut reader, &bars(), Some(Instant::now()));
    }
    assert_eq!(reads(), (2, 2), "a warm pull while armed");
}

/// THE LOOP ASKS FOR SPLIT TICKS ONLY WHILE A WINDOW IN VIEW SHOWS THE BARS
/// ([`Request::deck`]): the Deck, or the Sill (a close-up opens from either).
/// NEGATIVE CONTROLS: no bars drawn, the bars off, the Fleet Monitor tab
/// alone in view, and the bars shown on windows every one of which is
/// occluded ask none.
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

/// The split facts `snap` carries are its own reading's (none carried from
/// an earlier one), by coalition, over a window at least the cold pair's
/// spacing and at most the longest a window can be. Unix: the split counts
/// this user's processes, whose uid Windows does not read.
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

/// AN ARMED PULL WITH NO FRESH SPLIT TICK READS A PAIR, SO ITS REPLY HAS A
/// WINDOW: with none yet, the base and then the window's end, the cold
/// pair's spacing apart; with the last one (on the host's clock and the
/// model's) over a minute old while the last whole reading is fresh, the
/// same. NEGATIVE CONTROL: unarmed, that pull is warm and reads no
/// coalition.
#[test]
fn an_armed_pull_with_no_fresh_split_tick_reads_a_pair_and_has_a_window() {
    let (mut reader, armed) = stand_in_reader();
    let arming = armed.arm();
    READS.take();
    assert!(reader.split_cold(), "no split tick yet");
    let snap = pull(&mut reader, &bars(), None);
    assert_eq!(
        READS.take().coalitions,
        2,
        "the base, then the window's end"
    );
    assert_window(&snap);

    let gap = SPLIT_WINDOW_MAX + Duration::from_secs(1);
    let stale = || Instant::now().checked_sub(gap);
    // Every stamp from here on a minute and more past the model's base.
    reader.epoch = reader.epoch.checked_sub(gap).expect("a minute since boot");
    reader.split.last = stale();
    assert!(reader.split_cold(), "a base too old for a window");
    let snap = pull(&mut reader, &bars(), Some(Instant::now()));
    assert_eq!(READS.take().coalitions, 2, "cold for the split alone");
    assert_window(&snap);

    drop(arming);
    reader.split.last = stale();
    assert!(!reader.split_cold(), "no `sys why` arms the split");
    let _ = pull(&mut reader, &bars(), Some(Instant::now()));
    assert_eq!(READS.take().coalitions, 0);
}

/// A source whose pull is cold for the split (or not), noting when each of
/// its readings was taken.
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

/// A PULL COLD FOR THE SPLIT ALONE STILL KEEPS THE FLOOR: the last whole
/// reading just taken, its pair's first reading comes at least
/// [`MIN_WINDOW`] after it, so no rate the model takes is over less.
/// NEGATIVE CONTROL: warm for the split, the same pull reads once, after the
/// same wait.
#[test]
fn a_pull_cold_for_the_split_alone_keeps_the_floor() {
    for (cold, readings) in [(true, 2), (false, 1)] {
        let mut source = Paced {
            vitals: aterm_vitals::Vitals::new(),
            cold,
            taken: Vec::new(),
        };
        let last = Instant::now();
        let _ = pull(&mut source, &bars(), Some(last));
        assert_eq!(source.taken.len(), readings, "cold: {cold}");
        assert!(
            source.taken[0].saturating_duration_since(last) >= MIN_WINDOW,
            "cold: {cold}"
        );
    }
}

/// A PULL IS COLD FOR THE SPLIT A MARGIN INSIDE THE LONGEST WINDOW: armed,
/// with the last split tick a hair inside [`SPLIT_WINDOW_MAX`] when it looks
/// (past it by the time its reading's stamp is taken), it reads a pair.
/// NEGATIVE CONTROLS: the margin and more inside, it reads once; unarmed,
/// never twice for the split.
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

/// A KERNEL RECORD'S NAME IS THE SWEPT ONE WHOLE, OR CUT BY ONE AT 16 BYTES
/// (the short info holds 15), a letter the cut splits read as the sweep
/// decodes it. NEGATIVE CONTROL: a shorter swept name that begins a longer
/// record's is another process's.
#[test]
fn a_kernel_name_is_the_swept_one_whole_or_cut_by_one() {
    assert!(names_the_same("mds_stores", "mds_stores"));
    assert!(names_the_same("SetStoreUpdateSe", "SetStoreUpdateS"));
    // Eight two-byte letters: the sweep's 15 bytes end in half of one.
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
/// reads this process's, the one the self-check named; and its birth reads
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
    let sweep = (kernel.sweep)(aterm_sysprobe::SweepOpts { coalitions: true }).expect("a sweep");
    let row = sweep
        .procs
        .iter()
        .find(|p| p.pid == me)
        .expect("this process swept");
    assert_eq!(row.coalition, Some(coalition));
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
}

/// A long gap refreshes births even when the sweep's start stayed known.
/// Inside a window those births are kept, but identities are always current.
/// A missing start never vouches for reuse: same-name births are read again
/// at every tick, including a pid reused between consecutive sweeps.
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
    assert_eq!(sent(&s).len(), 2, "identities are refreshed");
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
    assert_eq!((r.births, r.identities), (2, 4));

    for p in &mut procs {
        p.start = None;
    }
    for born in [11_000, 12_000] {
        kernel(&[(1, "launchd", 1_000, true), (387, "mds", born, true)]);
        let s = tick(&mut split, procs.clone());
        assert_eq!(births(&s), [Some(born), Some(1_000)], "no start: {born}");
    }
    let r = READS.take();
    assert_eq!((r.births, r.identities), (4, 4));
}

/// A LEAD WHOSE IDENTITY READ MISSES (gone, or an exec mid-read) IS ASKED
/// AGAIN AT THE NEXT TICK, and sent then. Each miss, even after a successful
/// read, sends unknown so the model cannot keep a previous image or voucher.
#[test]
fn a_lead_whose_identity_misses_is_asked_again_at_the_next_tick() {
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
    MISSES.set(Some(387));
    let unknown = tick(&mut split, procs.clone());
    assert_eq!(sent(&unknown), [(387, 2_000, "", Voucher::Unknown)]);
    assert_eq!(unknown.leads[0].signing_id, None);
    assert_eq!(unknown.leads[0].team, None);
    kernel(&[(1, "launchd", 1_000, true), (387, "mds", 2_000, false)]);
    assert_eq!(
        sent(&tick(&mut split, procs)),
        [(387, 2_000, "mds", Voucher::DeveloperId)],
        "a current identity can be read again"
    );
    assert_eq!(READS.take().identities, 4);
}

/// A MISREAD TURNS THE HOST'S LANE OFF FOR GOOD: where the model finds the
/// coalitions' footprints past any machine's memory (a record whose layout
/// moved), the reading's lane is off and so is the reader's, and no reading
/// after reads the coalitions, armed or not. Unix: the misread is judged
/// against the machine's memory, which this reader reads there.
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
    let _ = pull(&mut reader, &bars(), Some(Instant::now()));
    assert_eq!(READS.take().coalitions, 0, "never again");
}

/// A SWEPT PROCESS'S COALITION REACHES THE MODEL ([`proc_sample`]), its
/// birth left to the split tick ([`SplitReader::fill`]). NEGATIVE CONTROL: a
/// sweep that read none gives none.
#[test]
fn a_swept_processs_coalition_reaches_the_model() {
    let epoch = Instant::now();
    let read = |coalition| aterm_sysprobe::ProcessReading {
        pid: 9,
        coalition,
        ..aterm_sysprobe::ProcessReading::default()
    };
    let p = proc_sample(&read(Some(7)), epoch);
    assert_eq!((p.coalition, p.birth_ms), (Some(7), None));
    assert_eq!(proc_sample(&read(None), epoch).coalition, None);
}

/// THE COALITIONS REACH THE MODEL FIELD FOR FIELD, a cut list's word
/// with them.
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

/// EACH KERNEL VOUCHER IS THE MODEL'S OF THE SAME NAME while the
/// code-signing reads vouch. NEGATIVE CONTROL: while they do not, each is
/// unknown.
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

/// One process of a sweep: user 501's, in `coalition`, named `name`, its
/// kernel start `start`.
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

/// A split tick of `procs` through `split`.
fn tick(split: &mut SplitReader, procs: Vec<ProcSample>) -> Sample {
    tick_at(split, procs, Instant::now())
}

/// A split tick at an injected time, so cache histories do not depend on scheduling.
fn tick_at(split: &mut SplitReader, procs: Vec<ProcSample>, now: Instant) -> Sample {
    let mut sample = Sample {
        procs: Some(procs),
        ..Sample::default()
    };
    let read = (split.probe.coalitions)().expect("the stand-in's list");
    split.fill(&mut sample, read, now);
    sample
}

/// Each lead sent, as pid, birth, image and voucher.
fn sent(s: &Sample) -> Vec<(u32, i64, &str, Voucher)> {
    s.leads
        .iter()
        .map(|l| (l.pid, l.birth_ms, l.exe.as_str(), l.voucher))
        .collect()
}

/// Each swept process's birth.
fn births(s: &Sample) -> Vec<Option<i64>> {
    s.procs.iter().flatten().map(|p| p.birth_ms).collect()
}

/// Known-start births are cached, and every selected lead's identity is
/// current at each split tick. An exec can keep the pid, birth and short
/// name while its signature changes. A changed name or start refreshes the
/// birth; a pid that leaves is forgotten. A kernel record with another
/// name than the sweep supplies no birth and is asked again next tick.
#[test]
fn known_start_births_are_kept_and_lead_identities_are_refreshed() {
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
    // Newest first, as the kernel lists them.
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
    assert_eq!((r.births, r.identities), (0, 3), "only births are cached");

    // An exec can retain the same short name and birth but change signature.
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
        ]
    );
    let r = READS.take();
    assert_eq!((r.births, r.identities), (0, 3));

    // An exec: the pid and the birth stay, the image moves.
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
    assert_eq!((r.births, r.identities), (1, 3));

    // A reused pid: another process, born later, leads 1342 now.
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
    assert_eq!((r.births, r.identities), (1, 3));

    // 387 gone: 388 leads 307; back, 387 is read and leads again.
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
    assert_eq!((r.births, r.identities), (1, 6));

    // The record read is another process's: no birth, read again next tick.
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

/// MANY BIRTHS TO READ ARE READ IN ONE READ OF THE TABLE: past
/// [`BIRTHS_AT_ONCE`] pids not yet read (the first tick, a build's storm),
/// one read of every record gives each its birth; the next tick reads
/// nothing. A 16-byte name the sweep holds cut to 15 is the same process's.
/// NEGATIVE CONTROL: at the threshold, each is read on its own.
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

/// NO VOUCHER WITHOUT THE CODE-SIGNING READS VOUCHING (RD §3.1's
/// self-check): with launchd not reading as Apple's, an Apple-signed lead's
/// voucher reads unknown, never Apple's, and the lane stays on. NEGATIVE
/// CONTROL: launchd Apple's, the same lead reads Apple's.
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

/// THE TIMELINE IS READ AGAIN ONLY WHEN THE MACHINE WOKE: three split ticks
/// read it once, each carrying it; a wake reads it again. Its last wake rides
/// each one.
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

/// THE LANE IS THE SELF-CHECK'S: passed, on; `unsupported` (Linux, Windows),
/// unsupported; any other miss, off, and then no reading is a split tick,
/// armed or not. The test reader's own lane is the inert arm: unsupported,
/// and its reading is no split tick.
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
        // The cores' ticks are whole ms: on the scene's own spacing, exact;
        // off it, a millicore or so of rounding.
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
    }
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
