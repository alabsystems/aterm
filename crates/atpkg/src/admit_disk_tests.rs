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

use super::{Reclaim, Run, admit_on, dirs_tag, scope_tag, size};
use crate::joined;

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

/// What [`verify_run`] builds ([`Run::scope`]).
const SCOPE: &str = "lint -p aterm-capacity";

/// A verify run whose directories are under `dir`.
fn verify_run(dir: &Path) -> Run {
    let target = dir.join("target");
    std::fs::create_dir_all(target.join("verify")).expect("the run's corner");
    Run {
        what: "verify lint -p aterm-capacity".to_owned(),
        scope: SCOPE.to_owned(),
        dirs: vec![target.join("build.noindex"), target.join("verify")],
        record: target.join("verify/footprint.json"),
        cold: 27 * GIB,
        on: PathBuf::from("/Volumes/scratch"),
        stuck_ends: None,
        scratch: false,
        machine: None,
    }
}

/// What a first [`verify_run`] on empty directories claims, as its first line says it: with no
/// record, the tag of its directories counts no run, so any tag reads the same.
fn first_run() -> aterm_capacity::Estimate {
    Footprints::default().need(0, 27 * GIB, 0, scope_tag(SCOPE))
}

/// A claim of `gib` GiB on [`image`]'s volume, held at once: what a run beside it is short of.
fn held_beside(ledger: &Ledger, m: &Fixed, gib: u64) -> aterm_capacity::Claim {
    aterm_capacity::admit_with(
        ledger,
        m,
        aterm_capacity::Need::new("held").disk(gib * GIB, "/Volumes/scratch"),
        Admit::Now,
        Pace::default(),
        &mut |_| {},
    )
    .expect("held")
}

/// `line` names each of `figures`.
#[track_caller]
fn names(line: &str, figures: &[&str]) {
    for f in figures {
        assert!(line.contains(f), "{line:?} does not name {f:?}");
    }
}

/// A first run claims the 27 GiB cold budget, and says so first. On a volume 16 GiB above its
/// floor beside a held 5 GiB claim it reclaims first (13 GiB, its line naming what stayed of a
/// stage the pass could not finish), then asks the window's watch for the floor, the held claim
/// and its need, and only then waits, within the room but short of what that claim holds, its
/// line naming the need, the free space, what is reserved and the largest holder; it is admitted
/// once room comes, and what it wrote is kept. Negative control: the next run, with room, is
/// relieved of nothing and says nothing, and it claims what the first run wrote, not the cold
/// budget.
#[test]
fn a_short_volume_is_reclaimed_first_then_waited_for() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-disk-")
        .tempdir()
        .expect("scratch");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let m = Arc::new(Fixed::new(image(40, 20)));
    let held = held_beside(&ledger, &m, 5);
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
            Box::new(move |room: super::Room<'_>, _stopped| {
                assert_eq!(room.mount, Path::new("/Volumes/scratch"));
                pass_order.lock().expect("order").push("reclaim");
                pass_m.set(image(40, 33));
                (
                    13 * GIB,
                    vec![PathBuf::from("/Volumes/scratch/gone/.cargo-build")],
                    vec![
                        "1 entry of /Volumes/scratch/left/.cargo-build.removing stayed: tmp/d"
                            .to_owned(),
                    ],
                )
            }),
            Box::new(move |bytes, _mount, _stopped| {
                ask_order.lock().expect("order").push("ask");
                assert_eq!(bytes, 36 * GIB, "the floor, the held claim and the need");
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
            "13 GiB",
            "gone/.cargo-build",
            "; 1 entry of /Volumes/scratch/left/.cargo-build.removing stayed: tmp/d",
        ],
    );
    let asked = rx.recv_timeout(MUST_HAPPEN).expect("the watch's line");
    names(&asked, &["36 GiB", "why=no-room"]);
    let wait = rx.recv_timeout(MUST_HAPPEN).expect("the wait line");
    names(
        &wait,
        &[
            "verify lint -p aterm-capacity",
            "27 GiB",
            "29 GiB",
            "5 GiB reserved",
            "/Volumes/scratch/ballast",
        ],
    );
    assert_eq!(*order.lock().expect("order"), ["reclaim", "ask"]);
    assert!(!waiting.is_finished(), "it waits instead of starting short");
    m.set(image(40, 40));
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
    drop(held);
}

