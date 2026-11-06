// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `await sys` on [`Shelf::new`] with scripted volume readers.

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const GIB: u64 = 1 << 30;

/// Absolute `/`-rooted path `p` (prefixed with `C:` on Windows).
fn abs(p: &str) -> String {
    if cfg!(windows) {
        format!("C:{p}")
    } else {
        p.to_owned()
    }
}

fn key() -> VolumeKey {
    VolumeKey::Container("disk3".to_owned())
}

fn reading(free: u64) -> Reading {
    Reading {
        mount: PathBuf::from("/System/Volumes/Data"),
        key: key(),
        total: 2000 * GIB,
        free,
        files_free: Some(1 << 30),
        dev: 1,
        read_only: false,
    }
}

/// The home volume's row at `free` bytes (default lines, room 100 GiB).
fn home_row(free: u64) -> VolumeRow {
    let thresholds = aterm_agent::harness::disk::Config::default().thresholds(Some(2000 * GIB));
    VolumeRow {
        level: thresholds.level(Some(free), Some(1 << 30)),
        thresholds,
        room: Some(100 * GIB),
        reading: reading(free),
        as_of: Instant::now(),
        as_of_unix: 75_600,
        reclaimed_here: true,
    }
}

/// Board holding the home row at `free`, with session 7 mapped and the watch running.
fn shelf_at(free: u64) -> Shelf {
    let shelf = Shelf::new();
    shelf.publish(|b| {
        b.volumes = vec![home_row(free)];
        b.sessions.insert(7, 0);
        b.watch = Watch::Running {
            beat: Instant::now(),
        };
        b.watched = true;
    });
    shelf
}

/// The owner's lines on a volume the watch has not measured: the defaults.
fn default_lines(r: &Reading) -> aterm_agent::harness::disk::Thresholds {
    aterm_agent::harness::disk::Config::default().thresholds(Some(r.total))
}

fn owner() -> Who {
    Who {
        session: 7,
        cwd: Some(PathBuf::from("/Users//x/repo")),
        owner_class: true,
    }
}

/// Scripted world returning `free` bytes and counting pokes.
struct World {
    free: AtomicU64,
    pokes: AtomicU32,
}

impl World {
    fn new(free: u64) -> Arc<World> {
        Arc::new(World {
            free: AtomicU64::new(free),
            pokes: AtomicU32::new(0),
        })
    }
}

/// `text` through `await sys`'s one grammar to the wait this host runs.
fn parse(text: &str) -> Result<SysWait, String> {
    match aterm_vitals::sys::predicate::AwaitSys::parse(text)? {
        aterm_vitals::sys::predicate::Parsed::Volume(v) => wait_of(v).map_err(str::to_owned),
        other => panic!("{text} is no volume wait: {other:?}"),
    }
}

fn run_in(shelf: &Shelf, wait: &str, who: &Who, world: &World) -> String {
    let w = parse(wait).expect("parses");
    let read = |_: &Path| Ok(reading(world.free.load(Ordering::SeqCst)));
    let poke = |_: u64| {
        world.pokes.fetch_add(1, Ordering::SeqCst);
    };
    let settings = || (false, false, false);
    let env = Env {
        read: &read,
        poke: &poke,
        settings: &settings,
        lines: &default_lines,
        hung_up: &|| false,
        exited: &|| false,
    };
    run(shelf, &w, who, &env)
}

#[test]
fn the_grammar_hands_over_terms_levels_and_an_absolute_volume() {
    let w = parse("sys free>=5GiB").expect("a size");
    assert_eq!(
        w.terms,
        [Term::Free {
            strict: false,
            need: Need::Bytes(5 * GIB)
        }]
    );
    assert_eq!(w.timeout_ms, aterm_control::wire::WAIT_DEFAULT_MS);
    assert_eq!(w.vol, None);
    let w = parse(&format!(
        "sys free>room and disk=ok,low vol={} timeout=600000",
        abs("/Volumes/x")
    ))
    .expect("all");
    assert_eq!(
        w.terms,
        [
            Term::Free {
                strict: true,
                need: Need::Room
            },
            Term::Disk(vec![Level::Ok, Level::Low]),
        ]
    );
    assert_eq!(w.vol, Some(PathBuf::from(abs("/Volumes/x"))));
    assert_eq!(w.timeout_ms, 600_000);
    let spaced = abs("/Volumes/a b");
    let w = parse(&format!(
        "sys disk=emergency,short vol={}",
        aterm_control::wire::pct_encode(&spaced)
    ))
    .expect("an encoded volume");
    assert_eq!(w.terms, [Term::Disk(vec![Level::Emergency, Level::Short])]);
    assert_eq!(w.vol, Some(PathBuf::from(spaced)));
    assert_eq!(parse("sys free>=1GB vol=rel").err().as_deref(), Some(USAGE));
    assert_eq!(parse("sys free>=1GB vol=").err().as_deref(), Some(USAGE));
}

