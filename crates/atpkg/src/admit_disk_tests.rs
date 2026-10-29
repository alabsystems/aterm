// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A run's disk claim on an injected machine and relief: no test reads the live volume, runs
//! the build cache's pass or asks a window.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_capacity::floor::GIB;
use aterm_capacity::{Admit, Fixed, Footprints, Ledger, Pace, Reading, Volume};
use aterm_spec::wait::MUST_HAPPEN;

use super::{Reclaim, Run, admit_on, dirs_tag};

/// The scratch image's volume: `total` GiB, `free` of it free, a 4 GiB floor.
fn image(total: u64, free: u64) -> Reading {
    let mut r = Reading::of_gib(48, 40, 14);
    r.volume = Some(Volume {
        mount: PathBuf::from("/Volumes/scratch"),
        total: total * GIB,
        free: free * GIB,
        floor: 4 * GIB,
        room: 2 * GIB,
    });
    r
}

/// A verify run whose directories are under `dir`.
fn verify_run(dir: &Path) -> Run {
    let target = dir.join("target");
    std::fs::create_dir_all(target.join("verify")).expect("the run's corner");
    Run {
        what: "verify lint -p aterm-capacity".to_owned(),
        dirs: vec![target.join("build.noindex"), target.join("verify")],
        record: target.join("verify/footprint.json"),
        cold: 27 * GIB,
        on: PathBuf::from("/Volumes/scratch"),
    }
}

/// What a first [`verify_run`] on empty directories claims, as its first line says it: with no
/// record, the tag of its directories counts no run, so any tag reads the same.
fn first_run() -> aterm_capacity::Estimate {
    Footprints::default().need(0, 27 * GIB, 0)
}

/// `line` names each of `figures`.
#[track_caller]
fn names(line: &str, figures: &[&str]) {
    for f in figures {
        assert!(line.contains(f), "{line:?} does not name {f:?}");
    }
}

/// What `thread` returned, once it ends, which it must: within the one-minute hang detector, so
/// an admission that never comes fails the test instead of hanging it.
#[track_caller]
fn joined<T>(thread: std::thread::JoinHandle<T>, what: &str) -> T {
    aterm_spec::wait::until_within(MUST_HAPPEN, what, || thread.is_finished().then_some(()));
    thread.join().expect(what)
}

/// A first run claims the 27 GiB cold budget, and says so first. On a volume 18 GiB above its
/// floor it reclaims first (its line naming what stayed of a stage the pass could not finish),
/// then asks the window's watch, and only then waits, its line naming the need, the free space
/// and the largest holder; it is admitted once room comes, and what it wrote is kept.
/// Negative control: the next run, with room, is relieved of nothing and says nothing, and it
/// claims what the first run wrote, not the cold budget.
#[test]
fn a_short_volume_is_reclaimed_first_then_waited_for() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-disk-")
        .tempdir()
        .expect("scratch");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let m = Arc::new(Fixed::new(image(40, 20)));
    let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let (tx, rx) = mpsc::channel::<String>();
    let run = verify_run(dir.path());
    let record = run.record.clone();
    let (theirs, their_order, their_ledger) = (Arc::clone(&m), Arc::clone(&order), ledger.clone());
    let waiting = std::thread::spawn(move || {
        let (pass_m, pass_order, ask_order) = (
            Arc::clone(&theirs),
            Arc::clone(&their_order),
            Arc::clone(&their_order),
        );
        let mut relief = Reclaim::of(
            Box::new(move |mount, _stopped| {
                assert_eq!(mount, Path::new("/Volumes/scratch"));
                pass_order.lock().expect("order").push("reclaim");
                pass_m.set(image(40, 22));
                (
                    2 * GIB,
                    vec![PathBuf::from("/Volumes/scratch/gone/.cargo-build")],
                    vec![
                        "1 entry of /Volumes/scratch/left/.cargo-build.removing stayed: tmp/d"
                            .to_owned(),
                    ],
                )
            }),
            Box::new(move |bytes, _mount, _stopped| {
                ask_order.lock().expect("order").push("ask");
                assert_eq!(bytes, 31 * GIB, "the floor and the need");
                Some("OK why=no-room".to_owned())
            }),
            Box::new(|_mount, _stopped| {
                vec![(PathBuf::from("/Volumes/scratch/ballast"), 18 * GIB)]
            }),
        );
        admit_on(
            run,
            &their_ledger,
            &*theirs,
            Admit::Wait,
            Pace {
                poll: Duration::from_millis(5),
                line_every: Duration::from_secs(60),
            },
            &mut relief,
            &mut |l| {
                let _ = tx.send(l.to_owned());
            },
        )
    });
    let figure = rx.recv_timeout(MUST_HAPPEN).expect("the claim's figure");
    names(&figure, &[&first_run().to_string()]);
    let reclaimed = rx.recv_timeout(MUST_HAPPEN).expect("the reclaim's line");
    names(
        &reclaimed,
        &[
            "/Volumes/scratch",
            "2 GiB",
            "gone/.cargo-build",
            "; 1 entry of /Volumes/scratch/left/.cargo-build.removing stayed: tmp/d",
        ],
    );
    let asked = rx.recv_timeout(MUST_HAPPEN).expect("the watch's line");
    names(&asked, &["31 GiB", "why=no-room"]);
    let wait = rx.recv_timeout(MUST_HAPPEN).expect("the wait line");
    names(
        &wait,
        &[
            "verify lint -p aterm-capacity",
            "27 GiB",
            "18 GiB",
            "/Volumes/scratch/ballast",
        ],
    );
    assert_eq!(*order.lock().expect("order"), ["reclaim", "ask"]);
    assert!(!waiting.is_finished(), "it waits instead of starting short");
    m.set(image(40, 36));
    let admitted = joined(waiting, "the run's admission once room came").expect("admitted");
    assert_eq!(
        (admitted.estimate.bytes, admitted.estimate.runs),
        (27 * GIB, 0),
        "the cold budget"
    );
    let target = dir.path().join("target");
    std::fs::create_dir_all(target.join("build.noindex")).expect("a build");
    std::fs::write(target.join("build.noindex/lib.rlib"), vec![7u8; 1 << 20]).expect("written");
    let took = admitted.finish();
    assert!(took.grew >= 1 << 20, "{took:?}");
    assert_eq!(took.dirs, Some(dirs_tag(&verify_run(dir.path()).dirs)));
    assert_eq!(Footprints::load(&record).runs(), &[took]);

    let mut never = Reclaim::of(
        Box::new(|_, _| panic!("a volume with room is reclaimed")),
        Box::new(|_, _, _| panic!("a volume with room is asked for")),
        Box::new(|_, _| panic!("a volume with room names its holders")),
    );
    let mut said = Vec::new();
    let next = admit_on(
        verify_run(dir.path()),
        &ledger,
        &*m,
        Admit::Wait,
        Pace::default(),
        &mut never,
        &mut |l| said.push(l.to_owned()),
    )
    .expect("room");
    assert!(said.is_empty(), "no line with room: {said:?}");
    assert_eq!(next.estimate.runs, 1, "{}", next.estimate);
    assert!(next.estimate.bytes < GIB, "{}", next.estimate);
    let _ = next.finish();
}

