// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;

/// A refusal a remote that keeps asking makes the node job write (`fleet_listen`'s line).
const REFUSAL: &str = "aterm-gui: fleet: 198.51.100.7:4242 not admitted: no proof";

/// 2023-11-14T22:13:20Z.
const T: u64 = 1_700_000_000;

/// Whether every line of `text` opens with a date and its space.
fn all_dated(text: &str) -> bool {
    text.lines()
        .all(|l| l.len() > STAMP_LEN && is_stamp(&l.as_bytes()[..STAMP_LEN]) && undated(l) != l)
}

/// The writer cuts the inode it was given (the one launchd opened) in place, never past the cap,
/// with no hole at an inherited offset, and opens the cut file with a dated line saying so.
/// NEGATIVE CONTROL: a raw append to that same descriptor (what every write of an unbounded job
/// did) grows past the cap; the next bounded write repairs it. Both append and ordinary inherited
/// offsets are covered, and a line longer than the cap is refused, not written.
#[test]
fn the_existing_log_inode_is_cut_in_place_and_says_so() {
    let root = aterm_tempfile::tempdir().unwrap();
    for append in [false, true] {
        let path = root.path().join(format!("log-{append}"));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .append(append)
            .open(&path)
            .unwrap();
        let inode = file.metadata().unwrap().ino();
        file.set_len(1000).unwrap();
        let mut sink = Bounded { file, cap: 256 };
        for _ in 0..40 {
            sink.write(b"a state changed\n", T).unwrap();
            let meta = std::fs::metadata(&path).unwrap();
            assert!(meta.len() <= sink.cap, "{} > {}", meta.len(), sink.cap);
            assert_eq!(
                meta.ino(),
                inode,
                "launchd must keep writing the bounded inode"
            );
            assert!(
                !std::fs::read(&path).unwrap().contains(&0),
                "no hole at the old offset"
            );
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let first = text.lines().next().unwrap();
        assert_eq!(
            first,
            "2023-11-14T22:13:20Z this log reached 256 bytes and was cut here: the lines \
             before are gone"
        );
        sink.file.write_all(&[b'x'; 300]).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > sink.cap);
        sink.bound(T).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{first}\n"),
            "a log other writes took past the cap is cut back"
        );
        sink.write(b"repaired\n", T).unwrap();
        let repaired = std::fs::read_to_string(&path).unwrap();
        assert!(repaired.ends_with("gone\nrepaired\n"), "{repaired:?}");
        sink.bound(T).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            repaired,
            "under the cap"
        );
        assert!(sink.write(&[b'x'; 257], T).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), repaired);
    }
}

/// A date is UTC, RFC 3339, to the second; a reader strips exactly the one the log put there,
/// and a line that is only a date reads as empty.
/// NEGATIVE CONTROL: a line that carries no date, or a date-like word not followed by a space,
/// is read back whole.
#[test]
fn a_date_is_utc_rfc3339_to_the_second_and_a_reader_strips_it() {
    for (secs, said) in [
        (0, "1970-01-01T00:00:00Z"),
        (951_782_400, "2000-02-29T00:00:00Z"),
        (T, "2023-11-14T22:13:20Z"),
        (1_798_761_599, "2026-12-31T23:59:59Z"),
        (u64::MAX, "9999-12-31T23:59:59Z"),
    ] {
        assert_eq!(std::str::from_utf8(&stamp(secs)).unwrap(), said);
    }
    let line = format!("{} {REFUSAL}", std::str::from_utf8(&stamp(T)).unwrap());
    assert_eq!(undated(&line), REFUSAL);
    let twice = format!("{} {line}", std::str::from_utf8(&stamp(T)).unwrap());
    assert_eq!(undated(&twice), line);
    for blank in ["2023-11-14T22:13:20Z", "2023-11-14T22:13:20Z "] {
        assert_eq!(undated(blank), "", "{blank:?}");
    }
    for whole in [
        REFUSAL,
        "2023-11-14T22:13:20Zx",
        "2023-11-14 22:13:20Z late",
        "2023-11-14T22:13:2",
        "",
    ] {
        assert_eq!(undated(whole), whole);
    }
}