#[test]
fn low_free_boundaries_do_not_overflow_or_acquire_admission() {
    for threshold in [0, 68 * GIB, u64::MAX] {
        for free in [
            threshold.saturating_sub(1),
            threshold,
            threshold.saturating_add(1),
        ] {
            for (operator, expected) in [("<", free < threshold), ("<=", free <= threshold)] {
                let wait = parse(&format!("sys free{operator}{threshold}B")).unwrap();
                let row = home_row(free);
                assert_eq!(
                    holds(&wait, &row),
                    expected,
                    "{free} {operator} {threshold}"
                );
                assert_eq!(largest_need(&wait, &row), None);
            }
        }
    }
    // Defense beyond parsing: a directly constructed mixed wait is still an
    // observation and cannot acquire a cleanup demand.
    let mut mixed = parse("sys free<68GiB").unwrap();
    mixed.terms.push(Term::Free {
        strict: false,
        need: Need::Room,
    });
    assert_eq!(largest_need(&mixed, &home_row(0)), None);
}

#[test]
fn low_free_reads_fresh_at_arm_without_watch_settings_or_cleanup() {
    let shelf = shelf_at(5 * GIB);
    shelf.publish(|b| {
        b.watched = false;
        b.volumes[0].room = None;
    });
    let reads = AtomicU32::new(0);
    let read = |_: &Path| {
        reads.fetch_add(1, Ordering::SeqCst);
        Ok(reading(80 * GIB))
    };
    let env = Env {
        read: &read,
        poke: &|_| panic!("observer requested cleanup"),
        settings: &|| panic!("observer read cleanup settings"),
        lines: &default_lines,
        hung_up: &|| false,
        exited: &|| false,
    };
    let wait = parse("sys free<68GiB timeout=0").unwrap();
    assert_eq!(run(&shelf, &wait, &owner(), &env), "OK timeout\n");
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "fresh cached low space is not authority"
    );
    assert!(shelf.board_lock().demands.is_empty());

    let read = |_: &Path| Ok(reading(67 * GIB));
    let env = Env { read: &read, ..env };
    let reply = run(&shelf, &wait, &owner(), &env);
    assert!(
        reply.starts_with("OK sys free_bytes=71940702208 "),
        "{reply}"
    );
    assert!(reply.contains("freed_bytes=0 "), "{reply}");
    assert!(shelf.board_lock().demands.is_empty());
}

#[test]
fn low_free_arms_on_the_requested_volume_instead_of_the_low_home_cache() {
    let shelf = shelf_at(5 * GIB);
    let path = PathBuf::from(abs("/Volumes/guard/repo"));
    let read = |requested: &Path| {
        assert_eq!(requested, path);
        let mut actual = reading(80 * GIB);
        actual.mount = PathBuf::from(abs("/Volumes/guard"));
        actual.key = VolumeKey::Container("guard".into());
        Ok(actual)
    };
    let env = Env {
        read: &read,
        poke: &|_| panic!("observer requested cleanup"),
        settings: &|| panic!("observer read cleanup settings"),
        lines: &default_lines,
        hung_up: &|| false,
        exited: &|| false,
    };
    let wait = parse(&format!("sys free<68GiB vol={} timeout=0", path.display())).unwrap();
    assert_eq!(run(&shelf, &wait, &owner(), &env), "OK timeout\n");
    assert!(
        shelf
            .board_lock()
            .row_keyed(&VolumeKey::Container("guard".into()))
            .is_some()
    );
    assert!(shelf.board_lock().demands.is_empty());
}

#[test]
fn low_free_park_refreshes_stale_hits_and_refuses_volume_replacement() {
    for replacement in [false, true] {
        let shelf = shelf_at(5 * GIB);
        shelf.publish(|b| b.volumes[0].as_of = Instant::now() - FRESH - Duration::from_secs(1));
        let path = PathBuf::from(abs("/Volumes/requested/repo"));
        let read = |requested: &Path| {
            assert_eq!(requested, path);
            let mut value = reading(80 * GIB);
            if replacement {
                value.key = VolumeKey::Container("replacement".into());
            }
            Ok(value)
        };
        let env = Env {
            read: &read,
            poke: &|_| panic!("observer requested cleanup"),
            settings: &|| panic!("observer read cleanup settings"),
            lines: &default_lines,
            hung_up: &|| false,
            exited: &|| false,
        };
        let wait = parse("sys free<=68GiB").unwrap();
        let armed = Instant::now();
        let reply = park(
            &shelf,
            &wait,
            &key(),
            &path,
            (None, None, None),
            armed,
            armed,
            &env,
        );
        assert_eq!(
            reply,
            if replacement {
                "ERR unavailable free why=unreadable\n"
            } else {
                "OK timeout\n"
            }
        );
        assert!(shelf.board_lock().demands.is_empty());
    }
}