/// A need above what the volume holds over its floor is refused at once, naming the need and
/// the volume, and nothing is reclaimed: waiting could never end. The one line said is why the
/// figure is what it is.
#[test]
fn a_need_the_volume_cannot_hold_is_refused_at_once() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-disk-never-")
        .tempdir()
        .expect("scratch");
    let mut never = Reclaim::of(
        Box::new(|_, _| panic!("reclaimed for a need that can never fit")),
        Box::new(|_, _, _| panic!("asked for a need that can never fit")),
        Box::new(|_, _| panic!("named holders for a need that can never fit")),
    );
    let mut said = Vec::new();
    let refused = admit_on(
        verify_run(dir.path()),
        &Ledger::at(dir.path().join("ledger")),
        &Fixed::new(image(20, 19)),
        Admit::Wait,
        Pace::default(),
        &mut never,
        &mut |l| said.push(l.to_owned()),
    );
    match refused {
        Err(aterm_capacity::Error::Never(why)) => {
            names(&why, &["27 GiB", "/Volumes/scratch", "16 GiB"]);
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(said.len(), 1, "the figure's why alone: {said:?}");
    names(&said[0], &[&first_run().to_string()]);
}

/// A claim that says anything says first why its figure is what it is, since a wait line, or
/// `--admit now`'s, names the figure alone: a record whose run peaked at 300 GiB on directories
/// that hold next to nothing now claims the 27 GiB cold budget, capped (a peak beyond it held
/// more than one run writes), and started short by `--admit now` on a volume 16 GiB above its
/// floor its first line is that figure's why, naming the budget's cap, its second what it
/// started short of. Negative control: with the figure's line gone, the first line said is
/// `--admit now`'s, which names 27 GiB and no why.
#[test]
fn a_claim_says_why_its_figure_is_what_it_is_before_it_starts_short() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-disk-why-")
        .tempdir()
        .expect("scratch");
    let run = verify_run(dir.path());
    let mut stale = Footprints::default();
    stale.push(super::Footprint {
        peak: 300 * GIB,
        grew: 0,
        at_ms: 1,
        dirs: None,
    });
    stale.save(&run.record).expect("the record");
    let mut never = Reclaim::of(
        Box::new(|_, _| panic!("reclaimed for a run --admit now starts")),
        Box::new(|_, _, _| panic!("asked for a run --admit now starts")),
        Box::new(|_, _| panic!("named holders for a run --admit now starts")),
    );
    let mut said = Vec::new();
    let started = admit_on(
        run,
        &Ledger::at(dir.path().join("ledger")),
        &Fixed::new(image(40, 20)),
        Admit::Now,
        Pace::default(),
        &mut never,
        &mut |l| said.push(l.to_owned()),
    )
    .expect("started at once");
    let figure = started.estimate;
    assert_eq!(
        (figure.bytes, figure.runs, figure.capped),
        (27 * GIB, 1, true),
        "{figure}"
    );
    assert_eq!(said.len(), 2, "{said:?}");
    names(&said[0], &[&figure.to_string()]);
    names(&said[1], &["--admit now", "27 GiB", "16 GiB"]);
    let _ = started.finish();
}

