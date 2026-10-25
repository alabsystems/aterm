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
            Box::new(move |mount, _stopped| {
                assert_eq!(mount, Path::new("/Volumes/scratch"));
                pass_order.lock().expect("order").push("reclaim");
                pass_m.set(image(40, 22));
                (
                    2 * GIB,
                    vec![PathBuf::from("/Volumes/scratch/gone/.cargo-build")],
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
/// the volume, and nothing is reclaimed: waiting could never end.
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
                Box::new(|_, _| (0, Vec::new())),
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
    let line = rx.recv_timeout(MUST_HAPPEN).expect("the relief's line");
    names(&line, &["/Volumes/scratch"]);
    let wait = rx.recv_timeout(MUST_HAPPEN).expect("the wait line");
    names(&wait, &["27 GiB"]);
    theirs.set(image(40, 36));
    let cold = waiting.join().expect("the run").expect("admitted");
    assert_eq!((cold.estimate.bytes, cold.estimate.runs), (27 * GIB, 0));
    let _ = cold.finish();
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
        Box::new(|_, _| (0, Vec::new())),
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