#[test]
fn a_condition_already_true_answers_at_once() {
    let shelf = shelf_at(500 * GIB);
    let world = World::new(500 * GIB);
    let reply = run_in(&shelf, "sys free>=room timeout=600000", &owner(), &world);
    assert!(
        reply.starts_with("OK sys free_bytes=536870912000 disk=ok "),
        "{reply}"
    );
    assert!(reply.contains(" vol=/System/Volumes/Data "), "{reply}");
    assert!(reply.ends_with(" held_ms=0 as_of=21:00:00\n"), "{reply}");
    assert_eq!(world.pokes.load(Ordering::SeqCst), 0);
    assert!(shelf.board_lock().demands.is_empty());
    let reply = run_in(&shelf, "sys disk=low,ok", &owner(), &world);
    assert!(reply.starts_with("OK sys "), "{reply}");
}

#[test]
fn arm_time_refusals_register_no_demand() {
    let shelf = shelf_at(20 * GIB);
    let world = World::new(20 * GIB);
    let reply = run_in(&shelf, "sys free>=900GiB", &owner(), &world);
    assert_eq!(
        reply,
        format!(
            "ERR unavailable free why=too-large cap_bytes={}\n",
            20 * GIB + 2000 * GIB / 100 * 25
        )
    );
    shelf.publish(|b| b.volumes[0].reclaimed_here = false);
    let reply = run_in(&shelf, "sys free>=30GiB", &owner(), &world);
    assert!(reply.starts_with("ERR unmet free_bytes="), "{reply}");
    assert!(reply.contains(" why=not-reclaimed "), "{reply}");
    assert!(
        !reply.contains("/Users/"),
        "no path of another tab: {reply}"
    );
    shelf.publish(|b| b.volumes[0].reclaimed_here = true);
    let w = parse("sys free>=30GiB").expect("parses");
    let read = |_: &Path| Ok(reading(20 * GIB));
    let off = || (true, false, false);
    let env = Env {
        read: &read,
        poke: &|_| {},
        settings: &off,
        lines: &default_lines,
        hung_up: &|| false,
        exited: &|| false,
    };
    let reply = run(&shelf, &w, &owner(), &env);
    assert!(reply.contains(" why=cleanup-off "), "{reply}");
    assert!(shelf.board_lock().demands.is_empty());
    assert_eq!(world.pokes.load(Ordering::SeqCst), 0);
}

#[test]
fn a_demand_is_registered_poked_and_met_by_a_pass() {
    let shelf = Arc::new(shelf_at(20 * GIB));
    let world = World::new(20 * GIB);
    let edge = Who {
        owner_class: false,
        ..owner()
    };
    let reply = run_in(&shelf, "sys free>=30GiB timeout=50", &edge, &world);
    assert_eq!(reply, "OK timeout\n");
    assert_eq!(
        world.pokes.load(Ordering::SeqCst),
        0,
        "no demand from an edge"
    );

    let watch = {
        let shelf = Arc::clone(&shelf);
        std::thread::spawn(move || {
            let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            while shelf.board_lock().demands.is_empty() {
                assert!(Instant::now() < deadline, "the demand never landed");
                aterm_uds::deadline::poll_yield();
            }
            assert_eq!(shelf.board_lock().demands[0].need, 30 * GIB);
            shelf.publish(|b| {
                b.volumes[0] = home_row(80 * GIB);
                b.last_pass = Some(PassSummary {
                    key: key(),
                    ended_at: Instant::now(),
                    free_after: Some(80 * GIB),
                    tiers: vec![1, 2],
                    freed: 60 * GIB,
                    demand: Some(30 * GIB),
                    ran_out: false,
                    spent: false,
                    held_back: false,
                    hogs: 0,
                    largest: 0,
                    left: Vec::new(),
                    ended_at_unix: None,
                });
            });
        })
    };
    let reply = run_in(&shelf, "sys free>=30GiB timeout=600000", &owner(), &world);
    watch.join().expect("the stand-in watch");
    assert!(
        reply.starts_with("OK sys free_bytes=85899345920 "),
        "{reply}"
    );
    assert!(
        reply.contains(&format!(" freed_bytes={} ", 60 * GIB)),
        "{reply}"
    );
    assert!(world.pokes.load(Ordering::SeqCst) >= 1);
    assert!(
        shelf.board_lock().demands.is_empty(),
        "the demand left the board"
    );
}