/// The window's disk watch is asked for the floor, what the claims ahead reserve and the run's
/// need, never for the need of a waiter ahead that no claim's end can admit: that waiter needs
/// 30 GiB on a volume 16 GiB above its floor and reserves nothing, so the run's ask is the
/// 31 GiB its own 27 GiB and the 4 GiB floor make, not 61 GiB, and its wait line says how to
/// start it now. Once that waiter has left and room comes, the run is admitted.
#[test]
fn the_watch_is_not_asked_for_a_waiter_no_claims_end_can_admit() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-disk-unservable-")
        .tempdir()
        .expect("scratch");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let m = Arc::new(Fixed::new(image(40, 20)));
    let ahead = aterm_capacity::Waiter::enter(
        &ledger,
        &*m,
        aterm_capacity::Need::new("verify cells").disk(30 * GIB, "/Volumes/scratch"),
    )
    .expect("a waiter ahead");
    let asked = Arc::new(Mutex::new(Vec::<u64>::new()));
    let (tx, rx) = mpsc::channel::<String>();
    let run = verify_run(dir.path());
    let (theirs, their_asked, their_ledger) = (Arc::clone(&m), Arc::clone(&asked), ledger.clone());
    let waiting = std::thread::spawn(move || {
        let mut relief = Reclaim::of(
            Box::new(|_, _| (0, Vec::new(), Vec::new())),
            Box::new(move |bytes, _, _| {
                their_asked.lock().expect("asked").push(bytes);
                Some("OK why=spent".to_owned())
            }),
            Box::new(|_, _| Vec::new()),
        );
        admit_on(
            run,
            &their_ledger,
            &*theirs,
            Admit::Wait,
            Pace {
                poll: Duration::from_millis(5),
                line_every: Duration::from_secs(60),
            },
            &mut relief,
            &mut |l| {
                let _ = tx.send(l.to_owned());
            },
        )
        .map(|a| a.estimate.bytes)
    });
    let figure = rx.recv_timeout(MUST_HAPPEN).expect("the claim's figure");
    names(&figure, &[&first_run().to_string()]);
    let reclaimed = rx.recv_timeout(MUST_HAPPEN).expect("the reclaim's line");
    names(&reclaimed, &["/Volumes/scratch", "nothing unused"]);
    let ask = rx.recv_timeout(MUST_HAPPEN).expect("the watch's line");
    names(&ask, &["31 GiB", "why=spent"]);
    let wait = rx.recv_timeout(MUST_HAPPEN).expect("the wait line");
    names(
        &wait,
        &[
            "verify lint -p aterm-capacity",
            "27 GiB",
            "16 GiB",
            "4 GiB",
            "0 GiB",
            "--admit now",
        ],
    );
    assert_eq!(*asked.lock().expect("asked"), [31 * GIB]);
    drop(ahead);
    m.set(image(40, 36));
    let claimed = joined(waiting, "the run's admission once room came").expect("admitted");
    assert_eq!(claimed, 27 * GIB);
}

/// A kind never measured here claims its cold budget less what its directories hold already:
/// a warm build directory with no record, holding more than the budget, claims nothing, so a
/// short volume neither reclaims nor waits for it, and that run's own growth is kept for the
/// next. Negative control: the same run on empty directories claims the whole budget, and on
/// that short volume waits for it.
#[test]
fn warm_directories_with_no_record_claim_only_the_rest_of_the_budget() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-disk-warm-")
        .tempdir()
        .expect("scratch");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let mut run = verify_run(dir.path());
    run.cold = 2 << 20;
    let build = dir.path().join("target/build.noindex");
    std::fs::create_dir_all(&build).expect("a warm build directory");
    std::fs::write(build.join("old.rlib"), vec![7u8; 3 << 20]).expect("written");
    let m = Fixed::new(image(40, 4));
    let mut never = Reclaim::of(
        Box::new(|_, _| panic!("reclaimed for a run that claims nothing")),
        Box::new(|_, _, _| panic!("asked for a run that claims nothing")),
        Box::new(|_, _| panic!("named holders for a run that claims nothing")),
    );
    let mut said = Vec::new();
    let warm = admit_on(
        run.clone(),
        &ledger,
        &m,
        Admit::Wait,
        Pace::default(),
        &mut never,
        &mut |l| said.push(l.to_owned()),
    )
    .expect("nothing to wait for");
    assert_eq!((warm.estimate.bytes, warm.estimate.runs), (0, 0));
    assert!(warm.estimate.holds >= 3 << 20, "{:?}", warm.estimate);
    assert!(said.is_empty(), "{said:?}");
    assert!(ledger.list().expect("listed").is_empty(), "no claim made");
    std::fs::write(build.join("new.rlib"), vec![7u8; 1 << 20]).expect("written");
    let took = warm.finish();
    assert!(took.grew >= 1 << 20, "{took:?}");
    assert_eq!(Footprints::load(&run.record).runs(), &[took]);

    std::fs::remove_dir_all(dir.path().join("target")).expect("emptied");
    let run = verify_run(dir.path());
    let (tx, rx) = mpsc::channel::<String>();
    let (theirs, their_ledger) = (Arc::new(m), ledger.clone());
    let waiting = {
        let theirs = Arc::clone(&theirs);
        std::thread::spawn(move || {
            let mut relief = Reclaim::of(
                Box::new(|_, _| (0, Vec::new(), Vec::new())),
                Box::new(|_, _, _| None),
                Box::new(|_, _| Vec::new()),
            );
            admit_on(
                run,
                &their_ledger,
                &*theirs,
                Admit::Wait,
                Pace {
                    poll: Duration::from_millis(5),
                    line_every: Duration::from_secs(60),
                },
                &mut relief,
                &mut |l| {
                    let _ = tx.send(l.to_owned());
                },
            )
        })
    };
    let figure = rx.recv_timeout(MUST_HAPPEN).expect("the claim's figure");
    names(&figure, &[&first_run().to_string()]);
    let line = rx.recv_timeout(MUST_HAPPEN).expect("the relief's line");
    names(&line, &["/Volumes/scratch"]);
    let wait = rx.recv_timeout(MUST_HAPPEN).expect("the wait line");
    names(&wait, &["27 GiB"]);
    theirs.set(image(40, 36));
    let cold = joined(waiting, "the run's admission once room came").expect("admitted");
    assert_eq!((cold.estimate.bytes, cold.estimate.runs), (27 * GIB, 0));
    let _ = cold.finish();
}

