// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::instance::{CONFIG_OFF, World};

#[test]
fn failed_stop_is_recorded_once_and_preserves_world_and_private_evidence() {
    let mut world = World::new(CONFIG_OFF).unwrap();
    let path = world.dir.clone();
    let mut failed = StopRecord::new(&path, world.cleanup_failed.clone(), 4242);
    assert!(
        failed
            .attempt(|| Err("registration unknown".into()))
            .is_err()
    );
    let evidence = failed.evidence.clone().unwrap();
    let result = aterm_uds::private_file::read_private(&evidence, "result.txt", 4096).unwrap();
    assert!(
        std::str::from_utf8(&result)
            .unwrap()
            .contains("state=unknown\nerror=registration unknown")
    );
    assert_eq!(
        failed.attempt(|| panic!("no repeated cleanup timeout")),
        Err("registration unknown".into())
    );
    let mut other = StopRecord::new(&path, world.cleanup_failed.clone(), 4343);
    other.attempt(|| Ok(())).unwrap();
    world.release();
    drop(world);
    assert!(
        path.is_dir(),
        "another successful stop must not discard failed evidence"
    );
    assert_eq!(
        aterm_uds::private_file::read_private(&evidence, "result.txt", 4096).unwrap(),
        result
    );
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn successful_stop_is_idempotent_and_allows_normal_world_removal() {
    let world = World::new(CONFIG_OFF).unwrap();
    let path = world.dir.clone();
    let mut stopped = StopRecord::new(&path, world.cleanup_failed.clone(), 4242);
    stopped.attempt(|| Ok(())).unwrap();
    stopped
        .attempt(|| panic!("completed stop is not rerun by Drop"))
        .unwrap();
    assert!(stopped.evidence.is_none());
    drop(world);
    assert!(!path.exists());
}

#[test]
fn missing_handoff_scope_still_stops_the_original_and_keeps_evidence() {
    let world = World::new(CONFIG_OFF).unwrap();
    let path = world.dir.clone();
    let mut stop = StopRecord::new(&path, world.cleanup_failed.clone(), 4242);
    stop.require_evidence("successor ownership unavailable".into());
    let mut original_stopped = false;
    assert!(
        stop.attempt(|| {
            original_stopped = true;
            Ok(())
        })
        .is_err()
    );
    assert!(original_stopped);
    assert!(stop.evidence.is_some());
    assert!(
        stop.attempt(|| panic!("owned cleanup must not run twice"))
            .is_err()
    );
    drop(world);
    assert!(path.is_dir());
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn cleanup_does_not_replace_primary_failure_or_signal() {
    let primary = Fail {
        msg: Some("work failed".into()),
        code: 143,
        stopped: Some(15),
    };
    let combined = after_stop::<()>(Err(primary), Err("cleanup unknown".into())).unwrap_err();
    assert_eq!(combined.code, 143);
    assert_eq!(combined.stopped, Some(15));
    let message = combined.msg.unwrap();
    assert!(message.contains("work failed") && message.contains("cleanup unknown"));
    assert!(after_stop(Ok(42), Err("cleanup unknown".into())).is_err());
    assert_eq!(after_stop(Ok(42), Ok(())).unwrap(), 42);
}

/// The role a stop test's re-run of this binary plays: its probe, or the probe's stand-in.
const PROBE: &str = "ATERM_LAB_INSTANCE_STOP_PROBE";

/// The stand-in's line once it holds its lifeline: a stop from then on finds it waiting for
/// the lifeline's end, its own start behind it.
const LIFELINE_HELD: &str = "PROBE the lifeline is held";

/// The stand-in's line once it has read its lifeline's end, which only the stop cuts.
const CUT_SEEN: &str = "PROBE the lifeline was cut";

/// A windowed instance's aterm stand-in's line once it runs.
const GUI_UP: &str = "PROBE the aterm runs";

/// Run this binary's `test` alone as `role`: its exit status and its output.
fn rerun(test: &str, role: &str) -> (Option<i32>, String) {
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    cmd.args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(PROBE, role);
    let out = aterm_uds::deadline::output_until(
        &mut cmd,
        std::time::Instant::now() + std::time::Duration::from_mins(2),
    )
    .expect("the probe ends within the hang detector");
    (
        out.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The stand-in's part. `gui`, a windowed instance's aterm, says it runs and stays until its
/// probe is gone. The others wait for their lifeline's end, as a headless aterm or a keeper
/// does, say so, then end after a moment (`ends`) or stay until their probe is gone (`stays`).
fn stand_in(role: &str) -> ! {
    let probe = std::os::unix::process::parent_id();
    if role == "gui" {
        println!("{GUI_UP}");
    } else {
        let lifeline = aterm_uds::lifeline::adopt(aterm_uds::lifeline::STDIN).expect("lifeline");
        println!("{LIFELINE_HELD}");
        let _ = aterm_uds::lifeline::wait_for_cut(lifeline);
        println!("{CUT_SEEN}");
    }
    let end =
        std::time::Instant::now() + std::time::Duration::from_secs(aterm_spec::wait::PARKED_SECS);
    if role == "ends" {
        std::thread::sleep(std::time::Duration::from_millis(500));
    } else {
        while std::os::unix::process::parent_id() == probe && std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    std::process::exit(0);
}

/// An instance in `world` whose process is this binary's `test` as the stand-in `role`, on
/// its lifeline in a session of its own, as [`super::super::Instance::start`] starts one,
/// once the stand-in holds its lifeline, so a stop's graces time its end and not its start;
/// with the rest of the stand-in's stdout. Its stop gives it the one-minute hang detector
/// ([`aterm_spec::wait::MUST_HAPPEN`]) to end by itself, not a lab instance's 8 s, so no stall
/// of the stand-in turns its own end into the stop's `SIGTERM`.
fn instance_of(world: &World, test: &str, role: &str) -> (super::super::Instance, Said) {
    use aterm_uds::spawnfd::Stdio;
    use std::io::BufRead as _;
    let exe = std::env::current_exe().expect("test binary");
    let (lifeline, far) = aterm_uds::lifeline::Lifeline::open(&world.dir).expect("lifeline");
    let mut child = aterm_uds::spawnfd::command(&exe)
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(PROBE, role)
        .setsid(true)
        .stdin(Stdio::from(far))
        .stdout(Stdio::piped())
        .spawn()
        .expect("the stand-in starts");
    let mut said = std::io::BufReader::new(child.stdout.take().expect("the stand-in's stdout"));
    let held = (&mut said)
        .lines()
        .map_while(Result::ok)
        .any(|line| line.ends_with(LIFELINE_HELD));
    assert!(held, "the stand-in said it holds its lifeline");
    let pid = i32::try_from(child.id()).expect("pid");
    let instance = super::super::Instance {
        child: super::super::Proc::Child(child),
        lifeline: Some(lifeline),
        pid,
        kept: false,
        aterm: exe.clone(),
        client: exe,
        sock: world.sock.clone(),
        log: world.dir.join("aterm.log"),
        env: world.env(),
        _admission: None,
        cleanup: StopRecord::new(&world.dir, world.cleanup_failed.clone(), pid),
        gui: None,
        handoff: None,
        graces: super::super::Graces {
            own: aterm_spec::wait::MUST_HAPPEN,
            ..super::super::STOP_GRACES
        },
    };
    (instance, said)
}

/// A windowed instance's aterm: this binary's `test` as the `gui` stand-in, once it has said
/// it runs, and its identity.
fn gui_of(test: &str) -> (aterm_uds::spawnfd::Child, aterm_uds::process::Identity) {
    use std::io::BufRead as _;
    let mut gui = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(PROBE, "gui")
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("the aterm stand-in starts");
    let said = gui.stdout.take().expect("the aterm stand-in's stdout");
    let up = std::io::BufReader::new(said)
        .lines()
        .map_while(Result::ok)
        .any(|line| line.ends_with(GUI_UP));
    assert!(up, "the aterm stand-in said it runs");
    let pid = i32::try_from(gui.id()).expect("pid");
    let identity = aterm_uds::process::read_identity(pid).expect("the aterm's identity");
    (gui, identity)
}

/// A stand-in's stdout, past what [`instance_of`] read of it.
type Said = std::io::BufReader<std::process::ChildStdout>;

/// Whether the stand-in said it read its lifeline's end, read to the end of its output.
fn heard_cut(said: Said) -> std::thread::JoinHandle<bool> {
    use std::io::BufRead as _;
    std::thread::spawn(move || {
        said.lines()
            .map_while(Result::ok)
            .any(|line| line.ends_with(CUT_SEEN))
    })
}

/// Whether the instance's own process ended by itself (exit status 0), as the stop left it.
fn own_end(instance: &mut super::super::Instance) -> Option<bool> {
    match &mut instance.child {
        super::super::Proc::Child(child) => child
            .try_wait()
            .expect("observe the stand-in")
            .map(|status| status.success()),
        super::super::Proc::Job(_) => None,
    }
}

/// Raise a stop in this probe as a signal to it does, once it catches them.
fn raise_stop() -> i32 {
    crate::sys::start_signal(libc::SIGTERM, false);
    crate::stop::install();
    let me = i32::try_from(std::process::id()).expect("pid");
    crate::sys::kill(me, libc::SIGTERM).expect("the stop's signal");
    assert_eq!(
        crate::stop::sleep(std::time::Duration::from_mins(1)),
        Err(crate::stop::Stopped(libc::SIGTERM)),
        "the stop is noted"
    );
    me
}

/// `lab instance`'s hold and end, on a stand-in instance: the signal ends the hold, and the
/// stop waits through it for the instance's own end, its lifeline cut, and says it came.
#[test]
fn a_stop_waits_through_the_signal_that_asked_for_it_for_the_instances_end() {
    const TEST: &str = "instance::cleanup::tests::a_stop_waits_through_the_signal_that_asked_for_it_for_the_instances_end";
    match std::env::var(PROBE).as_deref() {
        Ok("probe") => {
            let world = World::new(CONFIG_OFF).unwrap();
            let dir = world.dir.clone();
            let (mut instance, said) = instance_of(&world, TEST, "ends");
            let heard = heard_cut(said);
            raise_stop();
            let work = super::super::hold(&mut instance);
            let ended = super::super::end(&mut instance, work);
            let own = own_end(&mut instance);
            let heard = heard.join().expect("the stand-in's output");
            drop(instance);
            drop(world);
            let removed = !dir.exists();
            let _ = std::fs::remove_dir_all(&dir);
            println!(
                "PROBE ended={ended:?} own_end={own:?} cut_seen={heard} world_removed={removed}"
            );
            std::process::exit(0);
        }
        Ok(role) => stand_in(role),
        Err(_) => {}
    }
    let (code, out) = rerun(TEST, "probe");
    assert_eq!(code, Some(0), "{out}");
    assert!(
        out.contains(&format!("instance: ended (signal {})", libc::SIGTERM))
            && out
                .contains("PROBE ended=Ok(0) own_end=Some(true) cut_seen=true world_removed=true"),
        "the stop waits through the signal that asked for it until the instance has ended by \
         itself on its lifeline's cut, says it ended and removes its world: {out}"
    );
}

/// A windowed stand-in instance's stop, a second signal sent once the stop has reaped the
/// keeper (which ends on its lifeline's cut): the stop is then waiting for the aterm's
/// absence, under its one-minute deadline alone. With `gui_ends`, the aterm is ended just
/// before that signal, which the stop's last look still sees.
fn windowed_cut(test: &str, gui_ends: bool) -> ! {
    use aterm_uds::process::{ProcessPresence, process_presence};
    let world = World::new(CONFIG_OFF).unwrap();
    let dir = world.dir.clone();
    let (mut gui, aterm) = gui_of(test);
    let (mut instance, _said) = instance_of(&world, test, "ends");
    instance.kept = true;
    instance.gui = Some(aterm);
    let keeper = instance.pid;
    // As `lab instance` holds it: a second signal that comes once the end is established
    // cuts nothing and ends nothing.
    let _ahead = crate::stop::cleanup_ahead();
    let me = raise_stop();
    let second = std::thread::spawn(move || {
        let hang = std::time::Instant::now() + std::time::Duration::from_mins(1);
        while process_presence(keeper) != ProcessPresence::Absent {
            if std::time::Instant::now() >= hang {
                return (gui, false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        if gui_ends {
            gui.kill().expect("end the aterm stand-in");
            gui.wait().expect("reap the aterm stand-in");
        }
        (gui, crate::sys::kill(me, libc::SIGTERM).is_ok())
    });
    let work = super::super::hold(&mut instance);
    let ended = super::super::end(&mut instance, work);
    let (mut gui, second) = second.join().expect("the second signal's thread");
    let presence = process_presence(aterm.pid);
    let _ = gui.kill();
    gui.wait().expect("reap the aterm stand-in");
    drop(instance);
    drop(world);
    let kept = dir.is_dir();
    let _ = std::fs::remove_dir_all(&dir);
    let said = match &ended {
        Ok(code) => format!("Ok({code})"),
        Err(fail) => format!("Err({}: {})", fail.code, fail.msg.as_deref().unwrap_or("")),
    };
    let truthful = said.contains(&format!(
        "signal {} cut the GUI's native absence short; its keeper's pid {keeper} has ended \
         (exit status: 0), its aterm's pid {} still runs",
        libc::SIGTERM,
        aterm.pid
    ));
    println!(
        "PROBE second={second} truthful={truthful} presence={presence:?} world_kept={kept} \
         ended={said}"
    );
    std::process::exit(0);
}

#[test]
fn a_second_signal_cuts_the_stops_wait_short_and_it_says_what_still_runs() {
    const TEST: &str = "instance::cleanup::tests::a_second_signal_cuts_the_stops_wait_short_and_it_says_what_still_runs";
    match std::env::var(PROBE).as_deref() {
        Ok("probe") => windowed_cut(TEST, false),
        Ok(role) => stand_in(role),
        Err(_) => {}
    }
    let (code, out) = rerun(TEST, "probe");
    assert_eq!(
        code,
        Some(0),
        "a second signal during a stop cuts its wait short, not the run: {out}"
    );
    assert!(
        out.contains("PROBE second=true truthful=true presence=Present world_kept=true ended=Err(1: instance: signal"),
        "the cut stop fails the run saying its keeper ended and its aterm still runs, and \
         keeps its world: {out}"
    );
}

#[test]
fn an_end_that_comes_with_the_second_signal_is_still_established() {
    const TEST: &str =
        "instance::cleanup::tests::an_end_that_comes_with_the_second_signal_is_still_established";
    match std::env::var(PROBE).as_deref() {
        Ok("probe") => windowed_cut(TEST, true),
        Ok(role) => stand_in(role),
        Err(_) => {}
    }
    let (code, out) = rerun(TEST, "probe");
    assert_eq!(code, Some(0), "{out}");
    assert!(
        out.contains(
            "PROBE second=true truthful=false presence=Absent world_kept=false ended=Ok(0)"
        ),
        "the cut stop looks once more, finds the aterm ended and so the instance's end: {out}"
    );
}

/// A second signal before `lab instance` has acted on the first: it cuts the end's wait
/// short, and the end says what it left, where without its cleanup ahead the second would
/// end the run unsaid. Each is a signal of its own, sent once the one before it was read.
#[test]
fn a_second_signal_before_the_end_begins_cuts_it_short_and_it_says_what_still_runs() {
    const TEST: &str = "instance::cleanup::tests::a_second_signal_before_the_end_begins_cuts_it_short_and_it_says_what_still_runs";
    match std::env::var(PROBE).as_deref() {
        Ok("probe") => {
            let world = World::new(CONFIG_OFF).unwrap();
            let dir = world.dir.clone();
            let (mut instance, _said) = instance_of(&world, TEST, "stays");
            crate::sys::start_signal(libc::SIGINT, false);
            crate::sys::start_signal(libc::SIGTERM, false);
            crate::stop::install();
            let _ahead = crate::stop::cleanup_ahead();
            let me = i32::try_from(std::process::id()).expect("pid");
            crate::sys::kill(me, libc::SIGINT).expect("the stop");
            assert_eq!(
                crate::stop::sleep(std::time::Duration::from_mins(1)),
                Err(crate::stop::Stopped(libc::SIGINT)),
                "the stop is noted"
            );
            crate::sys::kill(me, libc::SIGTERM).expect("the second");
            // Read before the end begins: the second names the run's stop now.
            let hang = std::time::Instant::now() + std::time::Duration::from_mins(1);
            while crate::stop::check() != Err(crate::stop::Stopped(libc::SIGTERM)) {
                assert!(
                    std::time::Instant::now() < hang,
                    "the second signal was never read"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let work = super::super::hold(&mut instance);
            let ended = super::super::end(&mut instance, work);
            let pid = instance.pid;
            let presence = aterm_uds::process::process_presence(pid);
            crate::sys::kill(pid, libc::SIGKILL).expect("end the stand-in the stop left");
            if let super::super::Proc::Child(child) = &mut instance.child {
                child.wait().expect("reap the stand-in");
            }
            drop(instance);
            drop(world);
            let kept = dir.is_dir();
            let _ = std::fs::remove_dir_all(&dir);
            let expected = format!(
                "instance: signal {term}, and its end is not established: signal {term} cut \
                 the child grace short; the instance's pid {pid} still runs",
                term = libc::SIGTERM
            );
            let truthful = ended
                .as_ref()
                .err()
                .is_some_and(|fail| fail.code == 1 && fail.msg.as_ref() == Some(&expected));
            println!(
                "PROBE truthful={truthful} presence={presence:?} world_kept={kept} \
                 ended={ended:?}"
            );
            std::process::exit(0);
        }
        Ok(role) => stand_in(role),
        Err(_) => {}
    }
    let (code, out) = rerun(TEST, "probe");
    assert_eq!(
        code,
        Some(0),
        "the second signal cut the end short instead of ending the run: {out}"
    );
    assert!(
        out.contains("PROBE truthful=true presence=Present world_kept=true"),
        "the end says the instance's pid still runs and keeps its world: {out}"
    );
}

/// A windowed instance's aterm that ends between the two reads of its pid that a stop's wait
/// for its absence makes, and that its standing makes: each says it ended, neither that its
/// identity changed.
#[test]
fn an_aterm_that_ends_between_the_reads_of_its_pid_has_ended() {
    use super::super::{BETWEEN_READS, Now, Proc, standing, wait_gui_absent};
    const TEST: &str =
        "instance::cleanup::tests::an_aterm_that_ends_between_the_reads_of_its_pid_has_ended";
    if let Ok(role) = std::env::var(PROBE) {
        stand_in(&role);
    }
    let ends = |mut gui: aterm_uds::spawnfd::Child| -> Box<dyn FnOnce()> {
        Box::new(move || {
            gui.kill().expect("end the aterm stand-in");
            gui.wait().expect("reap the aterm stand-in");
        })
    };
    let (gui, aterm) = gui_of(TEST);
    BETWEEN_READS.set(Some(ends(gui)));
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    assert_eq!(
        wait_gui_absent(true, Some(aterm), deadline),
        Ok(()),
        "the stop's wait establishes the aterm's end"
    );
    // A keeper that still runs, beside an aterm that ends between the reads.
    let (keeper, _) = gui_of(TEST);
    let pid = i32::try_from(keeper.id()).expect("pid");
    let mut keeper = Proc::Child(keeper);
    let (gui, aterm) = gui_of(TEST);
    BETWEEN_READS.set(Some(ends(gui)));
    let said = standing(&mut keeper, pid, true, Some(aterm));
    if let Proc::Child(keeper) = &mut keeper {
        keeper.kill().expect("end the keeper stand-in");
        keeper.wait().expect("reap the keeper stand-in");
    }
    assert!(
        said.ends_with(&format!("its aterm's pid {} {}", aterm.pid, Now::Ended)),
        "the stop's standing says the aterm ended: {said}"
    );
}

/// A stop that fails with no signal fails the run (a frame taken, its instance left), where
/// a stop left to the instance's drop would say so on stderr alone and the run exit 0.
#[test]
fn a_stop_that_fails_without_a_signal_fails_the_run() {
    const TEST: &str = "instance::cleanup::tests::a_stop_that_fails_without_a_signal_fails_the_run";
    match std::env::var(PROBE).as_deref() {
        Ok("probe") => {
            let world = World::new(CONFIG_OFF).unwrap();
            let dir = world.dir.clone();
            // A windowed instance whose keeper has no aterm: no stop can establish its
            // aterm's end.
            let (mut instance, _said) = instance_of(&world, TEST, "ends");
            instance.kept = true;
            let ended = super::super::end(&mut instance, Ok(0));
            drop(instance);
            drop(world);
            let kept = dir.is_dir();
            let _ = std::fs::remove_dir_all(&dir);
            let failed = ended.as_ref().err().is_some_and(|fail| {
                fail.code == 1
                    && fail.msg.as_deref().is_some_and(|msg| {
                        msg.starts_with("cleanup failed: original GUI identity was not established")
                    })
            });
            println!("PROBE failed={failed} world_kept={kept} ended={ended:?}");
            std::process::exit(0);
        }
        Ok(role) => stand_in(role),
        Err(_) => {}
    }
    let (code, out) = rerun(TEST, "probe");
    assert_eq!(code, Some(0), "{out}");
    assert!(
        out.contains("PROBE failed=true world_kept=true"),
        "the run fails with the stop's failure and keeps the world: {out}"
    );
}

#[test]
fn a_process_runs_by_its_pid_only_while_the_pid_is_still_its_own() {
    use super::super::{Now, aterm_now, now_of};
    use aterm_uds::process::ProcessPresence::{Absent, Exited, Present, Unknown};
    for same in [Some(true), Some(false), None] {
        assert_eq!(now_of(Absent, same), Now::Ended);
        assert_eq!(now_of(Unknown, same), Now::Unseen);
    }
    assert_eq!(now_of(Present, Some(true)), Now::Runs);
    assert_eq!(now_of(Exited, Some(true)), Now::Exited);
    assert_eq!(now_of(Exited, None), Now::Exited);
    assert_eq!(
        now_of(Present, Some(false)),
        Now::Reused,
        "a pid that names another process says its own ended"
    );
    assert_eq!(now_of(Exited, Some(false)), Now::Reused);
    assert_eq!(now_of(Present, None), Now::Unreadable);
    let me = i32::try_from(std::process::id()).expect("pid");
    let aterm = aterm_uds::process::read_identity(me).expect("this process's identity");
    let next = aterm_uds::process::Identity {
        birth: aterm_uds::process::Birth(aterm.birth.0 + 1),
        ..aterm
    };
    assert_eq!(aterm_now(aterm, Present, Some(aterm)), Now::Runs);
    assert_eq!(
        aterm_now(aterm, Present, Some(next)),
        Now::Reused,
        "an aterm whose pid another process was given since has ended"
    );
    assert_eq!(aterm_now(aterm, Present, None), Now::Unreadable);
}

#[test]
fn keeper_absence_does_not_close_a_live_unknown_or_unidentified_gui() {
    use aterm_uds::process::ProcessPresence::{Absent, Exited, Present, Unknown};
    assert!(!super::super::gui_closed(true, true, Present).unwrap());
    assert!(!super::super::gui_closed(true, true, Exited).unwrap());
    assert!(super::super::gui_closed(true, true, Unknown).is_err());
    assert!(super::super::gui_closed(true, false, Absent).is_err());
    assert!(super::super::gui_closed(true, true, Absent).unwrap());
    assert!(super::super::gui_closed(false, false, Unknown).unwrap());
}