#[test]
fn a_spent_pass_answers_unmet_with_counts_not_paths() {
    let shelf = Arc::new(shelf_at(20 * GIB));
    let world = World::new(20 * GIB);
    let spent = PassSummary {
        key: key(),
        ended_at: Instant::now(),
        free_after: Some(25 * GIB),
        tiers: vec![1, 2, 3],
        freed: 5 * GIB,
        demand: Some(900 * GIB / 2),
        ran_out: true,
        spent: true,
        held_back: false,
        hogs: 5,
        largest: 100 * GIB,
        left: Vec::new(),
        ended_at_unix: None,
    };
    let watch = {
        let shelf = Arc::clone(&shelf);
        let spent = spent.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            while shelf.board_lock().demands.is_empty() {
                assert!(Instant::now() < deadline, "the demand never landed");
                aterm_uds::deadline::poll_yield();
            }
            shelf.publish(|b| {
                b.volumes[0] = home_row(25 * GIB);
                b.last_pass = Some(PassSummary {
                    ended_at: Instant::now(),
                    ..spent
                });
            });
        })
    };
    let reply = run_in(&shelf, "sys free>=450GiB timeout=600000", &owner(), &world);
    watch.join().expect("the stand-in watch");
    assert_eq!(
        reply,
        format!(
            "ERR unmet free_bytes={} need_bytes={} vol=/System/Volumes/Data freed_bytes={} \
             why=spent hogs=5 largest_bytes={}\n",
            25 * GIB,
            450 * GIB,
            5 * GIB,
            100 * GIB
        )
    );
    world.free.store(25 * GIB, Ordering::SeqCst);
    let again = run_in(&shelf, "sys free>=450GiB timeout=600000", &owner(), &world);
    assert!(again.contains(" why=spent "), "at arm time: {again}");
}

#[test]
fn a_stalled_watch_is_reported_only_while_it_runs() {
    let shelf = shelf_at(20 * GIB);
    let world = World::new(20 * GIB);
    let Some(old) = Instant::now().checked_sub(STALL + Duration::from_secs(1)) else {
        aterm_spec::host::could_not_run("the monotonic clock here cannot reach back a stall");
    };
    shelf.publish(|b| {
        b.watch = Watch::Running { beat: old };
        b.look_in_flight = true;
    });
    let reply = run_in(&shelf, "sys free>=30GiB timeout=600000", &owner(), &world);
    assert_eq!(reply, "ERR unavailable free why=watch-stalled\n");
    shelf.publish(|b| b.watch = Watch::Suspended);
    let reply = run_in(&shelf, "sys free>=30GiB timeout=50", &owner(), &world);
    assert_eq!(reply, "OK timeout\n");
    shelf.publish(|b| b.watch = Watch::Running { beat: old });
    let reply = run_in(&shelf, "sys disk=ok timeout=50", &owner(), &world);
    assert_eq!(reply, "OK timeout\n");
}

/// A STALL IS A HUNG LOOK, OR A LOOK THAT NEVER BEGINS ONCE THIS WAIT HAS POKED THAT LONG.
/// NEGATIVE CONTROL: each is one fact short of it.
#[test]
fn a_watch_stalls_on_a_hung_look_or_on_one_that_never_begins() {
    let young = STALL - Duration::from_secs(1);
    assert!(stalled(true, STALL, Duration::ZERO), "a hung look");
    assert!(stalled(false, STALL, STALL), "no look began");
    assert!(!stalled(true, young, STALL), "a look with a young beat");
    assert!(!stalled(false, STALL, young), "this wait's poke may yet");
    assert!(!stalled(false, young, STALL), "a look ended just now");
}

/// A stall ago, on the monotonic clock.
fn a_stall_ago() -> Instant {
    let Some(old) = Instant::now().checked_sub(STALL + Duration::from_secs(1)) else {
        aterm_spec::host::could_not_run("the monotonic clock here cannot reach back a stall");
    };
    old
}

/// A pass that ended just now on this volume, for `demand`, with more it could have freed.
fn pass_for(demand: Option<u64>) -> PassSummary {
    PassSummary {
        key: key(),
        ended_at: Instant::now(),
        free_after: Some(20 * GIB),
        tiers: vec![1],
        freed: 0,
        demand,
        ran_out: false,
        spent: false,
        held_back: false,
        hogs: 0,
        largest: 0,
        left: Vec::new(),
        ended_at_unix: None,
    }
}