/// UNMET: a first run's 27 GiB on a volume 16 GiB above its floor, which its relief brings to
/// 18 GiB (the pass reclaims 2 GiB, the watch it asks frees nothing), is refused at the next
/// look, no wait line said: the lines are its figure's why, the reclaim's and the watch's, and
/// the refusal names the run, what it is short of, what holds the most on the volume, how much
/// to free there and that its relief ran (the way to start it at once is its caller's to add).
/// The claim leaves the ledger and nothing is kept. The
/// admission runs on a thread of its own, joined within the hang detector, so a wait fails the
/// test. NEGATIVE CONTROL: [`a_short_volume_is_reclaimed_first_then_waited_for`], within the room
/// after its relief, waits and is admitted.
#[test]
fn a_run_its_relief_leaves_beyond_the_room_is_refused_at_once() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-disk-unmet-")
        .tempdir()
        .expect("scratch");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let m = Arc::new(Fixed::new(image(40, 20)));
    let run = verify_run(dir.path());
    let record = run.record.clone();
    let (theirs, their_ledger) = (Arc::clone(&m), ledger.clone());
    let deciding = std::thread::spawn(move || {
        let pass_m = Arc::clone(&theirs);
        let mut relief = Reclaim::of(
            Box::new(move |_room, _stopped| {
                pass_m.set(image(40, 22));
                (
                    2 * GIB,
                    vec![PathBuf::from("/Volumes/scratch/gone/.cargo-build")],
                    Vec::new(),
                )
            }),
            Box::new(|_bytes, _mount, _stopped| Some("OK why=no-room".to_owned())),
            Box::new(|_mount, _stopped| {
                vec![(PathBuf::from("/Volumes/scratch/ballast"), 18 * GIB)]
            }),
        );
        let mut said = Vec::new();
        let got = admit_on(
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
        (got.map(|a| a.estimate.bytes), said)
    });
    let (got, said) = joined(deciding, "its refusal");
    let refused = match got {
        Err(e @ aterm_capacity::Error::Unmet { .. }) => e.to_string(),
        other => panic!("refused as unmet, got {other:?}"),
    };
    names(
        &refused,
        &[
            "verify lint -p aterm-capacity: disk short by 9 GiB",
            "needs 27 GiB on /Volumes/scratch, 18 GiB free above its 4 GiB floor",
            "largest on /Volumes/scratch: /Volumes/scratch/ballast 18 GiB",
            "Free at least 9 GiB on /Volumes/scratch",
            "once its relief freed what it could",
        ],
    );
    assert_eq!(
        said.len(),
        3,
        "the figure, the reclaim and the ask: {said:?}"
    );
    names(&said[0], &[&first_run().to_string()]);
    names(&said[1], &["2 GiB", "gone/.cargo-build"]);
    names(&said[2], &["31 GiB", "why=no-room"]);
    assert!(said.iter().all(|l| !l.contains(" waits ")), "{said:?}");
    assert!(
        ledger.list().expect("listed").is_empty(),
        "it left the queue"
    );
    assert!(Footprints::load(&record).runs().is_empty(), "nothing kept");
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
        scope: scope_tag(SCOPE),
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
/// 31 GiB its own 27 GiB and the 4 GiB floor make, not 61 GiB. The run, as far beyond the room,
/// is then refused, saying its relief ran. NEGATIVE CONTROL: once that waiter has left and
/// room has come, the same run is admitted at once.
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
    let deciding = std::thread::spawn(move || {
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
    let refused = match joined(deciding, "its refusal") {
        Err(e @ aterm_capacity::Error::Unmet { .. }) => e.to_string(),
        other => panic!("refused as unmet, got {other:?}"),
    };
    names(
        &refused,
        &[
            "verify lint -p aterm-capacity",
            "27 GiB",
            "16 GiB",
            "4 GiB",
            "once its relief freed what it could",
        ],
    );
    assert_eq!(*asked.lock().expect("asked"), [31 * GIB]);
    drop(ahead);
    m.set(image(40, 36));
    let claimed = admit_on(
        verify_run(dir.path()),
        &ledger,
        &*m,
        Admit::Wait,
        Pace::default(),
        &mut no_relief(),
        &mut |_| {},
    )
    .expect("admitted at once");
    assert_eq!(claimed.estimate.bytes, 27 * GIB);
    let _ = claimed.finish();
}

/// A scratch run that removes what it wrote does so while its claim is still held
/// ([`Admitted::finish_emptying`]). Inside `empty` the ledger still says the claim is scratch;
/// after it, the claim is gone, and what the run kept is what its directories grew by before
/// they were emptied. NEGATIVE CONTROL: a claim let go before `empty` ran reads 0 claims inside
/// it, and one that forgot the scratch marker reads `false` inside it.
#[test]
fn a_run_empties_its_scratch_before_its_claim_goes() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-disk-emptying-")
        .tempdir()
        .expect("scratch");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let mut run = verify_run(dir.path());
    run.scratch = true;
    let claimed = admit_on(
        run.clone(),
        &ledger,
        &Fixed::new(image(40, 36)),
        Admit::Wait,
        Pace::default(),
        &mut no_relief(),
        &mut |_| {},
    )
    .expect("admitted at once");
    let build = dir.path().join("target/build.noindex");
    std::fs::create_dir_all(&build).expect("a build directory");
    std::fs::write(build.join("slot.o"), vec![7u8; 3 << 20]).expect("written");
    let mut inside = None;
    let took = claimed.finish_emptying(|| {
        let claims = ledger.list().expect("listed");
        inside = Some((claims.len(), claims[0].need.scratch));
        std::fs::remove_dir_all(&build).expect("emptied");
    });
    assert_eq!(
        inside,
        Some((1, true)),
        "emptied while its scratch claim is held"
    );
    assert!(
        ledger.list().expect("listed").is_empty(),
        "then its claim went"
    );
    assert!(
        took.grew >= 3 << 20,
        "measured before it was emptied: {took:?}"
    );
    assert_eq!(Footprints::load(&run.record).runs(), &[took]);
}

/// A kind never measured here claims its cold budget less what its directories hold already:
/// a warm build directory with no record, holding more than the budget, claims nothing, so a
/// short volume neither reclaims nor waits for it, and that run's own growth is kept for the
/// next. Negative control: the same run on empty directories claims the whole budget, which on
/// that short volume is refused (UNMET, beyond the room once its relief ran).
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
    let deciding = {
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
    match joined(deciding, "its refusal") {
        Err(e @ aterm_capacity::Error::Unmet { .. }) => names(&e.to_string(), &["needs 27 GiB"]),
        other => panic!("refused as unmet, got {other:?}"),
    }
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
        scope: scope_tag(SCOPE),
    });
    warm.save(&run.record).expect("the record");
    let then = warm.need(
        aterm_capacity::allocated(&run.dirs),
        27 * GIB,
        dirs_tag(&run.dirs),
        scope_tag(SCOPE),
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
        let now = warm.need(
            aterm_capacity::allocated(&dirs),
            27 * GIB,
            dirs_tag(&dirs),
            scope_tag(SCOPE),
        );
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
/// warm run claims 64 KiB on a volume 4 GiB above its floor, all of it held by a claim ahead;
/// its relief removes nothing and no watch answers, so it waits for that claim. Then a pass that
/// is not its own (another waiting run's, which keeps only that run's directories, or the
/// window's watch's each minute) removes its build directory
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
    let m = Arc::new(Fixed::new(image(40, 8)));
    let ahead = held_beside(&ledger, &m, 4);
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
    let now = warm.need(
        aterm_capacity::allocated(&dirs),
        27 * GIB,
        dirs_tag(&dirs),
        scope_tag(SCOPE),
    );
    assert!(now.bytes >= 8 << 20, "{now}");
    assert_eq!(admitted.estimate, now);
    let resized = rx
        .recv_timeout(MUST_HAPPEN)
        .expect("the line sizing it again");
    names(&resized, &[&now.to_string()]);
    let held: Vec<_> = ledger
        .list()
        .expect("listed")
        .into_iter()
        .filter(|r| r.need.what == "verify lint -p aterm-capacity")
        .collect();
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(held[0].need.disk, now.bytes, "the claim holds it");
    drop(ahead);

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
    use crate::build_cache::{ReclaimAfter, build_dir, cache_root, reclaim_watched};
    let dir = crate::testdir::dir("admit-disk-own-pass");
    let home = dir.join("home");
    let root = cache_root(&home);
    let lay = |name: &str| {
        let leaf = build_dir(&root, &Path::new("/").join(name));
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
            ReclaimAfter::UNDER_PRESSURE,
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

/// A CLAIM THAT DOES NOT FIT IS NOT A VOLUME BELOW ITS FLOOR: a relief asks for the hour's wait on
/// a live worktree's stale parts only where nothing stands above the floor. Above it, the wait
/// stays three days (a gone workspace's build directory has no wait at either).
#[test]
fn a_claims_relief_waits_the_hour_only_below_the_floor() {
    use crate::build_cache::{RECLAIM_STALE, RECLAIM_STALE_UNDER_PRESSURE};
    let relieved = |above_floor: u64| {
        let seen = std::sync::Arc::new(Mutex::new(None));
        let theirs = std::sync::Arc::clone(&seen);
        let mut relief = Reclaim::of(
            Box::new(move |room: super::Room<'_>, _stopped| {
                *theirs.lock().expect("seen") = Some(room.after);
                (0, Vec::new(), Vec::new())
            }),
            Box::new(|_, _, _| None),
            Box::new(|_, _| Vec::new()),
        );
        let short = aterm_capacity::Shortfall {
            shorts: vec![aterm_capacity::Short::Disk {
                need: 20 * GIB,
                mount: PathBuf::from("/Volumes/scratch"),
                above_floor,
                floor: 10 * GIB,
                reserved: GIB,
                held: Vec::new(),
            }],
            holders: Vec::new(),
        };
        aterm_capacity::Relief::relieve(&mut relief, &short, &mut |_| {}, &|| false);
        let seen = seen.lock().expect("seen").take();
        seen.expect("the pass ran")
    };

    let after = relieved(12 * GIB);
    assert_eq!(after.stale, RECLAIM_STALE);

    let after = relieved(0);
    assert_eq!(after.stale, RECLAIM_STALE_UNDER_PRESSURE);
}

/// A RELIEF IS RENEWED BY A WORKSPACE REMOVED WHILE ITS RUN WAITS: a build directory the gone
/// listing did not hold as its pass began renews it ([`aterm_capacity::Relief::renewed`]), one
/// removed while that pass ran included; one it did hold (a build kept it) does not, and the next
/// pass's listing is the new mark. NEGATIVE CONTROLS: a relief that watches no listing, or whose
/// last call was short of no disk (memory alone), is never renewed, so no look relieves again.
#[test]
fn a_workspace_removed_while_a_run_waits_renews_its_relief() {
    use aterm_capacity::Relief as _;
    use std::cell::RefCell;
    use std::rc::Rc;
    type Shared<T> = Rc<RefCell<T>>;
    let reclaim = |during: Shared<Option<PathBuf>>, listed: Shared<Vec<PathBuf>>| {
        Reclaim::of(
            Box::new(move |_room: super::Room<'_>, _stopped| {
                if let Some(leaf) = during.borrow_mut().take() {
                    listed.borrow_mut().push(leaf);
                }
                (0, Vec::new(), Vec::new())
            }),
            Box::new(|_, _, _| None),
            Box::new(|_, _| Vec::new()),
        )
    };
    let listed: Shared<Vec<PathBuf>> =
        Rc::new(RefCell::new(vec![PathBuf::from("/c/held/.cargo-build")]));
    let removing: Shared<Option<PathBuf>> = Rc::default();
    let theirs = Rc::clone(&listed);
    let mut relief = reclaim(Rc::clone(&removing), Rc::clone(&listed))
        .watching(Box::new(move || theirs.borrow().iter().cloned().collect()));
    let short = aterm_capacity::Shortfall {
        shorts: vec![aterm_capacity::Short::Disk {
            need: 20 * GIB,
            mount: PathBuf::from("/Volumes/scratch"),
            above_floor: 12 * GIB,
            floor: 10 * GIB,
            reserved: 0,
            held: Vec::new(),
        }],
        holders: Vec::new(),
    };
    relief.relieve(&short, &mut |_| {}, &|| false);
    assert!(!relief.renewed(&|| false), "what its pass left is no news");
    listed
        .borrow_mut()
        .push(PathBuf::from("/c/removed/.cargo-build"));
    assert!(
        relief.renewed(&|| false),
        "a workspace removed since renews it"
    );
    assert!(!relief.renewed(&|| true), "nor once its wait is stopped");
    relief.relieve(&short, &mut |_| {}, &|| false);
    assert!(!relief.renewed(&|| false), "the new pass is the new mark");
    *removing.borrow_mut() = Some(PathBuf::from("/c/mid-pass/.cargo-build"));
    relief.relieve(&short, &mut |_| {}, &|| false);
    assert!(
        relief.renewed(&|| false),
        "a workspace removed while the pass ran renews the next look"
    );

    let memory = aterm_capacity::Shortfall {
        shorts: vec![aterm_capacity::Short::Memory {
            need: 8 * GIB,
            margin: 0,
            available: GIB,
            reserved: 0,
            passes: None,
            stuck_ahead: 0,
            stuck: false,
        }],
        holders: Vec::new(),
    };
    relief.relieve(&memory, &mut |_| {}, &|| false);
    assert!(
        !relief.renewed(&|| false),
        "short of memory alone: never renewed"
    );

    let mut blind = reclaim(Rc::default(), Rc::default());
    blind.relieve(&short, &mut |_| {}, &|| false);
    assert!(!blind.renewed(&|| false), "watching nothing, never renewed");
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
/// first verify run's 27 GiB on a volume 36 GiB above its floor beside a held 10 GiB claim (it
/// fits the room but not beside that claim, so it waits), looking every two minutes
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
    let ledger = Ledger::at(dir.join("ledger"));
    let machine = Fixed::new(image(40, 40));
    let held = held_beside(&ledger, &machine, 10);
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
        &ledger,
        &machine,
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
    drop(held);
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

/// A disk claim held up by what a claim ahead holds on its volume ends on SIGTERM, or SIGINT, as
/// NOT RUN well inside
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
    assert_eq!(
        remove::named(&stage, 2).first,
        [(PathBuf::from("tmp/d"), remove::Why::StillRefusing)]
    );
    assert!(super::stayed_notes(&went[..1]).is_empty());
}