/// A run whose record holds one warm run that grew 64 KiB, its build directory under `dir`
/// holding 8 MiB, and that record: what it claims while the directory is there.
fn warm_run(dir: &Path) -> (Run, PathBuf, Footprints, aterm_capacity::Estimate) {
    let run = verify_run(dir);
    let build = dir.join("target/build.noindex");
    std::fs::create_dir_all(&build).expect("a warm build directory");
    std::fs::write(build.join("lib.rlib"), vec![7u8; 8 << 20]).expect("written");
    let mut warm = Footprints::default();
    warm.push(super::Footprint {
        peak: (8 << 20) + (64 << 10),
        grew: 64 << 10,
        at_ms: 1,
        dirs: Some(dirs_tag(&run.dirs)),
    });
    warm.save(&run.record).expect("the record");
    let then = warm.need(
        aterm_capacity::allocated(&run.dirs),
        27 * GIB,
        dirs_tag(&run.dirs),
    );
    assert!(then.bytes < 1 << 20, "{then}");
    (run, build, warm, then)
}

/// A CLAIM IS SIZED AGAIN AFTER A RELIEF THAT EMPTIED THE RUN'S DIRECTORIES. A run whose record
/// holds one warm run that grew 64 KiB, its build directory holding 8 MiB, claims 64 KiB on a
/// volume at its floor; the relief removes that build directory, as the window's watch's pass
/// takes one unwritten for 30 days. Measured again, the claim is what the record claims of the
/// emptied directories (the 8 MiB written again), said after the relief's lines and held in the
/// ledger; and once the run writes its 8 MiB back, its record says it grew by them, measured
/// from empty. Both ways a relief empties them are run: its pass, which says what it removed,
/// and the watch it asked. The admission runs on a thread of its own, joined within the hang
/// detector. Negative control: claimed and metered by the measure taken before the relief, it
/// holds 64 KiB and records a growth of 0.
#[test]
fn a_relief_that_empties_the_runs_directories_sizes_its_claim_again() {
    for by_watch in [false, true] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("admit-disk-emptied-")
            .tempdir()
            .expect("scratch");
        let ledger = Ledger::at(dir.path().join("ledger"));
        let (run, build, warm, then) = warm_run(dir.path());
        let (record, dirs) = (run.record.clone(), run.dirs.clone());

        let m = Arc::new(Fixed::new(image(40, 4)));
        let (pass_m, ask_m) = (Arc::clone(&m), Arc::clone(&m));
        let (pass_build, ask_build) = (build.clone(), build.clone());
        let (their_ledger, theirs) = (ledger.clone(), Arc::clone(&m));
        let waiting = std::thread::spawn(move || {
            let mut relief = Reclaim::of(
                Box::new(move |_mount, _stopped| {
                    if by_watch {
                        return (0, Vec::new(), Vec::new());
                    }
                    std::fs::remove_dir_all(&pass_build).expect("the pass removed it");
                    pass_m.set(image(40, 20));
                    (8 << 20, vec![pass_build.clone()], Vec::new())
                }),
                Box::new(move |_bytes, _mount, _stopped| {
                    if !by_watch {
                        return None;
                    }
                    std::fs::remove_dir_all(&ask_build).expect("the watch's pass removed it");
                    ask_m.set(image(40, 20));
                    Some("OK why=freed".to_owned())
                }),
                Box::new(|_, _| Vec::new()),
            );
            let mut said = Vec::new();
            let admitted = admit_on(
                run,
                &their_ledger,
                &*theirs,
                Admit::Wait,
                Pace {
                    poll: Duration::from_millis(5),
                    line_every: Duration::from_secs(60),
                },
                &mut relief,
                &mut |l| said.push(l.to_owned()),
            );
            (admitted, said)
        });
        let (admitted, said) = joined(waiting, "its admission once the relief freed room");
        let admitted = admitted.expect("admitted once the relief freed room");
        let way = if by_watch { "the watch" } else { "the pass" };
        assert!(!build.exists(), "{way}");
        let now = warm.need(aterm_capacity::allocated(&dirs), 27 * GIB, dirs_tag(&dirs));
        assert!(now.bytes >= 8 << 20, "{way}: {now}");
        assert_eq!(admitted.estimate, now, "{way}: {said:?}");
        let held = ledger.list().expect("listed");
        assert_eq!(held.len(), 1, "{way}: {held:?}");
        assert_eq!(held[0].need.disk, now.bytes, "{way}: the claim holds it");
        names(&said[0], &[&then.to_string()]);
        let resized = said.last().expect("lines said");
        names(resized, &[&now.to_string()]);
        assert_eq!(said.len(), 3 + usize::from(by_watch), "{way}: {said:?}");

        std::fs::create_dir_all(&build).expect("written again");
        std::fs::write(build.join("lib.rlib"), vec![7u8; 8 << 20]).expect("written");
        let took = admitted.finish();
        assert!(took.grew >= 8 << 20, "{way}: grew from empty: {took:?}");
        assert_eq!(
            Footprints::load(&record).runs().last(),
            Some(&took),
            "{way}"
        );
    }
}

