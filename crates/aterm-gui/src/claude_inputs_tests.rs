// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The input watch's own laws, on real files and directories and a real kernel queue.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use aterm_agent::harness::looks::Heed;
use aterm_spec::wait::MUST_HAPPEN;

use super::{InputWatch, Placed, Sink, Told};

/// A watch whose sink is a channel the test reads.
fn watch() -> (InputWatch, Receiver<Told>) {
    watch_opened(atpkg::dirwatch::Watcher::new)
}

/// [`watch`] with its watcher opened by `open`.
fn watch_opened(
    open: fn() -> std::io::Result<atpkg::dirwatch::Watcher>,
) -> (InputWatch, Receiver<Told>) {
    let (tx, rx) = channel();
    let tx = Mutex::new(tx);
    let sink: Sink = Arc::new(move |told| {
        tx.lock()
            .unwrap_or_else(|p| p.into_inner())
            .send(told)
            .is_ok()
    });
    (InputWatch::new(open, sink), rx)
}

/// Wait for `want`, past what an earlier step's events still tell (a hang detector: what must
/// be told and never is fails the test).
fn until(rx: &Receiver<Told>, want: &Told) {
    loop {
        let told = rx
            .recv_timeout(MUST_HAPPEN)
            .unwrap_or_else(|_| panic!("never told {want:?}"));
        if told == *want {
            return;
        }
    }
}

/// Everything told so far, once the forwarder has finished every wait the kernel's queued
/// events wake ([`InputWatch::wake_and_settle`]).
fn told(watch: &InputWatch, rx: &Receiver<Told>) -> BTreeSet<Told> {
    watch.wake_and_settle();
    rx.try_iter().collect()
}

/// A fresh scratch root for test `name`.
fn root(name: &str) -> (aterm_tempfile::TempDir, PathBuf) {
    let owned = aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-gui-inputs-{name}-"))
        .tempdir()
        .expect("exclusive input-watch scratch");
    let path = std::fs::canonicalize(owned.path()).unwrap();
    (owned, path)
}

/// What session `session` looks at: `paths`, each heard as `heed` says.
fn wants(session: u64, paths: &[(&Path, Heed)]) -> BTreeMap<u64, Vec<(PathBuf, Heed)>> {
    BTreeMap::from([(
        session,
        paths
            .iter()
            .map(|(path, heed)| (path.to_path_buf(), *heed))
            .collect(),
    )])
}

/// A reconcile that placed `fresh` afresh and left no session on the clocks arm.
fn placed(fresh: &[&Path]) -> Placed {
    Placed {
        fresh: fresh.iter().map(|path| path.to_path_buf()).collect(),
        clocks: BTreeSet::new(),
    }
}

/// Nothing looked at, nothing up; a file looked at, its watch is placed on it (and said fresh
/// once); nothing looked at again, it all goes.
#[test]
fn the_watch_lives_exactly_while_something_is_looked_at() {
    let (_owned_root, root) = root("lives");
    let file = root.join("row.json");
    std::fs::write(&file, b"{}").unwrap();
    let (mut watch, _rx) = watch();
    assert_eq!(watch.reconcile(&BTreeMap::new()), Placed::default());
    assert!(!watch.is_live());
    assert_eq!(
        watch.reconcile(&wants(1, &[(&file, Heed::All)])),
        placed(&[&file])
    );
    assert!(watch.is_live());
    assert_eq!(watch.level_of(&file), Some(file.clone()));
    assert_eq!(
        watch.reconcile(&wants(1, &[(&file, Heed::All)])),
        Placed::default(),
        "a kept watch is not fresh"
    );
    assert_eq!(watch.reconcile(&BTreeMap::new()), Placed::default());
    assert!(!watch.is_live(), "nothing looked at: no watch, no thread");
    assert_eq!(watch.level_of(&file), None);
}