/// What a wait armed at `armed` answers within `patience`, its demand first poked for at `poked`.
fn parked_since(shelf: &Shelf, armed: Instant, poked: Instant, patience: Duration) -> String {
    let w = parse("sys free>=30GiB timeout=50").expect("parses");
    let read = |_: &Path| Ok(reading(20 * GIB));
    let env = Env {
        read: &read,
        poke: &|_| {},
        settings: &|| (false, false, false),
        lines: &default_lines,
        hung_up: &|| false,
        exited: &|| false,
    };
    park(
        shelf,
        &w,
        &key(),
        Path::new("/"),
        (Some(30 * GIB), Some(Demanded { id: 1, poked }), None),
        armed,
        Instant::now() + patience,
        &env,
    )
}

/// A LOOK THAT NEVER BEGINS REACHES THE WAIT THAT HAS POKED FOR IT THAT LONG: the thread for it could
/// not be started, say, so nothing will free space, and only the time of this wait's first poke says
/// so.
#[test]
fn a_wait_that_poked_as_long_ago_as_the_beat_answers_stalled_with_no_look_in_flight() {
    let shelf = shelf_at(20 * GIB);
    let old = a_stall_ago();
    shelf.publish(|b| b.watch = Watch::Running { beat: old });
    assert_eq!(
        parked_since(&shelf, old, old, Duration::from_secs(60)),
        "ERR unavailable free why=watch-stalled\n"
    );
}

/// POKING COUNTS FROM THE FIRST POKE, NOT THE ARM: a wait whose own read of the volume took two
/// minutes has not yet asked for a look when that read returns. NEGATIVE CONTROL: the test above.
#[test]
fn a_wait_that_only_just_poked_is_no_stall_however_long_ago_it_armed() {
    let shelf = shelf_at(20 * GIB);
    let old = a_stall_ago();
    shelf.publish(|b| b.watch = Watch::Running { beat: old });
    assert_eq!(
        parked_since(&shelf, old, Instant::now(), Duration::from_millis(50)),
        "OK timeout\n"
    );
}

/// A PASS THAT TOOK THIS WAIT'S DEMAND ENDS ITS POKES, SO IT CANNOT SPEAK FOR A LOOK THAT DOES NOT
/// BEGIN: looks then come from the host's tick alone, whose restart can put the next one past the
/// beat's two minutes with nothing wrong. NEGATIVE CONTROL: the same wait with a look in flight is
/// stalled.
#[test]
fn a_wait_whose_demand_a_pass_took_answers_stalled_only_for_a_look_in_flight() {
    let shelf = shelf_at(20 * GIB);
    let old = a_stall_ago();
    shelf.publish(|b| {
        b.watch = Watch::Running { beat: old };
        b.last_pass = Some(pass_for(Some(30 * GIB)));
    });
    assert_eq!(
        parked_since(&shelf, old, old, Duration::from_millis(50)),
        "OK timeout\n"
    );
    shelf.publish(|b| b.look_in_flight = true);
    assert_eq!(
        parked_since(&shelf, old, old, Duration::from_secs(60)),
        "ERR unavailable free why=watch-stalled\n"
    );
}

/// A PASS THAT DID NOT TAKE THE DEMAND LEAVES THE WAIT POKING, SO IT STILL SPEAKS FOR A LOOK THAT DOES
/// NOT BEGIN. NEGATIVE CONTROL: the test above, whose pass took it.
#[test]
fn a_wait_whose_demand_no_pass_took_answers_stalled_with_no_look_in_flight() {
    for pass_demand in [None, Some(10 * GIB)] {
        let shelf = shelf_at(20 * GIB);
        let old = a_stall_ago();
        shelf.publish(|b| {
            b.watch = Watch::Running { beat: old };
            b.last_pass = Some(pass_for(pass_demand));
        });
        assert_eq!(
            parked_since(&shelf, old, old, Duration::from_secs(60)),
            "ERR unavailable free why=watch-stalled\n",
            "{pass_demand:?}"
        );
    }
}