/// A CLAIM IS SIZED AGAIN AS IT IS ADMITTED, AFTER ITS DIRECTORIES WENT WHILE IT WAITED. The same
/// warm run claims 64 KiB on a volume at its floor; its relief removes nothing and no watch
/// answers, so it waits. Then a pass that is not its own (another waiting run's, which keeps
/// only that run's directories, or the window's watch's each minute) removes its build directory
/// and room comes: at the look that admits it the claim is sized again from the emptied
/// directories, says so, and is held at that figure in the ledger, and the run's writes are
/// measured from empty. Negative control: sized only after its own relief, it is admitted
/// claiming 64 KiB and records a growth of 0 (DISK4-3's figures).
#[test]
fn a_build_directory_emptied_while_it_waits_sizes_its_claim_again() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-disk-emptied-waiting-")
        .tempdir()
        .expect("scratch");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let (run, build, warm, then) = warm_run(dir.path());
    let (record, dirs) = (run.record.clone(), run.dirs.clone());
    let m = Arc::new(Fixed::new(image(40, 4)));
    let (tx, rx) = mpsc::channel::<String>();
    let (their_ledger, theirs) = (ledger.clone(), Arc::clone(&m));
    let waiting = std::thread::spawn(move || {
        let mut relief = Reclaim::of(
            Box::new(|_mount, _stopped| (0, Vec::new(), Vec::new())),
            Box::new(|_bytes, _mount, _stopped| None),
            Box::new(|_, _| Vec::new()),
        );
        admit_on(
            run,
            &their_ledger,
            &*theirs,
            Admit::Wait,
            Pace {
                poll: Duration::from_millis(5),
                line_every: Duration::from_secs(60),
            },
            &mut relief,
            &mut |l| {
                let _ = tx.send(l.to_owned());
            },
        )
    });
    let figure = rx.recv_timeout(MUST_HAPPEN).expect("the claim's figure");
    names(&figure, &[&then.to_string()]);
    let _nothing = rx.recv_timeout(MUST_HAPPEN).expect("the relief's line");
    let wait = rx.recv_timeout(MUST_HAPPEN).expect("the wait line");
    names(&wait, &["verify lint -p aterm-capacity"]);

    std::fs::remove_dir_all(&build).expect("another pass removed it");
    m.set(image(40, 20));
    let admitted = joined(waiting, "its admission once room came").expect("admitted");
    let now = warm.need(aterm_capacity::allocated(&dirs), 27 * GIB, dirs_tag(&dirs));
    assert!(now.bytes >= 8 << 20, "{now}");
    assert_eq!(admitted.estimate, now);
    let resized = rx
        .recv_timeout(MUST_HAPPEN)
        .expect("the line sizing it again");
    names(&resized, &[&now.to_string()]);
    let held = ledger.list().expect("listed");
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(held[0].need.disk, now.bytes, "the claim holds it");

    std::fs::create_dir_all(&build).expect("written again");
    std::fs::write(build.join("lib.rlib"), vec![7u8; 8 << 20]).expect("written");
    let took = admitted.finish();
    assert!(took.grew >= 8 << 20, "grew from empty: {took:?}");
    assert_eq!(Footprints::load(&record).runs().last(), Some(&took));
}

/// THE RUN'S OWN PASS KEEPS ITS DIRECTORIES AND FREES THE REST ([`super::recorder`]): over a
/// scratch cache, 31 days on, when every build directory there is unwritten long enough to go
/// whatever its workspace's state, the pass keeps the run's own build directory (named through
/// a link) and one holding another of its directories, and removes the third, as it would
/// with nothing kept. Negative control: with no run directories the same pass removes both;
/// once an interrupt came it removes nothing.
#[test]
fn the_runs_own_pass_keeps_its_directories_and_frees_the_rest() {
    use crate::build_cache::{RECLAIM_GONE_UNDER_PRESSURE, build_dir, cache_root, reclaim_watched};
    let dir = crate::testdir::dir("admit-disk-own-pass");
    let home = dir.join("home");
    let root = cache_root(&home);
    let lay = |name: &str| {
        let leaf = build_dir(&root, &dir.join(name));
        std::fs::create_dir_all(leaf.join("debug")).expect("a build directory");
        std::fs::write(leaf.join("debug/x"), b"0123456789").expect("written");
        leaf
    };
    let (own, holding, other) = (lay("ws-own"), lay("ws-holding"), lay("ws-other"));
    #[cfg(unix)]
    let named = {
        let link = dir.join("cache-link");
        std::os::unix::fs::symlink(&root, &link).expect("a link to the cache");
        link.join(own.strip_prefix(&root).expect("under the cache"))
    };
    #[cfg(not(unix))]
    let named = own.clone();
    let later = std::time::SystemTime::now() + Duration::from_secs(31 * 24 * 60 * 60);
    let pass = |own: &[PathBuf], halt: bool| {
        let halt = std::sync::atomic::AtomicBool::new(halt);
        let mut removed = Vec::new();
        let went = reclaim_watched(
            &root,
            Some(&home),
            later,
            RECLAIM_GONE_UNDER_PRESSURE,
            None,
            &mut super::recorder(own, &halt, &mut removed),
        );
        let mut went: Vec<PathBuf> = went.into_iter().map(|(p, _)| p).collect();
        went.sort();
        removed.sort();
        assert_eq!(went, removed, "what went is what was let go");
        removed
    };

    assert_eq!(
        pass(&[named, holding.join("debug")], false),
        std::slice::from_ref(&other)
    );
    assert!(own.is_dir() && holding.is_dir() && !other.exists());

    let again = lay("ws-other");
    assert_eq!(pass(&[], true), Vec::<PathBuf>::new(), "an interrupt came");
    let mut want = vec![own.clone(), holding.clone(), again];
    want.sort();
    assert_eq!(pass(&[], false), want);
    assert!(!own.exists() && !holding.exists());
}