/// A path that names nothing is watched at its nearest existing level, which tells it when an
/// entry there moves; each reconcile places it as far inward as is there, and on the file once
/// it is.
#[test]
fn an_absent_path_is_watched_at_its_nearest_level_and_moved_inward() {
    let (_owned_root, root) = root("inward");
    let path = root.join("a").join("b").join("row.json");
    let looked = wants(1, &[(&path, Heed::All)]);
    let (mut watch, rx) = watch();
    assert_eq!(watch.reconcile(&looked), placed(&[&path]));
    assert_eq!(
        watch.level_of(&path),
        Some(root.clone()),
        "the nearest level there"
    );
    std::fs::create_dir(root.join("a")).unwrap();
    until(&rx, &Told::Input(path.clone()));
    assert_eq!(watch.reconcile(&looked), placed(&[&path]));
    assert_eq!(watch.level_of(&path), Some(root.join("a")));
    std::fs::create_dir(root.join("a").join("b")).unwrap();
    until(&rx, &Told::Input(path.clone()));
    std::fs::write(&path, b"{}").unwrap();
    assert_eq!(watch.reconcile(&looked), placed(&[&path]));
    assert_eq!(
        watch.level_of(&path),
        Some(path.clone()),
        "on the file itself"
    );
    std::fs::write(&path, b"{\"pid\":1}").unwrap();
    until(&rx, &Told::Input(path.clone()));
}

/// A file replaced by a rename over it (as a usage tap writes its reading, and git its `HEAD`)
/// is told, its watch gone with the old file, and the next reconcile places it on the new one,
/// which is told in turn.
#[test]
fn a_file_replaced_by_rename_is_placed_again() {
    let (_owned_root, root) = root("rename");
    let file = root.join("HEAD");
    std::fs::write(&file, b"ref: refs/heads/main\n").unwrap();
    let looked = wants(1, &[(&file, Heed::All)]);
    let (mut watch, rx) = watch();
    assert_eq!(watch.reconcile(&looked), placed(&[&file]));
    let staged = root.join("HEAD.lock");
    std::fs::write(&staged, b"ref: refs/heads/topic\n").unwrap();
    std::fs::rename(&staged, &file).unwrap();
    until(&rx, &Told::Input(file.clone()));
    assert!(!watch.holds(&file), "the watch went with the old file");
    assert_eq!(watch.reconcile(&looked), placed(&[&file]), "placed again");
    assert!(watch.holds(&file));
    std::fs::write(&file, b"ref: refs/heads/main\n").unwrap();
    until(&rx, &Told::Input(file.clone()));
}

/// A file looked at for its contents ([`Heed::Contents`]) is told of a write, never of a touch
/// that only moves its times; one looked at for every change is told of both.
#[test]
fn a_contents_watch_hears_no_touch() {
    let (_owned_root, root) = root("contents");
    let reading = root.join("reading.json");
    let row = root.join("row.json");
    std::fs::write(&reading, b"{}").unwrap();
    std::fs::write(&row, b"{}").unwrap();
    let looked = BTreeMap::from([(
        1,
        vec![(reading.clone(), Heed::Contents), (row.clone(), Heed::All)],
    )]);
    let (mut watch, rx) = watch();
    assert_eq!(watch.reconcile(&looked), placed(&[&reading, &row]));
    let touch = |path: &Path| {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5)),
            )
            .unwrap();
    };
    touch(&reading);
    touch(&row);
    assert_eq!(
        told(&watch, &rx),
        BTreeSet::from([Told::Input(row.clone())]),
        "a touch is told of the whole watch alone"
    );
    std::fs::OpenOptions::new()
        .append(true)
        .open(&reading)
        .and_then(|mut file| std::io::Write::write_all(&mut file, b"\n"))
        .unwrap();
    assert_eq!(
        told(&watch, &rx),
        BTreeSet::from([Told::Input(reading.clone())]),
        "a write is told of the contents watch"
    );
}

/// Every path that belongs on one level shares one watch, which tells each of them.
#[test]
fn one_watch_serves_every_path_on_its_level() {
    let (_owned_root, root) = root("shared");
    let (one, two) = (root.join("one.json"), root.join("two.json"));
    let looked = BTreeMap::from([
        (1, vec![(one.clone(), Heed::All)]),
        (2, vec![(two.clone(), Heed::All)]),
    ]);
    let (mut watch, rx) = watch();
    assert_eq!(watch.reconcile(&looked), placed(&[&one, &two]));
    assert_eq!(
        watch.placed(),
        1,
        "one watch, on the directory both are absent from"
    );
    std::fs::write(root.join("beside"), b"").unwrap();
    assert_eq!(
        told(&watch, &rx),
        BTreeSet::from([Told::Input(one.clone()), Told::Input(two.clone())])
    );
}