/// A definition's outputs are read from a plist (XML entities, any whitespace) and a unit
/// (`append:` and `file:`), each file once; `keep` records the paths it was handed on its thread
/// and keeps none that is not this process's standard output or error.
/// NEGATIVE CONTROL: a plist with no output keys and a unit whose standard error is the journal
/// name no file; a thread that called no `keep` asked nothing.
#[test]
fn a_definitions_outputs_are_read_and_keep_records_what_it_was_handed() {
    let plist = "<dict>\n\t<key>StandardOutPath</key>\n\t<string>/tmp/a&amp;b/x.log</string>\n\
                 <key>StandardErrorPath</key><string>/tmp/a&amp;b/x.log</string></dict>";
    let one = seam::plist_outputs(plist);
    assert_eq!(one.stdout.as_deref(), Some(Path::new("/tmp/a&b/x.log")));
    assert_eq!(one.stdout, one.stderr);
    assert_eq!(one.files(), vec![PathBuf::from("/tmp/a&b/x.log")]);
    let none = seam::plist_outputs("<dict><key>Label</key><string>x</string></dict>");
    assert_eq!(
        none,
        seam::Outputs {
            stdout: None,
            stderr: None
        }
    );
    assert!(none.files().is_empty());
    let unit =
        seam::unit_outputs("[Service]\nStandardOutput=file:/x/o\nStandardError=append:/x/e\n");
    assert_eq!(unit.stdout.as_deref(), Some(Path::new("/x/o")));
    assert_eq!(unit.stderr.as_deref(), Some(Path::new("/x/e")));
    assert_eq!(seam::unit_outputs("StandardError=journal\n").stderr, None);

    assert_eq!(seam::asked(), None, "this thread called no keep");
    let root = aterm_tempfile::tempdir().unwrap();
    let file = root.path().join("not-this-process.log");
    std::fs::write(&file, "a person's own line\n").unwrap();
    let missing = root.path().join("missing.log");
    assert_eq!(
        keep(&[&file, &missing]),
        Kept {
            stdout: false,
            stderr: false
        }
    );
    assert_eq!(seam::asked(), Some(vec![file.clone(), missing]));
    assert_eq!(seam::asked(), None, "taken");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "a person's own line\n"
    );
}

/// The full name of [`a_stand_in_job_keeps_only_the_file_its_output_is`] in this binary.
const KEEPS: &str = "job_log::tests::a_stand_in_job_keeps_only_the_file_its_output_is";

/// One stage of a stand-in job whose standard output and error the service manager opened on
/// the file `log` (one file for both, as launchd's two keys naming one path open it).
fn kept_stage(stage: &str, dir: &Path) {
    let log = dir.join("job.log");
    let theirs = dir.join("theirs.log");
    match stage {
        // The job's verb keeps the file its definition names, which is its output: a burst of
        // its diagnostics stays under the cap.
        "kept" => {
            seam::open_as_service_manager(&seam::Outputs {
                stdout: Some(log.clone()),
                stderr: Some(log.clone()),
            })
            .unwrap();
            let keeping = keep(&[&log]);
            for _ in 0..seam::BURST {
                line(format_args!("{REFUSAL}"));
            }
            line(format_args!("a diagnostic\nwith the newline a client sent"));
            assert_eq!(
                keeping,
                Kept {
                    stdout: true,
                    stderr: true
                }
            );
        }
        // The control: the same burst with nothing kept.
        "unkept" => {
            seam::open_as_service_manager(&seam::Outputs {
                stdout: Some(log.clone()),
                stderr: Some(log.clone()),
            })
            .unwrap();
            for _ in 0..seam::BURST {
                line(format_args!("{REFUSAL}"));
            }
        }
        // A person's run with its output on a file of their own: the definition's file is not
        // it, so nothing is kept, the person's file is never cut and the definition's is never
        // touched.
        "elsewhere" => {
            seam::open_as_service_manager(&seam::Outputs {
                stdout: Some(theirs.clone()),
                stderr: Some(theirs.clone()),
            })
            .unwrap();
            std::fs::write(&log, "a line of the job's last run\n").unwrap();
            assert_eq!(
                keep(&[&log]),
                Kept {
                    stdout: false,
                    stderr: false
                }
            );
            for _ in 0..seam::BURST {
                line(format_args!("{REFUSAL}"));
            }
        }
        // Straight writes (an `eprintln!`, a vendored warning) to the kept file are cut back by
        // the thread.
        "straight" => {
            seam::bound_every(Duration::from_millis(100));
            seam::open_as_service_manager(&seam::Outputs {
                stdout: Some(log.clone()),
                stderr: Some(log.clone()),
            })
            .unwrap();
            assert!(keep(&[&log]).stderr());
            for _ in 0..seam::BURST {
                eprintln!("{REFUSAL}");
            }
            assert!(
                seam::until_bounded(std::slice::from_ref(&log)),
                "the kept log stayed past the cap for a minute"
            );
        }
        // The control: the same straight burst, nothing kept.
        "straight-unkept" => {
            seam::bound_every(Duration::from_millis(100));
            seam::open_as_service_manager(&seam::Outputs {
                stdout: Some(log.clone()),
                stderr: Some(log.clone()),
            })
            .unwrap();
            for _ in 0..seam::BURST {
                eprintln!("{REFUSAL}");
            }
        }
        other => panic!("no stage {other}"),
    }
    std::process::exit(0);
}