/// How many pokes a wait makes for 30 GiB, due a poke at once, after a pass for `pass_demand`, within
/// `patience`, with `watched` as it is before and after the wait's read of the volume. The row starts
/// old and a poke ages it again, so the wait reads rather than parks for ten seconds, and `exited`
/// ends it after that poke.
fn pokes_after(pass_demand: Option<u64>, watched: (bool, bool), patience: Duration) -> u32 {
    let long = a_stall_ago();
    let shelf = shelf_at(20 * GIB);
    shelf.publish(|b| {
        b.volumes[0].as_of = long;
        b.last_pass = Some(pass_for(pass_demand));
        b.watched = watched.0;
    });
    let pokes = AtomicU32::new(0);
    let w = parse("sys free>=30GiB timeout=50").expect("parses");
    let read = |_: &Path| {
        shelf.publish(|b| b.watched = watched.1);
        Ok(reading(20 * GIB))
    };
    let poke = |_: u64| {
        pokes.fetch_add(1, Ordering::SeqCst);
        shelf.publish(|b| b.volumes[0].as_of = long);
    };
    let exited = || pokes.load(Ordering::SeqCst) > 0;
    let env = Env {
        read: &read,
        poke: &poke,
        settings: &|| (false, false, false),
        lines: &default_lines,
        hung_up: &|| false,
        exited: &exited,
    };
    let demand = Demanded { id: 1, poked: long };
    park(
        &shelf,
        &w,
        &key(),
        Path::new("/"),
        (Some(30 * GIB), Some(demand), None),
        long,
        Instant::now() + patience,
        &env,
    );
    pokes.load(Ordering::SeqCst)
}

/// ONLY A PASS FOR THIS DEMAND ENDS ITS POKES: a pass for no need, or for a smaller one, took nothing
/// for it, and a poke that lost to that look is gone. NEGATIVE CONTROL: a pass for a need at least as
/// large.
#[test]
fn only_a_pass_for_the_demand_ends_its_pokes() {
    let (hang, brief) = (Duration::from_secs(60), Duration::from_millis(50));
    let on = (true, true);
    assert_eq!(pokes_after(None, on, hang), 1, "a pass for no need");
    assert_eq!(pokes_after(Some(10 * GIB), on, hang), 1, "for less");
    assert_eq!(pokes_after(Some(30 * GIB), on, brief), 0, "for this");
    assert_eq!(pokes_after(Some(40 * GIB), on, brief), 0, "for more");
}

/// WITH NOTHING WATCHED A WAIT DOES NOT POKE: no pass then adopts a demand, so its pokes would only
/// force a pass every ten seconds. It asks when it would poke, because a change of the flag wakes no
/// wait. NEGATIVE CONTROL: the same pass with the watch on, above, and one turned on as it waited.
#[test]
fn a_wait_with_nothing_watched_does_not_poke() {
    let (hang, brief) = (Duration::from_secs(60), Duration::from_millis(50));
    assert_eq!(pokes_after(None, (false, false), brief), 0, "off");
    assert_eq!(pokes_after(None, (true, false), brief), 0, "turned off");
    assert_eq!(pokes_after(None, (false, true), hang), 1, "turned on");
}

/// AN OLD BEAT WITH NO LOOK IN FLIGHT IS NO STALL TO A WAIT JUST ARMED: it says only that no look has
/// started, and this wait's poke starts one. NEGATIVE CONTROL: the same beat with a look in flight,
/// above.
#[test]
fn an_old_beat_with_no_look_in_flight_is_no_stall() {
    let shelf = shelf_at(20 * GIB);
    let world = World::new(20 * GIB);
    let Some(old) = Instant::now().checked_sub(STALL + Duration::from_secs(1)) else {
        aterm_spec::host::could_not_run("the monotonic clock here cannot reach back a stall");
    };
    shelf.publish(|b| b.watch = Watch::Running { beat: old });
    let reply = run_in(&shelf, "sys free>=30GiB timeout=50", &owner(), &world);
    assert_eq!(reply, "OK timeout\n");
}

#[test]
fn an_unreadable_volume_is_unavailable() {
    let shelf = Shelf::new();
    let w = parse("sys free>=1GiB").expect("parses");
    let read = |_: &Path| Err(Unread::Remote);
    let env = Env {
        read: &read,
        poke: &|_| {},
        settings: &|| (false, false, false),
        lines: &default_lines,
        hung_up: &|| false,
        exited: &|| false,
    };
    assert_eq!(
        run(&shelf, &w, &owner(), &env),
        "ERR unavailable free why=remote\n"
    );
}

#[test]
fn a_demand_with_no_watch_is_unavailable() {
    let shelf = shelf_at(20 * GIB);
    shelf.publish(|b| b.watched = false);
    let world = World::new(20 * GIB);
    let reply = run_in(&shelf, "sys free>=30GiB timeout=600000", &owner(), &world);
    assert_eq!(reply, "ERR unavailable free why=no-watch\n");
    assert!(shelf.board_lock().demands.is_empty());
    assert_eq!(world.pokes.load(Ordering::SeqCst), 0);
    let reply = run_in(&shelf, "sys disk=ok timeout=50", &owner(), &world);
    assert_eq!(reply, "OK timeout\n");
}