/// WHAT A PASS KEPT IS NOT WHAT IT REMOVED: of the paths a run's record let go, one the pass
/// returned as nothing of it gone (`Unused::Refused`: its rename refused after the check, or
/// stopped once in its stage and returned as that stage's path, `Refusal::Staged`) is not among
/// the directories the relief removed, and gives a note of its own; one that went, or whose stage
/// stayed in part, still is. NEGATIVE CONTROL: the same paths returned as gone are all removed,
/// and give no note; and a relief pairing its paths with their outcomes by path keeps the staged
/// one among what it removed.
#[test]
fn a_relief_keeps_what_its_pass_refused_out_of_what_it_removed() {
    use crate::build_cache::{Refusal, Unused};
    let (went, kept, staged) = (
        PathBuf::from("/c/gone/.cargo-build"),
        PathBuf::from("/c/kept/.cargo-build"),
        PathBuf::from("/c/staged/.cargo-build"),
    );
    let stage = PathBuf::from("/c/staged/.cargo-build.removing");
    let asked = vec![went.clone(), kept.clone(), staged.clone()];
    let refused = Unused::Refused {
        by: Refusal::Rename(std::io::ErrorKind::PermissionDenied),
    };
    let returned = [
        (went.clone(), Unused::Unfinished),
        (kept.clone(), refused),
        (
            stage.clone(),
            Unused::Refused {
                by: Refusal::Staged,
            },
        ),
    ];
    assert_eq!(
        super::went_of(asked.clone(), &returned),
        std::slice::from_ref(&went)
    );
    let notes = super::stayed_notes(&returned);
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes[0].contains(&kept.display().to_string()), "{notes:?}");
    assert!(notes[0].contains(&Refusal::Rename(std::io::ErrorKind::PermissionDenied).word()));
    assert!(notes[1].contains(&stage.display().to_string()), "{notes:?}");
    assert!(notes[1].contains(&Refusal::Staged.word()), "{notes:?}");
    let all_gone = [
        (went.clone(), Unused::Unfinished),
        (kept.clone(), Unused::Unfinished),
        (staged.clone(), Unused::Unfinished),
    ];
    assert_eq!(super::went_of(asked.clone(), &all_gone), asked);
    assert!(super::stayed_notes(&all_gone).is_empty());
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
        scope: scope_tag(SCOPE),
    });
    kept.push(super::Footprint {
        peak: 1 << 20,
        grew: 1 << 20,
        at_ms: 2,
        dirs: Some(dirs_tag(&run.dirs)),
        scope: scope_tag(SCOPE),
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

/// A RUN'S FOOTPRINTS ARE KEPT BY ITS SCOPE ([`Run::scope`]): a `-p` verify run that wrote
/// 1 MiB into its build directory is what the next `-p` run claims, read from that one run, but
/// a whole-workspace run on the same directories claims the cold budget less what they hold,
/// read from no run; once it wrote 3 MiB, the `-p` run still claims its own 1 MiB, and the
/// whole-workspace run its 3 MiB. Negative control: with the scope ignored, the whole-workspace
/// run claims the `-p` run's 1 MiB.
#[test]
fn a_narrow_runs_record_is_not_what_a_wider_run_claims() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-disk-scope-")
        .tempdir()
        .expect("scratch");
    let narrow = verify_run(dir.path());
    let wide = Run {
        what: "verify lint".to_owned(),
        scope: "lint --workspace".to_owned(),
        ..narrow.clone()
    };
    let build = narrow.dirs[0].clone();
    std::fs::create_dir_all(&build).expect("the build directory");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let claim = |run: &Run| {
        let mut never = Reclaim::of(
            Box::new(|_, _| panic!("reclaimed for a run with room")),
            Box::new(|_, _, _| panic!("asked for a run with room")),
            Box::new(|_, _| panic!("named holders for a run with room")),
        );
        admit_on(
            run.clone(),
            &ledger,
            &Fixed::new(image(40, 36)),
            Admit::Wait,
            Pace::default(),
            &mut never,
            &mut |_| {},
        )
        .expect("room")
    };
    let first = claim(&narrow);
    std::fs::write(build.join("narrow.rlib"), vec![1u8; 1 << 20]).expect("written");
    let _ = first.finish();
    let again = claim(&narrow);
    let e = again.estimate;
    assert_eq!(e.runs, 1, "{e}");
    assert!((1 << 20..2 << 20).contains(&e.bytes), "{e}");
    let _ = again.finish();

    let holds = aterm_capacity::allocated(&wide.dirs);
    let first_wide = claim(&wide);
    let e = first_wide.estimate;
    assert_eq!((e.bytes, e.runs), (27 * GIB - holds, 0), "{e}");
    std::fs::write(build.join("wide.rlib"), vec![2u8; 3 << 20]).expect("written");
    let _ = first_wide.finish();

    let e = claim(&narrow).estimate;
    assert!(
        (1 << 20..2 << 20).contains(&e.bytes),
        "the -p run's own: {e}"
    );
    let e = claim(&wide).estimate;
    assert!(
        (3 << 20..4 << 20).contains(&e.bytes),
        "the wide run's own: {e}"
    );
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

/// A RUN THAT ASKS TO GIVE UP BEHIND A HOLDER THAT MAKES NO PROGRESS DOES (CW-WAIT-UNBOUNDED):
/// on a volume another claim holds, its holder idle as the machine reads it, a run whose
/// [`Run::stuck_ends`] is short gives up within the hang detector, naming the holder, and leaves
/// the queue with the holder's claim still held. NEGATIVE CONTROL: the same run asking nothing
/// waits on past three of those bounds, and is admitted once the holder lets go.
#[test]
fn a_run_that_asks_to_give_up_behind_a_stuck_holder_does() {
    use aterm_capacity::{Need, Proc, Procs};
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-disk-stuck-")
        .tempdir()
        .expect("scratch");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let me = std::process::id();
    let m = Arc::new(Fixed::new(image(40, 36)));
    m.set_procs(Some(Procs::of(vec![Proc {
        pid: me,
        ppid: 1,
        cpu_ns: 5,
        ..Proc::default()
    }])));
    let held = aterm_capacity::admit_with(
        &ledger,
        &*m,
        Need::new("lab windows boot").disk(30 * GIB, "/Volumes/scratch"),
        Admit::Wait,
        Pace::default(),
        &mut |_| {},
    )
    .expect("the holder's claim");
    let bound = Duration::from_millis(200);
    let wait = |stuck_ends: Option<Duration>| {
        let (ledger, m) = (ledger.clone(), Arc::clone(&m));
        let run = Run {
            stuck_ends,
            ..verify_run(dir.path())
        };
        std::thread::spawn(move || {
            let mut nothing = Reclaim::of(
                Box::new(|_, _| (0, Vec::new(), Vec::new())),
                Box::new(|_, _, _| None),
                Box::new(|_, _| Vec::new()),
            );
            admit_on(
                run,
                &ledger,
                &*m,
                Admit::Wait,
                Pace {
                    poll: Duration::from_millis(5),
                    line_every: Duration::from_secs(60),
                },
                &mut nothing,
                &mut |_| {},
            )
        })
    };
    match joined(wait(Some(bound)), "the stuck wait's end") {
        Err(e @ super::Error::Stuck { .. }) => names(
            &e.to_string(),
            &[
                "gave up its wait",
                &format!("pid {me} holds 30 GiB of disk"),
                "which has made no progress for",
            ],
        ),
        other => panic!("expected the wait to end behind the stuck holder, got {other:?}"),
    }
    let listed: Vec<String> = ledger
        .list()
        .expect("listed")
        .into_iter()
        .map(|r| r.need.what)
        .collect();
    assert_eq!(listed, ["lab windows boot"], "it left the queue");
    let waiting = wait(None);
    // Three bounds of the condition under test, not a wait for anything.
    std::thread::sleep(bound * 3);
    assert!(
        !waiting.is_finished(),
        "a run that asked nothing gave up: {:?}",
        joined(waiting, "its end")
    );
    drop(held);
    let admitted = joined(waiting, "the admission once the holder lets go").expect("admitted");
    let _ = admitted.finish();
}

/// The machine's record, under `dir`, of [`verify_run`]'s kind, and what such a run builds.
fn machinewide(dir: &Path) -> super::Machinewide {
    super::Machinewide {
        record: dir.join("state/verify/disk-footprint.json"),
        selection: super::Selection {
            work: "lint".to_owned(),
            crates: Some(vec!["aterm-capacity".to_owned()]),
        },
    }
}

/// A [`verify_run`] in the checkout `name` under `dir`, keeping its runs in the machine's record
/// ([`machinewide`]) where `machine` says so, its build directory holding the verify tool's own
/// build (`tool` bytes), as a fresh worktree's does once `targo verify` has built the tool.
fn checkout_run(dir: &Path, name: &str, machine: bool, tool: usize) -> Run {
    let run = Run {
        machine: machine.then(|| machinewide(dir)),
        ..verify_run(&dir.join(name))
    };
    std::fs::create_dir_all(&run.dirs[0]).expect("the build directory");
    std::fs::write(run.dirs[0].join("tool.rlib"), vec![3u8; tool]).expect("the tool's build");
    run
}

/// `run` admitted on a volume with room, with a relief that must not be asked.
fn admit_with_room(run: Run, ledger: &Ledger) -> super::Admitted {
    let mut never = Reclaim::of(
        Box::new(|_, _| panic!("reclaimed for a run with room")),
        Box::new(|_, _, _| panic!("asked for a run with room")),
        Box::new(|_, _| panic!("named holders for a run with room")),
    );
    admit_on(
        run,
        ledger,
        &Fixed::new(image(40, 36)),
        Admit::Wait,
        Pace::default(),
        &mut never,
        &mut |_| {},
    )
    .expect("room")
}

/// A FRESH WORKTREE WHOSE SCOPE RAN IN ANOTHER CHECKOUT HERE CLAIMS WHAT THAT RUN WROTE, not the
/// cold budget (V1, DA-MACHINE-FOOTPRINT). The first worktree's run, its build directory holding
/// only the verify tool's 2 MiB build, claims the cold budget less that, grows its directories by
/// 3 MiB and passes; the second worktree, holding the same tool build, claims those 3 MiB, read
/// from that one run in the machine's record, and says so. NEGATIVE CONTROL: a third fresh
/// worktree whose kind keeps no machine record claims the cold budget less what it holds, as
/// every fresh worktree did before; and with the machine's lookup dropped from the claim
/// (`need_of` reading `kept.need` alone) the second worktree claims the same cold figure and
/// this test fails.
#[test]
fn a_fresh_worktree_claims_what_its_scope_wrote_in_another_checkout_here() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-disk-machine-")
        .tempdir()
        .expect("scratch");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let tool = 2 << 20;
    let one = checkout_run(dir.path(), "one", true, tool);
    let build_one = one.dirs[0].clone();
    let holds = aterm_capacity::allocated(&one.dirs);
    let first = admit_with_room(one, &ledger);
    let e = first.estimate;
    assert_eq!(
        (e.bytes, e.runs, e.machine),
        (27 * GIB - holds, 0, None),
        "{e}"
    );
    std::fs::write(build_one.join("scope.rlib"), vec![4u8; 3 << 20]).expect("the scope's units");
    let done = first.finish_whole();
    assert!(
        done.here.is_ok() && matches!(done.machine, Some(Ok(()))),
        "both records took it: {done:?}"
    );
    let took = done.run;
    assert_eq!((took.peak, took.grew), (holds + (3 << 20), 3 << 20));

    let two = checkout_run(dir.path(), "two", true, tool);
    assert_eq!(
        aterm_capacity::allocated(&two.dirs),
        holds,
        "as fresh as the first"
    );
    let second = admit_with_room(two, &ledger);
    let e = second.estimate;
    assert_eq!(e.bytes, took.grew, "the first run's growth: {e}");
    assert_eq!(e.runs, 0, "no run of it on its own directories: {e}");
    let cover = e.machine.expect("read from the machine's record");
    assert_eq!((cover.runs, cover.selections, cover.exact), (1, 1, true));
    names(
        &e.to_string(),
        &[
            "3 MiB, the most a run of it in this machine's checkouts wrote",
            "no run of it measured on these directories",
        ],
    );
    let _ = second.finish_whole();

    let three = checkout_run(dir.path(), "three", false, tool);
    let holds = aterm_capacity::allocated(&three.dirs);
    let third = admit_with_room(three, &ledger);
    let e = third.estimate;
    assert_eq!((e.bytes, e.machine), (27 * GIB - holds, None), "{e}");
    assert!(
        third.finish_whole().machine.is_none(),
        "its kind keeps no machine record"
    );

    // A machine record that cannot be written says so, and the run is kept beside its
    // directories all the same.
    let mut four = checkout_run(dir.path(), "four", true, tool);
    let blocked = dir.path().join("blocked");
    std::fs::write(&blocked, b"a file where the record's folder goes").expect("written");
    if let Some(m) = four.machine.as_mut() {
        m.record = blocked.join("disk-footprint.json");
    }
    let done = admit_with_room(four, &ledger).finish_whole();
    assert!(
        done.here.is_ok() && matches!(done.machine, Some(Err(_))),
        "{done:?}"
    );
}

/// A SCOPE NO RUN HERE COVERS STILL CLAIMS THE COLD BUDGET less what its directories hold: a
/// fresh worktree's run of other crates than any run kept, or of other checks, reads nothing from
/// the machine's record. NEGATIVE CONTROL: the same worktree running the kept selection claims
/// what it wrote.
#[test]
fn a_scope_no_run_here_covers_still_claims_the_cold_budget() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-disk-machine-cold-")
        .tempdir()
        .expect("scratch");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let m = machinewide(dir.path());
    aterm_capacity::MachineFootprints::keep(
        &m.record,
        m.selection.clone(),
        super::Footprint {
            peak: 5 << 20,
            grew: 3 << 20,
            at_ms: 1,
            dirs: Some(1),
            scope: None,
        },
    )
    .expect("kept");
    let fresh = |selection: super::Selection| {
        let mut run = checkout_run(dir.path(), "fresh", true, 2 << 20);
        run.machine = Some(super::Machinewide {
            selection,
            ..machinewide(dir.path())
        });
        let holds = aterm_capacity::allocated(&run.dirs);
        let admitted = admit_with_room(run, &ledger);
        let e = admitted.estimate;
        drop(admitted);
        (e, holds)
    };
    let (e, holds) = fresh(super::Selection {
        work: "lint".to_owned(),
        crates: Some(vec!["aterm-time".to_owned()]),
    });
    assert_eq!(
        (e.bytes, e.machine),
        (27 * GIB - holds, None),
        "other crates: {e}"
    );
    let (e, holds) = fresh(super::Selection {
        work: "lint test".to_owned(),
        crates: m.selection.crates.clone(),
    });
    assert_eq!(
        (e.bytes, e.machine),
        (27 * GIB - holds, None),
        "other checks: {e}"
    );
    let (e, _) = fresh(m.selection.clone());
    assert_eq!(e.bytes, 3 << 20, "the kept selection: {e}");
}