/// **A JOB KEEPS THE FILE ITS OUTPUT IS, AND ONLY THAT FILE.** A stand-in job (this binary run
/// again, its standard output and error on one file as launchd opens them) hands `keep` that
/// file: a burst of about twice the cap's worth of its diagnostics leaves it under the cap, cut
/// and saying so, every line dated and the last one (a client's newline in it) one line; a burst
/// of straight `eprintln!` lines is cut back under the cap by the thread.
/// NEGATIVE CONTROL: the same bursts with nothing kept grow the file past the cap; and a run
/// whose output is a file of the person's own, handed the definition's path, keeps nothing: the
/// person's file takes every line uncut, and the definition's file is never touched.
#[test]
fn a_stand_in_job_keeps_only_the_file_its_output_is() {
    if let Some((stage, dir)) = seam::stage() {
        kept_stage(&stage, &dir);
        return;
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let run = |stage: &str| {
        let dir = root.path().join(stage);
        std::fs::create_dir(&dir).unwrap();
        seam::rerun(KEEPS, stage, &dir).unwrap();
        dir
    };
    let read = |dir: &Path, name: &str| std::fs::read_to_string(dir.join(name)).unwrap();

    let kept = read(&run("kept"), "job.log");
    assert!(kept.len() as u64 <= CAP, "{} > {CAP}", kept.len());
    assert!(kept.lines().next().unwrap().contains("was cut here"));
    assert!(all_dated(&kept), "every line is dated");
    assert_eq!(
        kept.lines().last().map(undated),
        Some("a diagnostic with the newline a client sent")
    );
    let unkept = read(&run("unkept"), "job.log");
    assert!(unkept.len() as u64 > CAP, "the control is unbounded");

    let elsewhere = run("elsewhere");
    let theirs = read(&elsewhere, "theirs.log");
    assert!(
        theirs.len() as u64 > CAP,
        "the person's own file is never cut"
    );
    assert!(!theirs.contains("was cut here"));
    assert_eq!(
        read(&elsewhere, "job.log"),
        "a line of the job's last run\n",
        "the definition's file is never touched"
    );

    let straight = read(&run("straight"), "job.log");
    assert!(straight.len() as u64 <= CAP, "{} > {CAP}", straight.len());
    // A straight line written while the thread cuts can land before the cut's own line.
    assert!(
        straight.contains("was cut here"),
        "the thread's cut says so"
    );
    let straight_unkept = read(&run("straight-unkept"), "job.log");
    assert!(
        straight_unkept.len() as u64 > CAP,
        "the straight control is unbounded"
    );
}

/// The full name of [`a_job_that_execs_keeps_writing_its_file_and_the_new_image_keeps_it`].
const EXECS: &str =
    "job_log::tests::a_job_that_execs_keeps_writing_its_file_and_the_new_image_keeps_it";

/// One stage of a stand-in job that replaces itself with `std`'s own exec, no aterm code
/// between, and the image it becomes.
fn exec_stage(stage: &str, dir: &Path) {
    let log = dir.join("job.log");
    match stage {
        "exec" | "exec-unkept" => {
            seam::open_as_service_manager(&seam::Outputs {
                stdout: Some(log.clone()),
                stderr: Some(log.clone()),
            })
            .unwrap();
            assert!(keep(&[&log]).stderr());
            line(format_args!("before the exec"));
            let after = if stage == "exec" {
                "after"
            } else {
                "after-unkept"
            };
            use std::os::unix::process::CommandExt as _;
            #[expect(
                clippy::disallowed_methods,
                reason = "the exec under test is std's own, which hands a job's log nothing: \
                          the stand-in reads no process table, so no gate is owed"
            )]
            let refused = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", EXECS, "--nocapture", "--test-threads=1"])
                .env(seam::STAGE, after)
                .env(seam::DIR, dir)
                .exec();
            panic!("the exec failed: {refused}");
        }
        // The new image: its standard output and error are still the file. Its own start keeps
        // the file again, so its burst stays under the cap.
        "after" => {
            // On a line of its own: the test harness has begun one on standard output.
            eprintln!();
            line(format_args!("from the image the job became"));
            assert_eq!(
                keep(&[&log]),
                Kept {
                    stdout: true,
                    stderr: true
                }
            );
            for _ in 0..seam::BURST {
                line(format_args!("{REFUSAL}"));
            }
            line(format_args!("the new image's last words"));
        }
        // The control: a new image that keeps nothing itself.
        "after-unkept" => {
            eprintln!();
            line(format_args!("from the image the job became"));
            for _ in 0..seam::BURST {
                line(format_args!("{REFUSAL}"));
            }
        }
        other => panic!("no stage {other}"),
    }
    std::process::exit(0);
}