#[test]
fn a_room_that_is_not_known_is_unavailable() {
    let shelf = shelf_at(500 * GIB);
    shelf.publish(|b| b.volumes[0].room = None);
    let world = World::new(500 * GIB);
    let reply = run_in(&shelf, "sys free>=room timeout=600000", &owner(), &world);
    assert_eq!(reply, "ERR unavailable free why=room-unknown\n");
    let reply = run_in(&shelf, "sys free>=1GiB timeout=600000", &owner(), &world);
    assert!(reply.starts_with("OK sys "), "{reply}");
}

#[test]
fn a_wait_whose_row_a_look_dropped_reads_the_volume_itself() {
    let data = abs("/System/Volumes/Data");
    let wait = parse(&format!("sys free>=10GiB vol={data} timeout=600000")).expect("parses");
    let at = Path::new(&data);
    let shelf = Shelf::new();
    let read = |_: &Path| Ok(reading(500 * GIB));
    let env = Env {
        read: &read,
        poke: &|_| {},
        settings: &|| (false, false, false),
        lines: &default_lines,
        hung_up: &|| false,
        exited: &|| false,
    };
    let armed = Instant::now();
    let deadline = armed + aterm_spec::wait::MUST_HAPPEN;
    let reply = park(
        &shelf,
        &wait,
        &key(),
        at,
        (Some(10 * GIB), None, None),
        armed,
        deadline,
        &env,
    );
    assert!(reply.starts_with("OK sys free_bytes="), "{reply}");
    let row = shelf
        .board_lock()
        .row_keyed(&key())
        .cloned()
        .expect("the wait's own row");
    assert_eq!(row.room, None);
    assert_eq!(row.thresholds, default_lines(&reading(500 * GIB)));

    let shelf = Shelf::new();
    let other = |_: &Path| {
        Ok(Reading {
            key: VolumeKey::Fsid(9),
            ..reading(500 * GIB)
        })
    };
    let env = Env {
        read: &other,
        ..env
    };
    let reply = park(
        &shelf,
        &wait,
        &key(),
        at,
        (Some(10 * GIB), None, None),
        armed,
        deadline,
        &env,
    );
    assert_eq!(reply, "ERR unavailable free why=unreadable\n");

    let stale = Instant::now() - (FRESH + Duration::from_secs(1));
    let shelf = shelf_at(20 * GIB);
    shelf.publish(|b| {
        b.volumes[0].as_of = stale;
        b.volumes[0].thresholds = aterm_agent::harness::disk::Config {
            warn_free_gib: 123,
            ..aterm_agent::harness::disk::Config::default()
        }
        .thresholds(Some(2000 * GIB));
    });
    let room = parse("sys free>=room timeout=600000").expect("parses");
    let reply = run(&shelf, &room, &owner(), &env);
    assert_eq!(reply, ROOM_UNKNOWN);
    let new = shelf
        .board_lock()
        .row_keyed(&VolumeKey::Fsid(9))
        .cloned()
        .expect("the replacement volume's row");
    assert_eq!(new.room, None);
    assert!(!new.reclaimed_here);
    assert_eq!(new.thresholds, default_lines(&new.reading));
    assert!(shelf.board_lock().demands.is_empty());

    let checks = std::cell::Cell::new(0);
    let hung_up = || {
        let n = checks.get() + 1;
        checks.set(n);
        n > 1
    };
    let changed = Env {
        hung_up: &hung_up,
        ..env
    };
    let shelf = shelf_at(GIB);
    shelf.publish(|b| b.volumes[0].as_of = stale);
    let reply = park(
        &shelf,
        &wait,
        &key(),
        at,
        (Some(10 * GIB), None, None),
        armed,
        deadline,
        &changed,
    );
    assert_eq!(reply, "ERR unavailable free why=unreadable\n");
    assert!(shelf.board_lock().row_keyed(&VolumeKey::Fsid(9)).is_none());

    let shelf = shelf_at(GIB);
    shelf.publish(|b| b.volumes[0].as_of = stale);
    checks.set(0);
    let same = Env {
        read: &read,
        ..changed
    };
    let reply = park(
        &shelf,
        &wait,
        &key(),
        at,
        (Some(10 * GIB), None, None),
        armed,
        deadline,
        &same,
    );
    assert!(reply.starts_with("OK sys free_bytes="), "{reply}");
}

