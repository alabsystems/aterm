// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `await sys` on a board of the test's own ([`Shelf::new`]), with scripted
//! readers: no test reaches a real volume. Every wait for something that must
//! happen is bounded by the one-minute hang detector; none asserts how fast.

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const GIB: u64 = 1 << 30;

/// The `/`-rooted path `p`, absolute here (a drive on Windows, where a path
/// with no prefix is not): what `vol=` takes.
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

/// A board holding the home row at `free`, session 7 mapped to it, the
/// watch running.
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

/// The scripted world: every read answers the board's free figure at
/// `free`, pokes are counted, settings are on.
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

/// THE WAIT THE ONE GRAMMAR HANDS OVER (`aterm_vitals::sys::predicate`
/// holds the grammar and its table): its terms with the watch's levels, its
/// `vol=` decoded, and `timeout=` the family's. A `vol=` that is not absolute
/// is the usage line.
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

/// LATCHED: a condition already true answers at once, with no demand and no
/// poke; `room` reads the volume's room.
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

/// At arm time: a need more than a quarter of the volume beyond what is free
/// is `too-large`; a
/// volume aterm removes nothing on is `not-reclaimed`; the owner's
/// `auto_free_gib = 0` is `cleanup-off`. None registers a demand.
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

/// THE DEMAND: an owner-class wait below its need registers a demand and
/// pokes the watch; a pass that frees enough (published by a stand-in watch)
/// latches it, its counted bytes in `freed_bytes=`, and the demand leaves the
/// board. NEGATIVE CONTROL: an edge-scoped caller's same wait registers no
/// demand and pokes nothing (it times out on the unchanged volume).
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
                std::thread::yield_now();
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

/// A pass for the demand that spent every tier it may and left free space
/// below the need answers `ERR unmet why=spent`, counts not paths; and a new
/// wait for the same need within the minute is answered at arm time.
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
                std::thread::yield_now();
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

/// THE HANG DETECTOR: a wait holding a demand answers `watch-stalled` once the
/// running watch's beat is a minute old — and not while the host is
/// suspended, where the same wait times out.
#[test]
fn a_stalled_watch_is_reported_only_while_it_runs() {
    let shelf = shelf_at(20 * GIB);
    let world = World::new(20 * GIB);
    let Some(old) = Instant::now().checked_sub(STALL + Duration::from_secs(1)) else {
        aterm_spec::host::could_not_run("the monotonic clock here cannot reach back a stall");
    };
    shelf.publish(|b| b.watch = Watch::Running { beat: old });
    let reply = run_in(&shelf, "sys free>=30GiB timeout=600000", &owner(), &world);
    assert_eq!(reply, "ERR unavailable free why=watch-stalled\n");
    shelf.publish(|b| b.watch = Watch::Suspended);
    let reply = run_in(&shelf, "sys free>=30GiB timeout=50", &owner(), &world);
    assert_eq!(reply, "OK timeout\n");
    // A wait with no demand needs no detector: a `disk=` wait times out.
    shelf.publish(|b| b.watch = Watch::Running { beat: old });
    let reply = run_in(&shelf, "sys disk=ok timeout=50", &owner(), &world);
    assert_eq!(reply, "OK timeout\n");
}

/// The reader a wait takes itself: an unreadable volume (remote, refused) is
/// `ERR unavailable free why=<word>` at arm time.
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

/// NO DISK WATCH in this process (`[harness]` off): a wait that would register
/// a demand answers `why=no-watch` at once, registering and poking nothing — no
/// pass would ever adopt it. A wait that holds no demand (a `disk=` term) is a
/// plain wait still. NEGATIVE CONTROL: with the watch on, the same wait
/// registers its demand (`a_demand_is_registered_poked_and_met_by_a_pass`).
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

/// A `room` on a volume whose room is not known (only a wait measured it, or
/// the harness state was missing) is `why=room-unknown` — never a need of 0
/// met at once. NEGATIVE CONTROL: with its room known the same wait is met
/// (`a_condition_already_true_answers_at_once`).
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

/// A volume the watch's look left off the board (no session works there now)
/// is read by the parked wait itself, which goes on — never `why=unreadable`
/// while the volume reads — its row carrying the owner's lines and no room.
/// NEGATIVE CONTROL: a reading at that path of ANOTHER volume is unreadable.
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

    // A path may now name a different volume while the board still holds
    // its old row. Arming reads the new volume with its own lines and no
    // inherited room or permission to reclaim there.
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

    // A parked wait is bound to its original volume. A captured hangup on
    // the second check terminates the unfixed refresh loop deterministically;
    // neither case waits for a timeout or a real mount change.
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

    // The same stale-row refresh for the original volume still latches.
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

/// A WAIT SPELLS ONLY A TAB THE BOARD PLACES NOWHERE (rev6-l-2): before it
/// asks the board, a wait from a tab that reports 8.3 short names spells
/// them long ([`spell_owed`]) only while no look mapped the tab and no long
/// spelling of that very spelling is kept for it — one kept that places on
/// no row yet included. A tab a look mapped reads its row's mount, as it
/// did before spellings were kept, and costs its wait no file-system call.
/// NEGATIVE CONTROLS: an unmapped tab, and a spelled one once it moved, are
/// owed a spelling.
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
/// PREMISES: the long spelling places on the row, the short one on none.
/// NEGATIVE CONTROL (by hand): a `fresh` that builds the reading's row from
/// nothing answers `why=room-unknown` and the demand `why=not-reclaimed`.
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
