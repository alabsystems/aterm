// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A DANGLING LINK STAYS ON THE BOARD WHILE IT DANGLES, AND ONLY THEN: a search that walked
/// another checkout adds its link and drops none, a look with no search keeps both, and once
/// one's target is back (a rebuild made `target.noindex` again) or the other link is gone,
/// the next look drops it. NEGATIVE CONTROL: take each search's links as the whole list, and
/// the first checkout's link is dropped while it still dangles.
#[cfg(unix)]
#[test]
fn a_dangling_link_stays_on_the_board_while_it_dangles() {
    use aterm_agent::harness::disk::discover::Dangling;
    let tmp = aterm_tempfile::tempdir().expect("scratch");
    let link = |repo: &str| {
        let checkout = tmp.path().join(repo);
        std::fs::create_dir_all(&checkout).expect("checkout");
        let link = checkout.join("target");
        std::os::unix::fs::symlink("target.noindex", &link).expect("link");
        Dangling {
            link,
            target: PathBuf::from("target.noindex"),
            checkout,
            state: aterm_vitals::left::LinkTarget::Gone,
        }
    };
    let (a, b) = (link("a"), link("b"));
    let shelf = Shelf::new();
    shelf.keep_dangling(vec![a.clone()]);
    shelf.keep_dangling(vec![b.clone()]);
    assert_eq!(
        shelf.dangling(),
        vec![a.clone(), b.clone()],
        "a search of b alone"
    );
    shelf.keep_dangling(Vec::new());
    assert_eq!(
        shelf.dangling(),
        vec![a.clone(), b.clone()],
        "a look with no search"
    );
    std::fs::create_dir(a.checkout.join("target.noindex")).expect("rebuilt");
    std::fs::remove_file(&b.link).expect("removed");
    shelf.keep_dangling(Vec::new());
    assert_eq!(shelf.dangling(), Vec::<Dangling>::new(), "both fixed since");
}

/// A BEAT FRESHENS AN OLD ONE, however it comes, or a look that began over an old beat would
/// answer a stall at once; and a suspended watch stays suspended, though a look can run there.
#[test]
fn a_beat_freshens_the_watch_unless_it_is_suspended() {
    let stall = crate::control::control_disk::STALL;
    let Some(old) = Instant::now().checked_sub(stall + Duration::from_secs(1)) else {
        aterm_spec::host::could_not_run("the monotonic clock here cannot reach back a stall");
    };
    let fresh = |shelf: &Shelf| matches!(shelf.board_lock().watch, Watch::Running { beat } if beat.elapsed() < stall);
    let moves = [
        ("a heard beat", Shelf::beat as fn(&Shelf)),
        ("a look's start", Shelf::look_began),
        ("a look's end", Shelf::look_ended),
    ];
    for (what, step) in moves {
        let shelf = Shelf::new();
        shelf.publish(|b| b.watch = Watch::Running { beat: old });
        step(&shelf);
        assert!(fresh(&shelf), "{what} freshens an old beat");
        shelf.publish(|b| b.watch = Watch::Suspended);
        step(&shelf);
        assert_eq!(shelf.board_lock().watch, Watch::Suspended, "{what}");
    }
}

fn row(mount: &str, key: VolumeKey, free: u64, level: Level) -> VolumeRow {
    VolumeRow {
        reading: Reading {
            mount: PathBuf::from(mount),
            key,
            total: 2000 << 30,
            free,
            files_free: Some(1 << 30),
            dev: 1,
            read_only: false,
        },
        thresholds: aterm_agent::harness::disk::Config::default().thresholds(Some(2000 << 30)),
        level,
        room: Some(16 << 30),
        as_of: Instant::now(),
        as_of_unix: 0,
        reclaimed_here: true,
    }
}