/// Set, the test binary is [`standin_claims_disk_until_interrupted`], its ledger and run under
/// this directory.
#[cfg(unix)]
const STANDIN: &str = "ATPKG_ADMIT_DISK_STANDIN";
/// Set, the stand-in's relief asks the window's watch and the ask lasts until the wait is
/// stopped, as `aterm ctl await sys` lasts up to a minute; else the ask answers at once and the
/// stand-in is signalled in its wait.
#[cfg(unix)]
const STANDIN_ASK: &str = "ATPKG_ADMIT_DISK_STANDIN_ASK";
/// Set, the interrupt comes while the stand-in's directories are measured, before its first
/// look, for a run that would claim nothing (no record, no cold budget): it sends itself SIGTERM
/// under a watch of its own, as a caller's wait would hold one, so the signal is noted before
/// the measuring looks, holding that point of the race.
#[cfg(unix)]
const STANDIN_MEASURING: &str = "ATPKG_ADMIT_DISK_STANDIN_MEASURING";

/// The signal the stand-in's own handler noted.
#[cfg(unix)]
static TOOL_SAW: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

#[cfg(unix)]
extern "C" fn tool_handler(sig: libc::c_int) {
    TOOL_SAW.store(sig, std::sync::atomic::Ordering::SeqCst);
}