/// **AN EXEC LEAVES THE LOG ON THE FILE, AND THE NEW IMAGE KEEPS IT BOUNDED.** A stand-in job
/// keeps its file, writes a line and replaces itself with `std`'s `Command::exec` (no aterm code
/// hands anything back): the new image's first line reaches the file, its own `keep` finds the
/// file on both descriptors, and its burst leaves the file under the cap with its last line last.
/// NEGATIVE CONTROL: a new image that does not keep the file itself writes it past the cap: what
/// the old image kept ended with it.
#[test]
fn a_job_that_execs_keeps_writing_its_file_and_the_new_image_keeps_it() {
    if let Some((stage, dir)) = seam::stage() {
        exec_stage(&stage, &dir);
        return;
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let run = |stage: &str| {
        let dir = root.path().join(stage);
        std::fs::create_dir(&dir).unwrap();
        seam::rerun(EXECS, stage, &dir).unwrap();
        std::fs::read_to_string(dir.join("job.log")).unwrap()
    };
    let kept = run("exec");
    assert!(kept.len() as u64 <= CAP, "{} > {CAP}", kept.len());
    assert!(kept.lines().next().unwrap().contains("was cut here"));
    assert_eq!(
        kept.lines().last().map(undated),
        Some("the new image's last words")
    );
    let unkept = run("exec-unkept");
    assert!(unkept.len() as u64 > CAP, "the control is unbounded");
    assert!(
        unkept
            .lines()
            .any(|l| l != "before the exec" && undated(l) == "before the exec"),
        "the old image's line, dated"
    );
    assert!(
        unkept
            .lines()
            .any(|l| undated(l) == "from the image the job became"),
        "the new image writes the same file"
    );
}

/// The full name of [`a_stop_signal_reaches_the_jobs_waiter_and_never_the_log_thread`].
const STOPS: &str =
    "job_log::tests::a_stop_signal_reaches_the_jobs_waiter_and_never_the_log_thread";

/// The signals `process::take_stop_signals` takes.
fn stop_signals() -> libc::sigset_t {
    let mut set = std::mem::MaybeUninit::<libc::sigset_t>::zeroed();
    // SAFETY: a writable local set, initialised by `sigemptyset` before the adds and the read.
    unsafe {
        libc::sigemptyset(set.as_mut_ptr());
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::sigaddset(set.as_mut_ptr(), sig);
        }
        set.assume_init()
    }
}

/// One stage of a stand-in job that keeps its log, then takes its stop signals, then waits.
fn stop_stage(stage: &str, dir: &Path) {
    let stops = stop_signals();
    match stage {
        // This image's libtest threads predate any mask the stage sets, and would take a signal
        // sent to the process themselves: run again in place with the stop signals blocked from
        // the first thread, which every libtest thread then inherits.
        "stop" | "stop-control" => {
            // SAFETY: changes only this thread's mask, from a live set.
            unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &stops, std::ptr::null_mut()) };
            let error = crate::spawnfd::transparent(std::env::current_exe().unwrap())
                .args(std::env::args_os().skip(1))
                .env(seam::STAGE, format!("{stage}-run"))
                .exec();
            panic!("the stand-in's exec: {error}");
        }
        // The job's own thread, as a job's main thread starts (nothing blocked): it keeps its
        // log, then takes its stop signals, then waits for one.
        "stop-run" | "stop-control-run" => {
            // SAFETY: as above.
            unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &stops, std::ptr::null_mut()) };
            let log = dir.join("job.log");
            seam::open_as_service_manager(&seam::Outputs {
                stdout: Some(log.clone()),
                stderr: Some(log.clone()),
            })
            .unwrap();
            assert!(keep(&[&log]).stderr());
            if stage == "stop-control-run" {
                // The control: a thread started here as the log's would be with nothing blocked.
                std::thread::spawn(|| {
                    loop {
                        std::thread::park();
                    }
                });
            }
            assert!(crate::process::take_stop_signals("job-stop", |sig| {
                line(format_args!("stopped by signal {sig}: the orderly end"));
                std::process::exit(0);
            }));
            let ready = dir.join("pid.part");
            std::fs::write(&ready, std::process::id().to_string()).unwrap();
            std::fs::rename(&ready, dir.join("pid")).unwrap();
            std::thread::sleep(Duration::from_secs(60));
            std::process::exit(3);
        }
        other => panic!("no stage {other}"),
    }
}