/// `status` reads `-` on an empty board.
#[cfg(unix)]
#[test]
fn status_reads_the_row_of_the_sessions_volume_or_dashes() {
    let mut b = Board::new();
    let empty = b.row_of(7, Some("/Users//x")).is_none();
    assert!(empty);
    b.table = Table {
        mounts: vec![aterm_sysprobe::volumes::Mount {
            on: PathBuf::from("/System/Volumes/Data"),
            from: "/dev/disk3s5".to_owned(),
            fstype: "apfs".to_owned(),
            local: true,
            automounted: false,
            removable: false,
            read_only: false,
            container: Some("disk3".to_owned()),
        }],
        complete: true,
        firmlinks: aterm_sysprobe::volumes::parse_firmlinks("/Users\tUsers\n"),
    };
    b.volumes.push(row(
        "/System/Volumes/Data",
        VolumeKey::Container("disk3".to_owned()),
        41 << 30,
        Level::Low,
    ));
    b.sessions.insert(7, 0);
    let mapped = b.row_of(7, None).expect("mapped");
    assert_eq!(mapped.level, Level::Low);
    let spelled = b.row_of(8, Some("/Users//x/repo")).expect("by its spelling");
    assert_eq!(spelled.reading.free, 41 << 30);
    assert!(b.row_of(9, Some("/elsewhere")).is_none());
    assert!(b.row_of(9, Some("relative")).is_none());
}