/// Run by [`an_interrupt_ends_a_disk_claim_and_its_relief`] in a process of its own: catches
/// SIGINT and SIGTERM in a handler of its own, as `ship trust run` and the lab do, then claims a
/// first verify run's 27 GiB on a volume 16 GiB above its floor, looking every two minutes
/// (twice the test's hang detector, so only a nap the interrupt ends ends in time), writing
/// `waiting` once its wait (or its relief's ask) is under way, and exits 3, a tool's NOT RUN,
/// naming the signal and what it was short of.
#[cfg(unix)]
#[test]
fn standin_claims_disk_until_interrupted() {
    let Some(dir) = std::env::var_os(STANDIN).map(PathBuf::from) else {
        return;
    };
    // SAFETY: `tool_handler` is an `extern "C" fn(c_int)` that lives for the whole program and
    // stores one atomic, which is async-signal-safe.
    unsafe {
        let tool = tool_handler as extern "C" fn(libc::c_int) as libc::sighandler_t;
        libc::signal(libc::SIGINT, tool);
        libc::signal(libc::SIGTERM, tool);
    }
    let measuring = std::env::var_os(STANDIN_MEASURING).is_some();
    let outer = measuring.then(|| {
        let watch = aterm_toolchain::interrupt::Watch::enter();
        // SAFETY: `kill` and `getpid` take and return integers only.
        assert_eq!(unsafe { libc::kill(libc::getpid(), libc::SIGTERM) }, 0);
        aterm_spec::wait::until_within(MUST_HAPPEN, "the SIGTERM noted", || watch.caught());
        watch
    });
    let mut run = verify_run(&dir);
    if measuring {
        run.cold = 0;
    }
    let waiting = dir.join("waiting");
    let ask_lasts = std::env::var_os(STANDIN_ASK).is_some();
    let asking = waiting.clone();
    let mut relief = Reclaim::of(
        Box::new(|_, _| (0, Vec::new(), Vec::new())),
        Box::new(move |_, _, stopped| {
            if ask_lasts {
                std::fs::write(&asking, "asking").expect("said asking");
                let started = std::time::Instant::now();
                while !stopped() {
                    assert!(started.elapsed() < MUST_HAPPEN, "the ask was never stopped");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            None
        }),
        Box::new(|_, _| Vec::new()),
    );
    let got = admit_on(
        run,
        &Ledger::at(dir.join("ledger")),
        &Fixed::new(image(40, 20)),
        Admit::Wait,
        Pace {
            poll: MUST_HAPPEN * 2,
            ..Pace::default()
        },
        &mut relief,
        &mut |line| {
            if line.contains(" waits ") {
                std::fs::write(&waiting, line).expect("said waiting");
            }
        },
    );
    drop(outer);
    match got {
        Err(
            e @ aterm_capacity::Error::Stopped {
                signal: Some(sig), ..
            },
        ) => {
            assert_eq!(
                TOOL_SAW.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "the wait took the signal"
            );
            println!("stopped by {}: {e}", aterm_toolchain::interrupt::name(sig));
            std::process::exit(3);
        }
        other => panic!("the claim ended otherwise: {other:?}"),
    }
}

/// A stand-in, killed when it is dropped still running (a test that failed before it ended),
/// so none outlives its test: one with its own handler naps on past its parent's detector.
#[cfg(unix)]
struct Standin(aterm_uds::spawnfd::Child);

#[cfg(unix)]
impl Drop for Standin {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// A disk claim held up by a short volume ends on SIGTERM, or SIGINT, as NOT RUN well inside
/// the one-minute hang detector (the time is reported), whether the signal comes while it waits
/// (its next look two minutes off) or while its relief asks the window's watch, which would
/// last a minute; it names the signal and the need, and leaves nothing in the ledger. Negative
/// control: with the wait unwatched (the claim before WAIT-INTERRUPT), the stand-in's own
/// handler takes the signal and the claim waits on, and this test runs to its detector.
#[cfg(unix)]
#[test]
fn an_interrupt_ends_a_disk_claim_and_its_relief() {
    for (ask, sig, word) in [
        (false, libc::SIGTERM, "SIGTERM"),
        (true, libc::SIGTERM, "SIGTERM"),
        (false, libc::SIGINT, "SIGINT"),
    ] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("admit-disk-int-")
            .tempdir()
            .expect("scratch");
        let mut child = standin(dir.path(), if ask { &[STANDIN_ASK] } else { &[] });
        let said = aterm_spec::wait::until_within(MUST_HAPPEN, "the stand-in's wait", || {
            std::fs::read_to_string(dir.path().join("waiting")).ok()
        });
        let pid = i32::try_from(child.0.id()).expect("a pid");
        let sent = std::time::Instant::now();
        // SAFETY: `kill` takes two integers; the pid is this test's own child.
        assert_eq!(unsafe { libc::kill(pid, sig) }, 0, "signalled the stand-in");
        let status = aterm_spec::wait::until_within(MUST_HAPPEN, "the stand-in's end", || {
            child.0.try_wait().ok().flatten()
        });
        let took = sent.elapsed();
        let mut out = String::new();
        if let Some(mut o) = child.0.stdout.take() {
            let _ = std::io::Read::read_to_string(&mut o, &mut out);
        }
        let what = format!("{word} {}", if ask { "in the ask" } else { "in the wait" });
        assert_eq!(
            status.code(),
            Some(3),
            "{what}: {status} {out} (said {said})"
        );
        names(
            &out,
            &[&format!("stopped by {word}: "), "disk short by", "27 GiB"],
        );
        let left = left_in(dir.path());
        assert!(left.is_empty(), "{what} left {left:?}");
        eprintln!("{what} ended the disk claim in {took:?}");
    }
}

/// Start [`standin_claims_disk_until_interrupted`] on `dir` with `env` set.
#[cfg(unix)]
fn standin(dir: &Path, env: &[&str]) -> Standin {
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    for key in env {
        cmd.env(key, "1");
    }
    Standin(
        cmd.args([
            "--exact",
            "admit_disk::tests::standin_claims_disk_until_interrupted",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(STANDIN, dir)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the stand-in"),
    )
}

/// The claims and waits left in the stand-in's ledger under `dir`.
#[cfg(unix)]
fn left_in(dir: &Path) -> Vec<String> {
    Ledger::at(dir.join("ledger"))
        .dir()
        .read_dir()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".claim") || n.ends_with(".wait"))
        .collect()
}

/// The measuring of a run's directories is part of its wait: an interrupt then ends the claim
/// as NOT RUN, before its first look, even for a run that would claim nothing (which takes no
/// look at all), and nothing is left in the ledger. Before, such a run went on unclaimed and
/// the interrupt reached the tool's own handling only once the measuring's watch ended.
#[cfg(unix)]
#[test]
fn an_interrupt_while_the_run_is_measured_ends_its_claim() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-disk-measuring-")
        .tempdir()
        .expect("scratch");
    let mut child = standin(dir.path(), &[STANDIN_MEASURING]);
    let status = aterm_spec::wait::until_within(MUST_HAPPEN, "the stand-in's end", || {
        child.0.try_wait().ok().flatten()
    });
    let mut out = String::new();
    if let Some(mut o) = child.0.stdout.take() {
        let _ = std::io::Read::read_to_string(&mut o, &mut out);
    }
    assert_eq!(status.code(), Some(3), "{status} {out}");
    names(&out, &["stopped by SIGTERM: ", "before its first look"]);
    let left = left_in(dir.path());
    assert!(left.is_empty(), "left {left:?}");
}

/// The build cache's pass runs on a thread of its own, so an interrupt ends the relief while a
/// step of the pass is under way, as a large build directory's removal can be: the relief
/// returns without it (the time from the interrupt is reported), and the pass, once its step
/// ends, is told to remove nothing more. The step here lasts until the test lets it go, after
/// the relief has returned; waiting for the pass instead, the relief would run to the hang
/// detector. A pass no interrupt ends returns what it removed.
#[test]
fn an_interrupt_leaves_the_cache_pass_behind() {
    assert_eq!(super::on_thread(&|| false, |_| 7), Some(7));
    let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (stepping_tx, stepping) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let (told_tx, told) = mpsc::channel();
    let (ended_tx, ended) = mpsc::channel();
    let flag = Arc::clone(&interrupted);
    std::thread::spawn(move || {
        let got = super::on_thread(
            &|| flag.load(std::sync::atomic::Ordering::SeqCst),
            move |halt| {
                let _ = stepping_tx.send(());
                let _ = released.recv_timeout(MUST_HAPPEN * 2);
                let _ = told_tx.send(halt.load(std::sync::atomic::Ordering::SeqCst));
            },
        );
        let _ = ended_tx.send(got);
    });
    stepping
        .recv_timeout(MUST_HAPPEN)
        .expect("the pass began a step");
    let sent = std::time::Instant::now();
    interrupted.store(true, std::sync::atomic::Ordering::SeqCst);
    let got = ended
        .recv_timeout(MUST_HAPPEN)
        .expect("the relief ended without its pass");
    let took = sent.elapsed();
    assert_eq!(got, None, "an interrupted pass gives nothing");
    let _ = release.send(());
    assert_eq!(
        told.recv_timeout(MUST_HAPPEN),
        Ok(true),
        "the pass, its step ended, was told to remove nothing more"
    );
    eprintln!("the relief ended {took:?} after the interrupt, its pass mid-step");
}

/// WHAT STAYED IS CARRIED, AND ONLY THAT: of what a pass returned, each stage it could not
/// finish gives one note, with its own count and its first entries as they stand
/// (`build_cache::remove::named`), and a removal whole gives none. NEGATIVE CONTROL: the same
/// stage under another count gives another note.
#[test]
fn a_relief_notes_what_stayed_of_each_stage_it_could_not_finish() {
    use crate::build_cache::{Unused, remove};
    let dir = crate::testdir::dir("admit-disk-stayed-notes");
    let stage = dir.join(".cargo-build.removing");
    std::fs::create_dir_all(stage.join("tmp/d")).expect("the stage");
    let went = [
        (dir.join("gone/.cargo-build"), Unused::Unfinished),
        (
            stage.clone(),
            Unused::Stayed {
                stayed: 2,
                freed: 7,
            },
        ),
    ];
    let notes = super::stayed_notes(&went);
    assert_eq!(notes, [remove::named(&stage, 2).words(&stage)]);
    assert_ne!(notes, [remove::named(&stage, 7).words(&stage)]);
    assert_eq!(remove::named(&stage, 2).first, [PathBuf::from("tmp/d")]);
    assert!(super::stayed_notes(&went[..1]).is_empty());
}

/// A RECORD MEASURED ON OTHER DIRECTORIES IS NOT WHAT A RUN CLAIMS BY: the record holds a run
/// that peaked at 300 GiB on another build root and one that wrote 1 MiB on the run's own
/// directories, each tagged with the directories it measured ([`dirs_tag`]); the run claims the
/// 1 MiB, read from that one run, not the 27 GiB cold budget that caps the old peak. Negative
/// control: tagged with any other directories (or the tag ignored), the claim is the budget.
#[test]
fn a_run_measured_on_other_directories_is_not_what_a_claim_is_read_from() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-disk-tag-")
        .tempdir()
        .expect("scratch");
    let run = verify_run(dir.path());
    let mut kept = Footprints::default();
    kept.push(super::Footprint {
        peak: 300 * GIB,
        grew: 0,
        at_ms: 1,
        dirs: Some(dirs_tag(&[dir.path().join("elsewhere")])),
    });
    kept.push(super::Footprint {
        peak: 1 << 20,
        grew: 1 << 20,
        at_ms: 2,
        dirs: Some(dirs_tag(&run.dirs)),
    });
    kept.save(&run.record).expect("the record");
    let mut never = Reclaim::of(
        Box::new(|_, _| panic!("reclaimed for a run with room")),
        Box::new(|_, _, _| panic!("asked for a run with room")),
        Box::new(|_, _| panic!("named holders for a run with room")),
    );
    let admitted = admit_on(
        run,
        &Ledger::at(dir.path().join("ledger")),
        &Fixed::new(image(40, 36)),
        Admit::Wait,
        Pace::default(),
        &mut never,
        &mut |_| {},
    )
    .expect("room");
    let e = admitted.estimate;
    assert_eq!((e.bytes, e.runs, e.capped), (1 << 20, 1, false), "{e}");
    let _ = admitted.finish();
}