/// A level that two looked paths share, renamed away and made again in its place, is placed again
/// on the new one at the next reconcile, for both: the old watch follows the renamed directory
/// (the kernel tells it the rename and keeps it), so it is let go as the object it was placed on
/// no longer stands at its path, and the new directory's entries are told.
///
/// NEGATIVE CONTROL (run 2026-11-16): with `Places::drop_stale` dropping only the levels the
/// watcher let go and `Places::place` keeping a level whatever object it was placed on, the test
/// fails at "the new directory is told" with `left: {} right: {Input(".../one.json"),
/// Input(".../two.json")}`.
#[test]
fn a_shared_level_made_again_in_place_is_placed_again() {
    let (_owned_root, root) = root("remade");
    let level = root.join("sessions");
    std::fs::create_dir(&level).unwrap();
    let (one, two) = (level.join("one.json"), level.join("two.json"));
    let looked = BTreeMap::from([
        (1, vec![(one.clone(), Heed::All)]),
        (2, vec![(two.clone(), Heed::All)]),
    ]);
    let (mut watch, rx) = watch();
    assert_eq!(watch.reconcile(&looked), placed(&[&one, &two]));
    assert_eq!(watch.placed(), 1, "one watch, on the shared level");
    let old = root.join("sessions.old");
    std::fs::rename(&level, &old).unwrap();
    std::fs::create_dir(&level).unwrap();
    let _ = told(&watch, &rx);
    assert_eq!(watch.reconcile(&looked), placed(&[&one, &two]));
    assert_eq!(watch.placed(), 1, "the old level's watch went");
    assert_eq!(watch.level_of(&one), Some(level.clone()));
    std::fs::write(level.join("beside"), b"").unwrap();
    assert_eq!(
        told(&watch, &rx),
        BTreeSet::from([Told::Input(one.clone()), Told::Input(two.clone())]),
        "the new directory is told"
    );
    std::fs::write(old.join("beside"), b"").unwrap();
    assert_eq!(
        told(&watch, &rx),
        BTreeSet::new(),
        "the renamed directory is not"
    );
}

/// A watch the kernel let go of is placed again at the next reconcile; a wait the forwarder is
/// only woken from tells nothing.
#[test]
fn a_watch_the_kernel_let_go_is_placed_again() {
    let (_owned_root, root) = root("lost");
    let file = root.join("row.json");
    std::fs::write(&file, b"{}").unwrap();
    let looked = wants(1, &[(&file, Heed::All)]);
    let (mut watch, rx) = watch();
    assert_eq!(watch.reconcile(&looked), placed(&[&file]));
    assert_eq!(
        told(&watch, &rx),
        BTreeSet::new(),
        "a wake alone tells nothing"
    );
    watch.lose_watch(&file);
    assert!(!watch.holds(&file));
    assert_eq!(watch.reconcile(&looked), placed(&[&file]));
    assert!(watch.holds(&file));
}

/// THE BUDGET AND THE CLOCKS ARM: past [`super::BUDGET`] levels (here two), a session whose look
/// would need another is on the clocks arm, while one whose looks share a placed level is not;
/// with the watcher refused, every session that looks at anything is.
#[test]
fn past_the_budget_a_session_is_on_the_clocks_arm() {
    let (_owned_root, root) = root("budget");
    let files: Vec<PathBuf> = (0..3).map(|n| root.join(format!("{n}.json"))).collect();
    for file in &files {
        std::fs::write(file, b"{}").unwrap();
    }
    let (mut watch, _rx) = watch();
    watch.set_budget(2);
    let looked = BTreeMap::from([
        (
            1,
            vec![(files[0].clone(), Heed::All), (files[1].clone(), Heed::All)],
        ),
        (2, vec![(files[2].clone(), Heed::All)]),
        (3, vec![(files[0].clone(), Heed::All)]),
    ]);
    let placed = watch.reconcile(&looked);
    assert_eq!(placed.clocks, BTreeSet::from([2]), "{placed:?}");
    assert_eq!(watch.placed(), 2);

    let (mut refused, _rx) =
        watch_opened(|| Err(std::io::Error::from(std::io::ErrorKind::Unsupported)));
    assert_eq!(
        refused.reconcile(&looked).clocks,
        BTreeSet::from([1, 2, 3]),
        "no watcher: every session on the clocks"
    );
    assert!(!refused.is_live());
}

