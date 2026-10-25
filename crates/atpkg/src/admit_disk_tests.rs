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

use super::{Reclaim, Run, admit_on};

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

/// `line` names each of `figures`.
#[track_caller]
fn names(line: &str, figures: &[&str]) {
    for f in figures {
        assert!(line.contains(f), "{line:?} does not name {f:?}");
    }
}

/// A first run claims the 27 GiB cold budget. On a volume 18 GiB above its floor it reclaims
/// first, then asks the window's watch, and only then waits, its line naming the need, the free
/// space and the largest holder; it is admitted once room comes, and what it wrote is kept.
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
            Box::new(move |mount| {
                assert_eq!(mount, Path::new("/Volumes/scratch"));
                pass_order.lock().expect("order").push("reclaim");
                pass_m.set(image(40, 22));
                (
                    2 * GIB,
                    vec![PathBuf::from("/Volumes/scratch/gone/.cargo-build")],
                )
            }),
            Box::new(move |bytes, _mount| {
                ask_order.lock().expect("order").push("ask");
                assert_eq!(bytes, 31 * GIB, "the floor and the need");
                Some("OK why=no-room".to_owned())
            }),
            Box::new(|_mount| vec![(PathBuf::from("/Volumes/scratch/ballast"), 18 * GIB)]),
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
    let reclaimed = rx.recv_timeout(MUST_HAPPEN).expect("the reclaim's line");
    names(
        &reclaimed,
        &["/Volumes/scratch", "2 GiB", "gone/.cargo-build"],
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
    let admitted = waiting.join().expect("the run").expect("admitted");
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
    assert_eq!(Footprints::load(&record).runs(), &[took]);

    let mut never = Reclaim::of(
        Box::new(|_| panic!("a volume with room is reclaimed")),
        Box::new(|_, _| panic!("a volume with room is asked for")),
        Box::new(|_| panic!("a volume with room names its holders")),
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
/// the volume, and nothing is reclaimed: waiting could never end.
#[test]
fn a_need_the_volume_cannot_hold_is_refused_at_once() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-disk-never-")
        .tempdir()
        .expect("scratch");
    let mut never = Reclaim::of(
        Box::new(|_| panic!("reclaimed for a need that can never fit")),
        Box::new(|_, _| panic!("asked for a need that can never fit")),
        Box::new(|_| panic!("named holders for a need that can never fit")),
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
    assert!(said.is_empty(), "{said:?}");
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
        Box::new(|_| panic!("reclaimed for a run that claims nothing")),
        Box::new(|_, _| panic!("asked for a run that claims nothing")),
        Box::new(|_| panic!("named holders for a run that claims nothing")),
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
                Box::new(|_| (0, Vec::new())),
                Box::new(|_, _| None),
                Box::new(|_| Vec::new()),
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
    let line = rx.recv_timeout(MUST_HAPPEN).expect("the relief's line");
    names(&line, &["/Volumes/scratch"]);
    let wait = rx.recv_timeout(MUST_HAPPEN).expect("the wait line");
    names(&wait, &["27 GiB"]);
    theirs.set(image(40, 36));
    let cold = waiting.join().expect("the run").expect("admitted");
    assert_eq!((cold.estimate.bytes, cold.estimate.runs), (27 * GIB, 0));
    let _ = cold.finish();
}