/// A RUN THAT DID NOT DO ALL ITS WORK IS KEPT FOR ITS OWN DIRECTORIES ALONE: one that failed may
/// have stopped short of what its scope writes, so the next fresh worktree claims the cold budget
/// as it would with no run kept, while the run's own checkout claims what it grew. NEGATIVE
/// CONTROL: the same run finished whole is what the next fresh worktree claims.
#[test]
fn a_run_that_did_not_pass_is_kept_for_its_own_directories_alone() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-disk-machine-failed-")
        .tempdir()
        .expect("scratch");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let one = checkout_run(dir.path(), "one", true, 2 << 20);
    let build_one = one.dirs[0].clone();
    let first = admit_with_room(one.clone(), &ledger);
    std::fs::write(build_one.join("scope.rlib"), vec![4u8; 1 << 20]).expect("written");
    let _ = first.finish();
    assert!(
        aterm_capacity::MachineFootprints::load(&machinewide(dir.path()).record)
            .rows()
            .is_empty(),
        "nothing kept for the machine"
    );
    let e = admit_with_room(one.clone(), &ledger).estimate;
    assert_eq!((e.bytes, e.runs), (1 << 20, 1), "its own directories: {e}");
    let two = checkout_run(dir.path(), "two", true, 2 << 20);
    let holds = aterm_capacity::allocated(&two.dirs);
    let e = admit_with_room(two.clone(), &ledger).estimate;
    assert_eq!(e.bytes, 27 * GIB - holds, "a fresh worktree: {e}");

    // Its checkout held the tool, the failed run's 1 MiB and its record when it ran whole, so a
    // fresh one, holding the tool alone, writes what it takes to bring its directories to that
    // run's peak: those 1 MiB, its 2 MiB and the record's block.
    let again = admit_with_room(one, &ledger);
    std::fs::write(build_one.join("more.rlib"), vec![5u8; 2 << 20]).expect("written");
    let took = again.finish_whole().run;
    assert_eq!(took.grew, 2 << 20);
    let e = admit_with_room(two, &ledger).estimate;
    assert_eq!(e.bytes, took.peak - holds, "the run finished whole: {e}");
    assert!((3 << 20..(3 << 20) + (64 << 10)).contains(&e.bytes), "{e}");
}