/// THE WALL CLOCK'S MOVES ARE TOLD AS HOST MOVES (the footer cause design's step C8): while a
/// path is watched, the forwarder polls the wall clock's watch beside the kernel's queue
/// (`atpkg::dirwatch::Watcher::add_readable`); a step or a wake it announces is told as
/// [`Told::Host`] of [`Bell::Wall`], a new time zone as one of [`Bell::Zone`], each once, and
/// nothing is told while it announces nothing. With nothing looked at there is no live watch and
/// no wall clock watch.
///
/// NEGATIVE CONTROL (run 2026-11-16): with `Forwarder::wall_moved` telling nothing, the test fails
/// at "a step is told" with `left: {} right: {Host(Wall)}`.
#[test]
fn a_wall_clock_move_is_told_as_a_host_move() {
    use crate::host_facts::Bell;
    use aterm_uds::wall_watch::Moved;
    let (_owned_root, root) = root("wall");
    let file = root.join("row.json");
    std::fs::write(&file, b"{}").unwrap();
    let (mut watch, rx) = watch();
    assert!(
        watch.wall().is_none(),
        "nothing watched: no wall clock watch"
    );
    let _ = watch.reconcile(&wants(1, &[(&file, Heed::All)]));
    assert!(told(&watch, &rx).is_empty(), "CONTROL: nothing moved");
    let wall = watch.wall().expect("a live watch polls the wall clock");
    wall.tell(Moved {
        stepped: true,
        ..Moved::default()
    });
    assert_eq!(
        told(&watch, &rx),
        BTreeSet::from([Told::Host(Bell::Wall)]),
        "a step is told"
    );
    assert!(told(&watch, &rx).is_empty(), "once");
    let wall = watch.wall().expect("still live");
    wall.tell(Moved {
        woke: true,
        zoned: true,
        ..Moved::default()
    });
    assert_eq!(
        told(&watch, &rx),
        BTreeSet::from([Told::Host(Bell::Wall), Told::Host(Bell::Zone)]),
        "a wake and a new zone"
    );
}

/// THE WATCH HEARS THE WALL ONLY WHERE ITS WALL CLOCK WATCH HEARS EACH SLEEP
/// ([`InputWatch::hears_wall`], which the footer's resolver waits by: a wait toward a deadline
/// runs whole only where it says so, `claude_footer::WALL_LOOK` otherwise). Nothing watched: no
/// live watch, so no. A watch told by hand counts each sleep it is told: yes. No wall clock watch
/// had: no. A watch of the system's step announcement alone (`WallWatch::steps_only`, as
/// `WallWatch::start_zoned` makes one where the power half was refused) hears a sleep only where
/// a resume is a step (Linux's timer), so on macOS no; a step it hears is told all the same.
///
/// NEGATIVE CONTROL (run 2026-11-16 on macOS, restored): with `Live::start` taking any polled
/// wall clock watch as hearing the wall (`wall.is_some()`), the test fails at "a watch of steps
/// alone hears a sleep only where a resume is a step" with `left: true right: false`.
#[test]
fn a_watch_hears_the_wall_only_where_its_wall_clock_watch_hears_each_sleep() {
    use crate::host_facts::Bell;
    use aterm_uds::wall_watch::{Moved, WallWatch};
    let (_owned_root, root) = root("hears-wall");
    let file = root.join("row.json");
    std::fs::write(&file, b"{}").unwrap();
    let looked = wants(1, &[(&file, Heed::All)]);

    let (mut by_hand, _rx) = watch();
    assert!(!by_hand.hears_wall(), "nothing watched: no live watch");
    let _ = by_hand.reconcile(&looked);
    assert!(
        by_hand.hears_wall(),
        "a watch told by hand hears each sleep"
    );

    let (mut none, _rx) = watch();
    none.hear_wall_by(|| None);
    let _ = none.reconcile(&looked);
    assert!(none.is_live(), "CONTROL: the file is watched");
    assert!(!none.hears_wall(), "no wall clock watch: no sleep heard");

    let (mut steps, rx) = watch();
    steps.hear_wall_by(|| WallWatch::steps_only().map(Arc::new));
    let _ = steps.reconcile(&looked);
    let wall = steps
        .wall()
        .expect("macOS and Linux announce a step: a wall clock watch is polled");
    assert_eq!(
        steps.hears_wall(),
        cfg!(target_os = "linux"),
        "a watch of steps alone hears a sleep only where a resume is a step"
    );
    wall.tell(Moved {
        stepped: true,
        ..Moved::default()
    });
    assert_eq!(
        told(&steps, &rx),
        BTreeSet::from([Told::Host(Bell::Wall)]),
        "a step it hears is told"
    );
}