#[test]
fn a_wait_spells_only_a_tab_the_board_places_nowhere() {
    let (short, moved, long) = if cfg!(windows) {
        (
            r"C:\Users\ANDREW~1\repo",
            r"C:\Users\ANDREW~1\other",
            r"C:\Users\Andrew Yates\repo",
        )
    } else {
        ("/tmp/REPO~1/x", "/tmp/REPO~1/y", "/tmp/a repo/x")
    };
    let (short, moved) = (Path::new(short), Path::new(moved));
    let shelf = shelf_at(500 * GIB);
    {
        let b = shelf.board_lock();
        assert!(b.row_of(7, short.to_str()).is_some(), "PREMISE: 7 mapped");
        assert!(!spell_owed(&b, 7, short), "a look mapped the tab");
        assert!(spell_owed(&b, 8, short), "no look mapped it");
    }
    shelf.publish(|b| b.spell(8, short.to_path_buf(), PathBuf::from(long)));
    let b = shelf.board_lock();
    assert!(b.row_of(8, short.to_str()).is_none(), "PREMISE: on no row");
    assert!(!spell_owed(&b, 8, short), "spelled already");
    assert!(spell_owed(&b, 8, moved), "the tab moved");
}

/// AN 8.3 SPELLING CARRIES ON ITS VOLUME'S ROW (live5 F1): a wait whose
/// `vol=` — or whose tab's directory, before any look mapped the tab — is
/// spelled with 8.3 short names gives no key on the board's table, and reads
/// the volume itself (the product's reader spells short names long). It
/// answers from the row the watch measured there: `free>=room` is met on
/// its room, a demand from that tab registers on its `reclaimed_here`, and
/// the row keeps both, so another tab's `free>=room` is met after it.
#[cfg(windows)]
#[test]
fn an_8_3_spelling_carries_on_its_volumes_row() {
    use aterm_sysprobe::volumes::{Mount, Table};
    let c = Mount {
        on: PathBuf::from(r"C:\"),
        from: r"\Device\HarddiskVolume3".to_owned(),
        fstype: String::new(),
        local: true,
        automounted: false,
        removable: false,
        read_only: false,
        container: Some("Volume{6140fc3b-044f-43d7-b060-cb98170c7a63}".to_owned()),
    };
    let key = c.key().expect("a volume key");
    let read_at = |free: u64| Reading {
        mount: PathBuf::from(r"C:\"),
        key: key.clone(),
        ..reading(free)
    };
    let shelf = shelf_at(500 * GIB);
    shelf.publish(|b| {
        b.table = Table {
            mounts: vec![c],
            complete: true,
            firmlinks: Vec::new(),
        };
        b.volumes[0].reading = read_at(500 * GIB);
        b.sessions.clear();
    });
    {
        let b = shelf.board_lock();
        assert!(
            b.row_at(Path::new(r"C:\Program Files\x")).is_some(),
            "PREMISE: long"
        );
        assert!(
            b.row_at(Path::new(r"C:\PROGRA~1\x")).is_none(),
            "PREMISE: 8.3"
        );
    }
    let pokes = AtomicU32::new(0);
    let read = |_: &Path| Ok(read_at(500 * GIB));
    let poke = |_: u64| {
        pokes.fetch_add(1, Ordering::SeqCst);
    };
    let env = Env {
        read: &read,
        poke: &poke,
        settings: &|| (false, false, false),
        lines: &default_lines,
        hung_up: &|| false,
        exited: &|| false,
    };
    let wait = |text: &str| parse(text).expect("parses");
    let reply = run(
        &shelf,
        &wait(r"sys free>=room vol=C:\PROGRA~1\not-yet-made timeout=600000"),
        &owner(),
        &env,
    );
    assert!(reply.starts_with("OK sys "), "8.3 vol=: {reply}");
    let tab = Who {
        session: 8,
        cwd: Some(PathBuf::from(r"C:\Users\ANDREW~1\repo")),
        owner_class: true,
    };
    let reply = run(&shelf, &wait("sys free>=room timeout=600000"), &tab, &env);
    assert!(reply.starts_with("OK sys "), "8.3 tab: {reply}");
    let reply = run(&shelf, &wait("sys free>=600GiB timeout=50"), &tab, &env);
    assert_eq!(reply, "OK timeout\n", "a demand, never not-reclaimed");
    assert!(
        pokes.load(Ordering::SeqCst) >= 1,
        "the demand poked the watch"
    );
    let held = shelf
        .board_lock()
        .row_keyed(&key)
        .cloned()
        .expect("the row");
    assert_eq!(held.room, Some(100 * GIB), "the watch's room");
    assert!(held.reclaimed_here, "the watch's reclaim");
    let reply = run(
        &shelf,
        &wait(r"sys free>=room vol=C:\work timeout=600000"),
        &owner(),
        &env,
    );
    assert!(reply.starts_with("OK sys "), "another tab: {reply}");
}