/// **A STOP SIGNAL REACHES THE JOB'S WAITER, NEVER THE LOG'S THREAD.** A stand-in job (this binary
/// run again, every thread but the job's own with the stop signals blocked, as a job's are) keeps
/// its log, which starts the log's thread, then takes its stop signals
/// (`process::take_stop_signals`, which blocks them only on its own thread and those it starts
/// after) and is sent `SIGTERM`: its waiter takes it and the job ends in order, saying so.
/// NEGATIVE CONTROL (macOS, which hands a process's signal to its first thread that does not
/// block it): the same job with one more thread started before it took its stop signals, as the
/// log's was before its mask, is ended by the signal itself, its orderly end never run.
#[test]
fn a_stop_signal_reaches_the_jobs_waiter_and_never_the_log_thread() {
    if let Some((stage, dir)) = seam::stage() {
        stop_stage(&stage, &dir);
        return;
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let run = |stage: &'static str| {
        let dir = root.path().join(stage);
        std::fs::create_dir(&dir).unwrap();
        let job = {
            let dir = dir.clone();
            std::thread::spawn(move || seam::rerun(STOPS, stage, &dir))
        };
        // Ready, or ended: the rerun's own hang detector (a minute of the stand-in's without
        // progress) ends one that never gets ready.
        let pid = loop {
            if let Ok(pid) = std::fs::read_to_string(dir.join("pid")) {
                break pid.parse::<libc::pid_t>().unwrap();
            }
            if job.is_finished() {
                panic!("{stage}: the stand-in ended unready: {:?}", job.join());
            }
            crate::deadline::poll_yield();
        };
        // SAFETY: a signal to the stand-in this test started, which waits for it.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
        let ended = job.join().unwrap();
        (
            ended,
            std::fs::read_to_string(dir.join("job.log")).unwrap_or_default(),
        )
    };
    let (ended, log) = run("stop");
    ended.expect("the job's waiter took the signal and ended it in order");
    assert_eq!(
        log.lines().last().map(undated),
        Some("stopped by signal 15: the orderly end"),
        "{log}"
    );
    if cfg!(target_vendor = "apple") {
        let (ended, log) = run("stop-control");
        let refused = ended.expect_err("a thread with the stop signals open takes one");
        assert!(refused.to_string().contains("signal: 15"), "{refused}");
        assert!(!log.contains("the orderly end"), "{log}");
    }
}

/// The full name of [`a_log_an_earlier_run_left_past_the_cap_is_cut_back_as_the_job_keeps_it`].
const STARTS: &str =
    "job_log::tests::a_log_an_earlier_run_left_past_the_cap_is_cut_back_as_the_job_keeps_it";