/// A RUN'S DIRECTORIES ARE NAMED BY WHAT THEY ARE, NOT HOW THEY ARE GIVEN: their tag is the same
/// in any order, with one given twice, or through a link to one, and for one not made yet once
/// it is made, two levels deep too (a new worktree's `<target>/verify` before `<target>` is
/// made); other directories have another; and the digest is the stable FNV-1a of the canonical
/// paths, sorted and each ended by a NUL, so a record one build writes is read the same by the
/// next. Negative controls: std's `DefaultHasher` in its place gives another figure; with the
/// names not yet made appended in the order they were met (`canonical` without its `rev`),
/// the deep one's tag changes once it is made.
#[test]
fn a_runs_directories_are_tagged_by_what_they_are() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-disk-dirs-")
        .tempdir()
        .expect("scratch");
    let real = dir
        .path()
        .canonicalize()
        .expect("the scratch's own spelling");
    let (a, b) = (real.join("a"), real.join("b"));
    let tag = dirs_tag(&[a.clone(), b.clone()]);
    assert_eq!(dirs_tag(&[b.clone(), a.clone()]), tag, "in any order");
    assert_eq!(
        dirs_tag(&[a.clone(), b.clone(), a.clone()]),
        tag,
        "one twice"
    );
    std::fs::create_dir_all(&a).expect("made");
    assert_eq!(dirs_tag(&[a.clone(), b.clone()]), tag, "once made");
    let deep = real.join("x").join("y");
    let unmade = dirs_tag(std::slice::from_ref(&deep));
    std::fs::create_dir_all(&deep).expect("made");
    assert_eq!(
        dirs_tag(std::slice::from_ref(&deep)),
        unmade,
        "two levels deep, once made"
    );
    #[cfg(unix)]
    {
        let link = real.join("link");
        std::os::unix::fs::symlink(&a, &link).expect("a link");
        assert_eq!(dirs_tag(&[link, b.clone()]), tag, "through a link");
    }
    assert_ne!(
        dirs_tag(&[a.clone(), real.join("c")]),
        tag,
        "other directories"
    );
    let mut bytes = Vec::new();
    for p in [&a, &b] {
        bytes.extend_from_slice(p.as_os_str().as_encoded_bytes());
        bytes.push(0);
    }
    assert_eq!(tag, aterm_hash::fnv1a_64(&bytes), "the stable digest");
}