/// WINDOWS: a session's working directory in any spelling a Windows tab reports reads the row
/// of the volume the table places it on.
#[cfg(windows)]
#[test]
fn a_windows_spelling_reads_the_row_of_its_volume() {
    use aterm_sysprobe::volumes::{Mount, NETWORK};
    let volume = |on: &str, n: u32, guid: &str| Mount {
        on: PathBuf::from(on),
        from: format!(r"\Device\HarddiskVolume{n}"),
        fstype: String::new(),
        local: true,
        automounted: false,
        removable: false,
        read_only: false,
        container: Some(format!("Volume{{{guid}}}")),
    };
    let c = volume(r"C:\", 3, "6140fc3b-044f-43d7-b060-cb98170c7a63");
    let key = c.key().expect("a volume key");
    let mut b = Board::new();
    b.table = Table {
        mounts: vec![
            Mount {
                on: PathBuf::from(NETWORK),
                from: "UNC".to_owned(),
                fstype: String::new(),
                local: false,
                automounted: false,
                removable: false,
                read_only: false,
                container: None,
            },
            c,
            volume(r"D:\", 4, "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d"),
        ],
        complete: true,
        firmlinks: Vec::new(),
    };
    b.volumes.push(row(r"C:\", key, 41 << 30, Level::Low));
    for spelled in [
        r"C:\Users\x\repo",
        r"\\?\C:\Users\x\repo",
        r"c:\USERS\X\REPO\",
        "C:/Users//x/repo",
        r"C:\",
    ] {
        let found = b.row_of(8, Some(spelled)).map(|r| r.reading.free);
        assert_eq!(found, Some(41 << 30), "{spelled}");
    }
    for unplaced in [r"D:\work", r"\\srv\share\x", r"C:\PROGRA~1\x", r"Users\x"] {
        assert!(b.row_of(9, Some(unplaced)).is_none(), "{unplaced}");
    }
}

#[test]
fn demands_are_adopted_by_their_volumes_largest_need() {
    let mut b = Board::new();
    let key = VolumeKey::Container("disk3".to_owned());
    let other = VolumeKey::Fsid(9);
    let now = Instant::now();
    let a = b.demand(1, key.clone(), 5 << 30, Some(PathBuf::from("/h/a")), now);
    let c = b.demand(2, key.clone(), 9 << 30, Some(PathBuf::from("/h/b")), now);
    b.demand(3, other.clone(), 99 << 30, None, now);
    let (need, cwds) = b.adopt(&key).expect("adopted");
    assert_eq!(need, 9 << 30);
    assert_eq!(cwds.len(), 2);
    b.drop_demand(c);
    assert_eq!(b.adopt(&key).map(|(n, _)| n), Some(5 << 30));
    b.drop_demand(a);
    assert!(b.adopt(&key).is_none());
    b.refresh(row("/m", key.clone(), 1, Level::Short));
    b.refresh(row("/m", key.clone(), 2, Level::Short));
    assert_eq!(b.volumes.len(), 1);
    assert_eq!(b.row_keyed(&key).map(|r| r.reading.free), Some(2));
}

const GIB: u64 = 1 << 30;
const UTC_MIDNIGHT: i64 = 20_718 * 86_400;
const EAST_2H: i64 = 7_200;

fn clock(unix: i64) -> Clock {
    Clock {
        at: Instant::now(),
        unix: Some(unix),
        offset: Some(EAST_2H),
    }
}

#[test]
fn freed_tally_counts_the_local_day_and_starts_again_at_its_midnight() {
    let local_midnight = UTC_MIDNIGHT - EAST_2H;
    let mut t = FreedTally::new();
    assert_eq!(t.today(&clock(local_midnight - 600)), None);

    let at_2350 = clock(local_midnight - 600);
    t.add(5 * GIB, BUILD_CACHES, &at_2350);
    t.add(GIB, "old Claude Code versions", &at_2350);
    let today = t.today(&at_2350).expect("freed today");
    assert_eq!(today.aterm_bytes, 6 * GIB);
    assert_eq!(today.last_at, local_midnight - 600);
    assert_eq!(today.last_what, "old Claude Code versions");

    let at_2359 = clock(local_midnight - 60);
    t.add(3 * GIB, BUILD_CACHES, &at_2359);
    t.add(0, "old package builds", &clock(local_midnight - 30));
    let today = t.today(&at_2359).expect("freed today");
    assert_eq!(today.aterm_bytes, 9 * GIB);
    assert_eq!(today.last_at, local_midnight - 60);
    assert_eq!(today.last_what, "build caches");

    let no_zone = Clock {
        offset: None,
        ..clock(local_midnight + 60)
    };
    let no_wall = Clock {
        unix: None,
        offset: None,
        ..no_zone
    };
    t.add(4 * GIB, BUILD_CACHES, &no_zone);
    t.add(4 * GIB, BUILD_CACHES, &no_wall);
    assert_eq!((no_zone.day(), no_wall.day()), (None, None));
    assert_eq!(t.today(&no_zone), None);
    assert_eq!(t.today(&at_2359).map(|t| t.aterm_bytes), Some(9 * GIB));

    assert_eq!(t.today(&clock(local_midnight + 60)), None, "a new day");
    t.add(GIB, BUILD_CACHES, &clock(local_midnight + 60));
    t.add(2 * GIB, BUILD_CACHES, &clock(UTC_MIDNIGHT));
    let today = t.today(&clock(UTC_MIDNIGHT + 1)).expect("freed today");
    assert_eq!(today.aterm_bytes, 3 * GIB);
    assert_eq!(Some(today.day), clock(local_midnight).day());
    assert_eq!(today.last_at, UTC_MIDNIGHT);
}

fn lines() -> Thresholds {
    Thresholds {
        warn: 69 * GIB,
        floor: 34 * GIB,
        target: 50 * GIB,
        emergency: 8 * GIB,
        files_emergency: 10_000,
    }
}

fn measured(mount: &str, key: VolumeKey, free: u64, files_free: u64) -> VolumeRow {
    let thresholds = lines();
    VolumeRow {
        reading: Reading {
            mount: PathBuf::from(mount),
            key,
            total: 2000 * GIB,
            free,
            files_free: Some(files_free),
            dev: 1,
            read_only: false,
        },
        thresholds,
        level: thresholds.level(Some(free), Some(files_free)),
        room: Some(16 * GIB),
        as_of: Instant::now(),
        as_of_unix: 0,
        reclaimed_here: true,
    }
}

fn pass(key: VolumeKey, left: Vec<Hog>, ended_at_unix: Option<i64>) -> PassSummary {
    PassSummary {
        key,
        ended_at: Instant::now(),
        free_after: None,
        tiers: vec![1],
        freed: 0,
        demand: None,
        ran_out: false,
        spent: false,
        held_back: false,
        hogs: left.len(),
        largest: left.iter().map(|h| h.bytes).max().unwrap_or(0),
        left,
        ended_at_unix,
    }
}

#[test]
fn deck_facts_with_the_watch_on() {
    let home = VolumeKey::Container("disk3".to_owned());
    let backup = VolumeKey::Fsid(9);
    let mut b = Board::new();
    b.watched = true;
    b.volumes.push(measured(
        "/System/Volumes/Data",
        home.clone(),
        41 * GIB,
        1 << 30,
    ));
    b.volumes.push(measured(
        "/Volumes/Backup",
        backup.clone(),
        12 * GIB,
        1 << 30,
    ));
    b.episode.stuck.insert("s1".to_owned(), "ENOSPC".to_owned());
    b.episode.stuck.insert("s2".to_owned(), "ENOSPC".to_owned());
    let beat = Instant::now();
    b.watch = Watch::Running { beat };
    let now = Clock {
        at: beat + crate::control::control_disk::STALL + Duration::from_secs(90),
        ..clock(UTC_MIDNIGHT + 37_800)
    };
    b.freed
        .add(41 * GIB, BUILD_CACHES, &clock(UTC_MIDNIGHT + 30_000));
    let hog = Hog {
        built_s_ago: None,
        path: PathBuf::from("/Users//x/aterm/target"),
        bytes: 212 * GIB,
        bytes_partial: false,
        why: "a live checkout's build directory".to_owned(),
        serves: None,
    };
    b.last_pass = Some(pass(
        home.clone(),
        vec![hog.clone()],
        Some(UTC_MIDNIGHT + 30_000),
    ));

    let f = b.deck_facts(&home, &now);
    assert!(f.watch);
    assert_eq!(f.level, Some(Level::Low));
    assert_eq!(
        (f.warn, f.floor, f.emergency),
        (Some(69 * GIB), Some(34 * GIB), Some(8 * GIB))
    );
    assert!(!f.files_emergency);
    assert_eq!(f.stuck, 2);
    assert_eq!(f.others, [("Backup".to_owned(), Level::Short, 12 * GIB)]);
    let quiet = crate::control::control_disk::STALL.as_secs() + 90;
    assert_eq!(
        f.stalled_since,
        Some(UTC_MIDNIGHT + 37_800 - i64::try_from(quiet).expect("seconds"))
    );
    let freed = f.freed_today.expect("freed today");
    assert_eq!(
        (freed.aterm_bytes, freed.last_at),
        (41 * GIB, UTC_MIDNIGHT + 30_000)
    );
    assert_eq!(f.hogs, std::slice::from_ref(&hog));
    assert_eq!(f.hogs_at, Some(UTC_MIDNIGHT + 30_000));

    let unread = Clock {
        unix: None,
        offset: None,
        ..now
    };
    assert_eq!(b.deck_facts(&home, &unread).stalled_since, None);
    b.last_pass = Some(pass(home.clone(), vec![hog.clone()], None));
    let f = b.deck_facts(&home, &now);
    assert_eq!((f.hogs.len(), f.hogs_at), (1, None));

    let fresh = Clock {
        at: beat + crate::control::control_disk::STALL - Duration::from_secs(1),
        ..now
    };
    assert_eq!(b.deck_facts(&home, &fresh).stalled_since, None);
    b.watch = Watch::Suspended;
    assert_eq!(b.deck_facts(&home, &now).stalled_since, None);

    b.volumes[0] = measured("/System/Volumes/Data", home.clone(), 41 * GIB, 10);
    let f = b.deck_facts(&home, &now);
    assert_eq!(f.level, Some(Level::Emergency));
    assert!(f.files_emergency);
    b.volumes[0] = measured("/System/Volumes/Data", home.clone(), 6 * GIB, 10);
    assert!(!b.deck_facts(&home, &now).files_emergency);
    b.volumes[0].reclaimed_here = false;
    let f = b.deck_facts(&home, &now);
    assert_eq!((f.warn, f.floor), (Some(69 * GIB), None));
    assert_eq!(f.emergency, Some(8 * GIB));
    b.volumes[0].reclaimed_here = true;
    assert_eq!(b.deck_facts(&home, &now).floor, Some(34 * GIB));
    b.volumes[0].thresholds.floor = 0;
    assert_eq!(b.deck_facts(&home, &now).floor, None);

    b.last_pass = Some(pass(backup.clone(), vec![hog], Some(UTC_MIDNIGHT)));
    let f = b.deck_facts(&home, &now);
    assert!(f.hogs.is_empty());
    assert_eq!(f.hogs_at, None);

    let elsewhere = b.deck_facts(&VolumeKey::Fsid(77), &now);
    assert!(elsewhere.watch);
    assert_eq!((elsewhere.level, elsewhere.warn), (None, None));
    assert_eq!(elsewhere.others.len(), 2);

    b.watched = false;
    assert_eq!(b.deck_facts(&home, &now), DiskFactsRaw::default());
}

#[test]
fn with_the_harness_off_the_deck_reads_no_board_facts() {
    let home = VolumeKey::Container("disk3".to_owned());
    let shelf = Shelf::new();
    shelf.publish(|b| {
        b.watched = true;
        b.volumes.push(measured(
            "/System/Volumes/Data",
            home.clone(),
            41 * GIB,
            1 << 30,
        ));
        b.episode.stuck.insert("s1".to_owned(), "ENOSPC".to_owned());
        b.freed.add(GIB, BUILD_CACHES, &clock(UTC_MIDNIGHT));
    });
    let now = clock(UTC_MIDNIGHT + 60);
    assert!(shelf.watched());
    let on = shelf.deck_facts(&home, &now);
    assert!(on.watch);
    assert_eq!(on.level, Some(Level::Low));
    assert_eq!(on.stuck, 1);
    assert!(on.freed_today.is_some());
    shelf.publish(|b| b.watched = false);
    assert!(!shelf.watched());
    let off = shelf.deck_facts(&home, &now);
    assert_eq!(off, DiskFactsRaw::default());
    assert!(!off.watch);
    assert_eq!(off.level, None);
    assert_eq!((off.warn, off.floor, off.emergency), (None, None, None));
    assert_eq!((off.stuck, off.freed_today), (0, None));
}

#[test]
fn a_waits_own_reading_keeps_the_watchs_judgement_of_its_volume() {
    let key = VolumeKey::Container("disk3".to_owned());
    let plain = aterm_agent::harness::disk::Config::default().thresholds(Some(2000 << 30));
    let raised = aterm_agent::harness::disk::Thresholds {
        warn: 900 << 30,
        ..plain
    };
    let mut b = Board::new();
    b.volumes.push(VolumeRow {
        thresholds: raised,
        level: raised.level(Some(41 << 30), Some(1 << 30)),
        ..row("/m", key.clone(), 41 << 30, Level::Low)
    });
    let wait = |free: u64, key: VolumeKey| VolumeRow {
        thresholds: plain,
        level: plain.level(Some(free), Some(1 << 30)),
        room: None,
        reclaimed_here: false,
        ..row("/m", key, free, Level::Ok)
    };
    assert_ne!(
        raised.level(Some(500 << 30), Some(1 << 30)),
        plain.level(Some(500 << 30), Some(1 << 30)),
        "PREMISE: the raised lines read 500 GiB otherwise"
    );
    b.refresh(wait(500 << 30, key.clone()));
    let held = b.row_keyed(&key).expect("the watch's row").clone();
    assert_eq!(held.reading.free, 500 << 30, "the wait's reading");
    assert_eq!(held.room, Some(16 << 30), "the watch's room");
    assert!(held.reclaimed_here, "the watch's reclaim");
    assert_eq!(held.thresholds, raised, "the watch's lines");
    assert_eq!(held.level, raised.level(Some(500 << 30), Some(1 << 30)));

    let other = VolumeKey::Fsid(9);
    b.refresh(wait(7 << 30, other.clone()));
    let added = b.row_keyed(&other).expect("a volume no row held");
    assert_eq!(
        (added.room, added.reclaimed_here, added.thresholds),
        (None, false, plain)
    );

    b.measured(wait(400 << 30, key.clone()));
    let judged = b.row_keyed(&key).expect("the watch's row");
    assert_eq!(
        (judged.room, judged.reclaimed_here, judged.thresholds),
        (None, false, plain)
    );
}

fn one_volume_board() -> (Board, &'static str, &'static str) {
    let mut b = Board::new();
    #[cfg(windows)]
    let (mount, placed, short) = (
        aterm_sysprobe::volumes::Mount {
            on: PathBuf::from(r"C:\"),
            from: r"\Device\HarddiskVolume3".to_owned(),
            fstype: String::new(),
            local: true,
            automounted: false,
            removable: false,
            read_only: false,
            container: Some("Volume{6140fc3b-044f-43d7-b060-cb98170c7a63}".to_owned()),
        },
        r"\\?\C:\Program Files\x",
        r"C:\PROGRA~1\x",
    );
    #[cfg(not(windows))]
    let (mount, placed, short) = (
        aterm_sysprobe::volumes::Mount {
            on: PathBuf::from("/System/Volumes/Data"),
            from: "/dev/disk3s5".to_owned(),
            fstype: "apfs".to_owned(),
            local: true,
            automounted: false,
            removable: false,
            read_only: false,
            container: Some("disk3".to_owned()),
        },
        "/System/Volumes/Data/Users//x/repo",
        "/tmp/REPO~1/x",
    );
    let key = mount.key().expect("a volume key");
    b.volumes
        .push(row(&mount.on.to_string_lossy(), key, 41 << 30, Level::Low));
    b.table = Table {
        mounts: vec![mount],
        complete: true,
        firmlinks: Vec::new(),
    };
    (b, placed, short)
}

#[test]
fn a_session_spelled_long_reads_its_volumes_row_while_it_reports_it() {
    let (mut b, placed, short) = one_volume_board();
    assert!(b.row_at(Path::new(placed)).is_some(), "PREMISE: {placed}");
    assert!(b.row_of(7, Some(short)).is_none(), "unspelled: {short}");
    b.spell(7, PathBuf::from(short), PathBuf::from(placed));
    let free = |b: &Board, id: u64, cwd: &str| b.row_of(id, Some(cwd)).map(|r| r.reading.free);
    assert_eq!(free(&b, 7, short), Some(41 << 30));
    assert_eq!(free(&b, 8, short), None, "another session's spelling");
    let moved = if cfg!(windows) {
        r"C:\PROGRA~2\y"
    } else {
        "/tmp/OTHER~1/y"
    };
    assert_eq!(free(&b, 7, moved), None, "the session moved");
    let first = b.volumes[0].clone();
    b.volumes = vec![VolumeRow {
        reading: Reading {
            free: 9 << 30,
            ..first.reading
        },
        ..first
    }];
    assert_eq!(free(&b, 7, short), Some(9 << 30), "the new rows");
    for id in 100..100 + MAX_SPELLED as u64 {
        b.spell(id, PathBuf::from(short), PathBuf::from(placed));
    }
    assert_eq!(b.spelled.len(), MAX_SPELLED);
    assert_eq!(free(&b, 7, short), None, "the oldest went");
    assert_eq!(free(&b, 100, short), Some(9 << 30));
}

/// A sizing of the volume `key`: 926 GiB, 779 GiB used, at least 381 GiB of it rebuildable,
/// ended at unix second `at`.
fn sizing(key: &VolumeKey, at: i64) -> Sizing {
    Sizing {
        volume: key.spelled(),
        total: 926 * GIB,
        used: 779 * GIB,
        sized: aterm_agent::harness::disk::Rebuildable {
            bytes: 381 * GIB,
            partial: true,
        },
        sized_unix: at,
    }
}

/// THE BUDGET READS ON ITS OWN VOLUME ALONE: the volume the watch sizes reads the sizing's
/// figures with its age; another volume, one with no row and a process that sizes none read
/// not sampled, whatever the cap; before a watching process's first look, and on its volume
/// before a sizing, it reads warming; and a clock that is unread or reads before the sizing
/// gives an age of 0, never a wrapped one.
#[test]
fn the_budget_reads_on_its_own_volume_alone() {
    let home = VolumeKey::Container("disk3".to_owned());
    let other = VolumeKey::Container("disk7".to_owned());
    let mut b = Board::new();
    assert_eq!(
        b.budget_read(Some(&home), Some(2_800)),
        BudgetRead::NotSampled
    );
    b.watched = true;
    assert_eq!(b.budget_read(Some(&home), Some(2_800)), BudgetRead::Warming);
    b.budget_look(Cap::Written(466), Some(home.spelled()));
    assert_eq!(b.budget_read(Some(&home), Some(2_800)), BudgetRead::Warming);
    b.budget_sized(sizing(&home, 1_000));
    let facts = BudgetFacts {
        cap: 466 * GIB,
        cap_written: true,
        kept: 398 * GIB,
        target: 68 * GIB,
        rebuildable: 381 * GIB,
        partial: true,
        sized_s: 1_800,
    };
    assert_eq!(
        b.budget_read(Some(&home), Some(2_800)),
        BudgetRead::Sized(facts)
    );
    assert_eq!(
        b.budget_read(Some(&other), Some(2_800)),
        BudgetRead::NotSampled
    );
    assert_eq!(b.budget_read(None, Some(2_800)), BudgetRead::NotSampled);
    let fresh = BudgetRead::Sized(BudgetFacts {
        sized_s: 0,
        ..facts
    });
    assert_eq!(b.budget_read(Some(&home), None), fresh);
    assert_eq!(b.budget_read(Some(&home), Some(900)), fresh);

    // The key is read before the switch: off on the sized volume, not sampled elsewhere.
    b.budget_look(Cap::Off, Some(home.spelled()));
    assert_eq!(b.budget_read(Some(&home), Some(2_800)), BudgetRead::Off);
    assert_eq!(
        b.budget_read(Some(&other), Some(2_800)),
        BudgetRead::NotSampled
    );

    // A process that is not the account's own watch sizes nothing and says so.
    b.budget_look(Cap::Written(466), None);
    assert_eq!(
        b.budget_read(Some(&home), Some(2_800)),
        BudgetRead::NotSampled
    );

    // A sizing of another volume is not this one's.
    b.budget_look(Cap::Written(466), Some(other.spelled()));
    assert_eq!(
        b.budget_read(Some(&other), Some(2_800)),
        BudgetRead::Warming
    );
}

/// A CAP EDIT MOVES THE BUDGET WITHOUT A WALK, AND THE LATER SIZING STANDS: the stored sizing
/// gives its budget under the cap each look reads, so an unwritten cap reads the volume's
/// share, a cap that turns the budget off keeps the sizing, and the cap written again reads the
/// first figures back; an older record read after a newer sizing does not replace it.
#[test]
fn a_cap_edit_moves_the_budget_without_a_walk() {
    let home = VolumeKey::Container("disk3".to_owned());
    let mut b = Board::new();
    b.budget_look(Cap::Written(466), Some(home.spelled()));
    b.budget_sized(sizing(&home, 1_000));
    let first = b.budget_read(Some(&home), Some(1_000));

    b.budget_look(Cap::Unwritten, Some(home.spelled()));
    let share = (926 * GIB / 100) * 80;
    let BudgetRead::Sized(unwritten) = b.budget_read(Some(&home), Some(1_000)) else {
        panic!("an unwritten cap still reads a budget");
    };
    assert_eq!((unwritten.cap, unwritten.cap_written), (share, false));
    assert_eq!(unwritten.kept, 398 * GIB);
    assert_eq!(unwritten.target, share - 398 * GIB);
    assert_eq!(unwritten.rebuildable, 381 * GIB);

    b.budget_look(Cap::Off, Some(home.spelled()));
    assert_eq!(b.budget_read(Some(&home), Some(1_000)), BudgetRead::Off);
    assert!(
        b.budget.is_some(),
        "the sizing is kept while the budget is off"
    );

    b.budget_look(Cap::Written(466), Some(home.spelled()));
    assert_eq!(b.budget_read(Some(&home), Some(1_000)), first);
    assert!(matches!(first, BudgetRead::Sized(f) if f.cap == 466 * GIB && f.cap_written));

    b.budget_sized(sizing(&home, 500));
    assert_eq!(
        b.budget.as_ref().map(|s| s.sized_unix),
        Some(1_000),
        "older"
    );
    b.budget_sized(sizing(&home, 4_600));
    assert_eq!(
        b.budget.as_ref().map(|s| s.sized_unix),
        Some(4_600),
        "newer"
    );
}

/// A BOARD WITH A VOLUME FAR OVER ITS BUDGET SAYS WHAT IT SAID WITHOUT ONE: `status disk=`'s
/// fields and the Stats Deck's facts for the home volume are the same with no budget, with one
/// 313 GiB over and with the budget off. CONTROL: a lower free figure moves both, so the
/// comparison can fail.
#[test]
fn a_board_over_its_budget_says_disk_ok_in_status_and_on_the_deck() {
    let home = VolumeKey::Container("disk3".to_owned());
    let mut b = Board::new();
    b.watched = true;
    b.volumes.push(measured(
        "/System/Volumes/Data",
        home.clone(),
        900 * GIB,
        1 << 30,
    ));
    b.sessions.insert(7, 0);
    let at = Instant::now();
    let now = clock(UTC_MIDNIGHT + 37_800);
    let said = |b: &Board| (b.status_words(7, None, at), b.deck_facts(&home, &now));
    let plain = said(&b);
    assert!(plain.0.starts_with("disk=ok "), "{}", plain.0);
    assert_eq!(plain.1.level, Some(Level::Ok));

    b.budget_look(Cap::Written(466), Some(home.spelled()));
    b.budget_sized(sizing(&home, UTC_MIDNIGHT));
    assert!(matches!(
        b.budget_read(Some(&home), now.unix),
        BudgetRead::Sized(f) if f.rebuildable > f.target
    ));
    assert_eq!(said(&b), plain, "over its budget");
    b.budget_look(Cap::Off, Some(home.spelled()));
    assert_eq!(said(&b), plain, "the budget off");

    // CONTROL: the board's own reading moves what it says.
    b.volumes[0] = measured("/System/Volumes/Data", home.clone(), 40 * GIB, 1 << 30);
    let low = said(&b);
    assert_ne!(low.0, plain.0);
    assert_ne!(low.1, plain.1);
}