/// **A LOG AN EARLIER RUN LEFT PAST THE CAP IS CUT BACK AS THE JOB KEEPS IT.** A stand-in job
/// whose log an earlier run left past the cap (a job its supervisor restarts sooner than the
/// thread's first cut, writing only straight lines) keeps it and ends at once, writing nothing:
/// the log is under the cap, holding only the line that says it was cut.
/// NEGATIVE CONTROL: the same start without `keep` leaves the earlier run's log as it was.
#[test]
fn a_log_an_earlier_run_left_past_the_cap_is_cut_back_as_the_job_keeps_it() {
    if let Some((stage, dir)) = seam::stage() {
        let log = dir.join("job.log");
        seam::open_as_service_manager(&seam::Outputs {
            stdout: Some(log.clone()),
            stderr: Some(log.clone()),
        })
        .unwrap();
        if stage == "past" {
            assert!(keep(&[&log]).stderr());
        }
        std::process::exit(0);
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let earlier = "an earlier run's straight line\n".repeat(CAP as usize / 20);
    assert!(earlier.len() as u64 > CAP);
    let run = |stage: &str| {
        let dir = root.path().join(stage);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("job.log"), &earlier).unwrap();
        seam::rerun(STARTS, stage, &dir).unwrap();
        std::fs::read_to_string(dir.join("job.log")).unwrap()
    };
    let kept = run("past");
    assert!(kept.len() as u64 <= CAP, "{} > {CAP}", kept.len());
    assert_eq!(kept.lines().count(), 1, "{kept}");
    assert!(kept.contains("was cut here"), "{kept}");
    assert_eq!(
        run("past-unkept"),
        earlier,
        "the control's log is as it was"
    );
}

/// The full name of [`a_line_never_lands_inside_a_line_another_writer_is_making`].
const SERIAL: &str = "job_log::tests::a_line_never_lands_inside_a_line_another_writer_is_making";

/// One stage of a stand-in job that makes one line of two writes under standard error's lock
/// while another thread writes a line of its own.
fn serial_stage(stage: &str, dir: &Path) {
    use std::io::Write as _;
    let log = dir.join("job.log");
    seam::open_as_service_manager(&seam::Outputs {
        stdout: Some(log.clone()),
        stderr: Some(log.clone()),
    })
    .unwrap();
    assert!(keep(&[&log]).stderr());
    let control = stage == "serial-control";
    let mut held = io::stderr().lock();
    held.write_all(b"one line made of ").unwrap();
    let before = std::fs::metadata(&log).unwrap().len();
    let (started, starts) = std::sync::mpsc::channel();
    let other = {
        let log = log.clone();
        std::thread::spawn(move || {
            started.send(()).unwrap();
            if control {
                // The control: the same line written to the file under no lock.
                let mut raw = OpenOptions::new().append(true).open(&log).unwrap();
                raw.write_all(b"a line from another thread\n").unwrap();
            } else {
                line(format_args!("a line from another thread"));
            }
        })
    };
    starts.recv().unwrap();
    let grew = || std::fs::metadata(&log).unwrap().len() > before;
    if control {
        // The control's write must land: a hang detector.
        let hang = std::time::Instant::now() + Duration::from_secs(60);
        while !grew() {
            assert!(std::time::Instant::now() < hang, "the control never wrote");
            crate::deadline::poll_yield();
        }
    } else {
        // Room for the other thread's line to land, were anything to let it: under the lock
        // nothing does, so how long this waits decides nothing the test asserts.
        let room = std::time::Instant::now() + Duration::from_secs(1);
        while !grew() && std::time::Instant::now() < room {
            crate::deadline::poll_yield();
        }
    }
    held.write_all(b"two writes\n").unwrap();
    drop(held);
    other.join().unwrap();
    // Before libtest writes its verdict to standard output, which is the log.
    std::process::exit(0);
}

/// **A LINE NEVER LANDS INSIDE A LINE ANOTHER WRITER IS MAKING.** A stand-in job keeps its log,
/// then makes one line of two writes under standard error's lock (as `eprintln!` and a library's
/// line of several writes do) while another thread calls `line`: that line waits for the lock and
/// follows the whole line, dated.
/// NEGATIVE CONTROL: the other thread's line written to the same file under no lock lands
/// between the two writes.
#[test]
fn a_line_never_lands_inside_a_line_another_writer_is_making() {
    if let Some((stage, dir)) = seam::stage() {
        serial_stage(&stage, &dir);
        return;
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let run = |stage: &str| {
        let dir = root.path().join(stage);
        std::fs::create_dir(&dir).unwrap();
        seam::rerun(SERIAL, stage, &dir).unwrap();
        std::fs::read_to_string(dir.join("job.log")).unwrap()
    };
    let serial = run("serial");
    let lines: Vec<&str> = serial.lines().collect();
    assert_eq!(lines.len(), 2, "{serial}");
    assert_eq!(lines[0], "one line made of two writes");
    assert_ne!(lines[1], "a line from another thread", "dated");
    assert_eq!(undated(lines[1]), "a line from another thread");
    let control = run("serial-control");
    assert_eq!(
        control, "one line made of a line from another thread\ntwo writes\n",
        "the control's line lands inside"
    );
}