/// A noop relief: no pass, no window, no holders.
fn no_relief() -> Reclaim {
    Reclaim::of(
        Box::new(|_, _| (0, Vec::new(), Vec::new())),
        Box::new(|_, _, _| None),
        Box::new(|_, _| Vec::new()),
    )
}

/// A release's whole disk claim, sized by its caller.
fn release_claim(dir: &Path, bytes: u64) -> super::SizedClaim {
    super::SizedClaim {
        what: "release 0.156.0".to_owned(),
        bytes,
        on: PathBuf::from("/Volumes/scratch"),
        keep: vec![dir.join("clone-build")],
        checkout: Some(dir.join("clone").display().to_string()),
        stuck_ends: None,
    }
}

/// A CLAIM OF A KNOWN SIZE IS HELD WITHOUT A WALK, AND KEEPS NOTHING ([`super::admit_sized_on`]):
/// on a volume with room it is admitted at once, saying nothing, for exactly what its caller
/// sized, its checkout named; its marker is the name the ledger holds it by, which covers its
/// holder's run ([`aterm_capacity::covering_disk`]); what the work says it wrote is the claim's
/// progress; dropped, it leaves the ledger, and nothing was written beside the directories it
/// keeps. NEGATIVE CONTROL: a need more than the volume holds above its floor is refused at once,
/// saying first what it claims.
#[test]
fn a_sized_claim_is_held_unwalked_and_keeps_nothing() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-sized-")
        .tempdir()
        .expect("scratch");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let m = Fixed::new(image(400, 300));
    let run = release_claim(dir.path(), 39 * GIB);
    let checkout = run.checkout.clone();
    let mut said = Vec::new();
    let mut held = super::admit_sized_on(
        run,
        &ledger,
        &m,
        Admit::Wait,
        Pace::default(),
        &mut no_relief(),
        &mut |l| said.push(l.to_owned()),
    )
    .expect("admitted");
    assert!(said.is_empty(), "{said:?}");
    let listed = ledger.list().expect("listed");
    assert_eq!(listed.len(), 1);
    let r = &listed[0];
    assert_eq!(
        (r.need.what.as_str(), r.need.disk, Some(&r.checkout)),
        ("release 0.156.0", 39 * GIB, checkout.as_ref())
    );
    assert!(r.held.is_some());
    assert_eq!(held.marker(), r.cover_marker());
    assert_eq!(held.record().cover_marker(), held.marker());
    let covered = aterm_capacity::covering_disk(
        &ledger,
        &m,
        std::process::id(),
        &held.marker(),
        Path::new("/Volumes/scratch/clone"),
    )
    .expect("read");
    assert_eq!(covered.map(|r| r.need.disk), Some(39 * GIB));
    held.wrote(5 * GIB).expect("wrote");
    assert_eq!(ledger.list().expect("listed")[0].wrote, Some(5 * GIB));
    drop(held);
    assert!(ledger.list().expect("listed").is_empty());
    assert!(
        std::fs::read_dir(dir.path())
            .expect("scratch")
            .flatten()
            .all(|e| e.file_name() == "ledger"),
        "nothing kept beside it"
    );

    let mut said = Vec::new();
    let never = super::admit_sized_on(
        release_claim(dir.path(), 500 * GIB),
        &ledger,
        &m,
        Admit::Wait,
        Pace::default(),
        &mut no_relief(),
        &mut |l| said.push(l.to_owned()),
    );
    assert!(matches!(never, Err(super::Error::Never(_))), "{never:?}");
    assert_eq!(said.len(), 1, "{said:?}");
    names(&said[0], &[&size(500 * GIB), "release 0.156.0"]);
    assert!(ledger.list().expect("listed").is_empty());
}

/// A SIZED CLAIM ON A SHORT VOLUME IS RELIEVED, THEN WAITS, saying first what it claims: within
/// the room but short of what a held claim reserves, the pass is asked for the short volume, and
/// once room comes the claim is admitted, having waited. Beyond the room once relieved, it is
/// refused at once (UNMET), naming how much to free, and leaves the ledger; the release has no
/// way to start at once, so it is offered none.
/// NEGATIVE CONTROL: the claim admitted at once
/// ([`a_sized_claim_is_held_unwalked_and_keeps_nothing`]) asks no relief.
#[test]
fn a_sized_claim_on_a_short_volume_is_relieved_then_waits() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("admit-sized-short-")
        .tempdir()
        .expect("scratch");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let m = Arc::new(Fixed::new(image(100, 50)));
    let ahead = held_beside(&ledger, &m, 10);
    let passes = Arc::new(Mutex::new(0usize));
    let (theirs, their_passes, their_ledger) =
        (Arc::clone(&m), Arc::clone(&passes), ledger.clone());
    let run = release_claim(dir.path(), 39 * GIB);
    let (tx, rx) = mpsc::channel::<String>();
    let waiting = std::thread::spawn(move || {
        let freed = Arc::clone(&theirs);
        let mut relief = Reclaim::of(
            Box::new(move |room: super::Room<'_>, _stopped| {
                assert_eq!(room.mount, Path::new("/Volumes/scratch"));
                *their_passes.lock().expect("passes") += 1;
                (0, Vec::new(), Vec::new())
            }),
            Box::new(|_, _, _| None),
            Box::new(|_, _| Vec::new()),
        );
        let held = super::admit_sized_on(
            run,
            &their_ledger,
            freed.as_ref(),
            Admit::Wait,
            Pace {
                poll: Duration::from_millis(5),
                ..Pace::default()
            },
            &mut relief,
            &mut |l| {
                let _ = tx.send(l.to_owned());
            },
        );
        held.map(|h| h.waited)
    });
    let first = rx.recv_timeout(MUST_HAPPEN).expect("its first line");
    names(&first, &[&size(39 * GIB), "release 0.156.0"]);
    aterm_spec::wait::until_within(MUST_HAPPEN, "the claim to wait", || {
        let listed = ledger.list().ok()?;
        let release = listed.iter().find(|r| r.need.what == "release 0.156.0")?;
        release.held.is_none().then_some(())
    });
    m.set(image(100, 80));
    let waited = joined(waiting, "the sized claim to be admitted").expect("admitted");
    assert!(waited > Duration::ZERO);
    assert_eq!(*passes.lock().expect("passes"), 1, "relieved once");
    drop(ahead);

    let short = Fixed::new(image(100, 30));
    let mut said = Vec::new();
    let refused = super::admit_sized_on(
        release_claim(dir.path(), 39 * GIB),
        &ledger,
        &short,
        Admit::Wait,
        Pace {
            poll: Duration::from_millis(5),
            ..Pace::default()
        },
        &mut no_relief(),
        &mut |l| said.push(l.to_owned()),
    );
    match refused {
        Err(e @ aterm_capacity::Error::Unmet { .. }) => names(
            &e.to_string(),
            &[
                "release 0.156.0",
                "Free at least 13 GiB on /Volumes/scratch",
            ],
        ),
        other => panic!("refused as unmet, got {other:?}"),
    }
    names(&said[0], &[&size(39 * GIB), "release 0.156.0"]);
    assert!(
        ledger.list().expect("listed").is_empty(),
        "it left the queue"
    );
}
