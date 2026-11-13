// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::items::defaults::tests::user;
use crate::port::{Fake, Ran, Stat};

#[test]
fn activation_obligation_survives_a_failed_or_interrupted_write_until_finished() {
    let who = user("alice", 501);
    let mut sys = Fake::root();
    sys.euid = who.uid;
    assert!(!pending(&sys, &who, "dock").unwrap());
    begin(&sys, &who, "dock").unwrap();
    assert!(pending(&sys, &who, "dock").unwrap());
    assert!(!pending(&sys, &who, "widgets").unwrap());
    begin(&sys, &who, "dock").unwrap();
    finish(&sys, &who, "dock").unwrap();
    assert!(!pending(&sys, &who, "dock").unwrap());
}

#[test]
fn linked_foreign_or_malformed_state_never_turns_into_success_or_an_overwrite() {
    let who = user("alice", 501);
    for (uid, symlink, mode) in [(0, false, 0o600), (501, true, 0o600), (501, false, 0o644)] {
        let sys = Fake::root();
        let marker = path(&who, "dock").unwrap();
        sys.stat_of(
            marker.to_str().unwrap(),
            Stat {
                uid,
                gid: 20,
                mode,
                dir: false,
                symlink,
                len: RECORD.len() as u64,
            },
        );
        assert!(pending(&sys, &who, "dock").is_err());
        assert!(begin(&sys, &who, "dock").is_err());
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains(" write ") && !call.starts_with("mkdir "))
        );
    }
    let mut sys = Fake::root();
    sys.euid = who.uid;
    begin(&sys, &who, "dock").unwrap();
    sys.write_user_file(&who, &path(&who, "dock").unwrap(), b"unknown marker", 0o600)
        .unwrap();
    assert!(pending(&sys, &who, "dock").is_err());
    assert!(finish(&sys, &who, "dock").is_err());
}

#[test]
fn linked_foreign_or_shared_parent_cannot_receive_an_activation_marker() {
    let who = user("alice", 501);
    for (uid, symlink, mode) in [(0, false, 0o700), (501, true, 0o700), (501, false, 0o777)] {
        let sys = Fake::root();
        sys.stat_of(
            "/Users//alice/Library/Application Support/aterm",
            Stat {
                uid,
                gid: 20,
                mode,
                dir: true,
                symlink,
                len: 0,
            },
        );
        assert!(begin(&sys, &who, "dock").is_err());
        assert!(sys.calls().is_empty());
    }
}

#[test]
fn a_successful_kill_request_cannot_hide_a_surviving_old_consumer_or_interrupted_wait() {
    let who = user("alice", 501);
    for interrupted in [false, true] {
        let mut sys = Fake::root();
        sys.euid = who.uid;
        begin(&sys, &who, "dock").unwrap();
        sys.on("alice$ /usr/bin/pgrep -u 501 -x Dock", Ran::ok("123\n"));
        sys.alive(123);
        sys.on("/bin/launchctl print gui/501", Ran::ok(""));
        sys.on("alice@gui$ /usr/bin/killall -u alice Dock", Ran::ok(""));
        // A Ctrl-C the run caught reads at the first look after a sleep.
        sys.stop_after = interrupted.then_some(0);
        let answers =
            relaunch_settled(&sys, &who, Session::Ask, &["Dock"], |_| Ok::<_, String>(()));
        assert!(answers[0].is_err(), "{answers:?}");
        assert!(pending(&sys, &who, "dock").unwrap());
        assert_eq!(kills(&sys), 1);
        assert_eq!(sleeps(&sys), if interrupted { 1 } else { TICKS - 1 });
        assert_eq!(spawned_sleeps(&sys), 0);
    }
}

#[test]
fn missing_consumer_needs_no_launch_and_unproven_census_cannot_clear_pending_work() {
    let who = user("alice", 501);
    for output in [None, Some("not-a-pid"), Some(""), Some("123\n123\n")] {
        let mut sys = Fake::root();
        sys.euid = who.uid;
        begin(&sys, &who, "widgets").unwrap();
        let census = "alice$ /usr/bin/pgrep -u 501 -x NotificationCenter";
        sys.on(
            census,
            output.map_or_else(
                || Ran {
                    status: Some(1),
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                },
                Ran::ok,
            ),
        );
        let mut answers =
            relaunch_settled(&sys, &who, Session::Ask, &["NotificationCenter"], |_| {
                Ok::<_, String>(())
            });
        let result = answers.remove(0);
        if output.is_none() {
            result.unwrap();
            finish(&sys, &who, "widgets").unwrap();
            assert!(!pending(&sys, &who, "widgets").unwrap());
        } else {
            assert!(result.is_err());
            assert!(pending(&sys, &who, "widgets").unwrap());
        }
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("launchctl") && !call.contains("killall"))
        );
    }
    let mut sys = Fake::root();
    sys.euid = who.uid;
    begin(&sys, &who, "dock").unwrap();
    let census = "alice$ /usr/bin/pgrep -u 501 -x Dock";
    sys.on(census, Ran::ok("123\n"));
    sys.refuse_complete_stdout(census, "incomplete capture");
    let answers = relaunch_settled(&sys, &who, Session::Ask, &["Dock"], |_| Ok::<_, String>(()));
    assert!(answers[0].is_err(), "{answers:?}");
    assert!(pending(&sys, &who, "dock").unwrap());
    assert!(sys.calls().iter().all(|call| !call.contains("killall")));
}

/// A relaunched consumer counts as up only once its old process exited, a new one
/// runs, and what it reads stopped changing across a poll under that same process: a
/// Dock still starting can write what it read back.
#[test]
fn a_relaunched_consumer_is_up_once_it_runs_again_and_what_it_reads_is_still() {
    let who = user("alice", 501);
    let census = "alice$ /usr/bin/pgrep -u 501 -x Dock";
    let script = |sys: &Fake, after: Ran| {
        sys.on(census, Ran::ok("123\n"));
        sys.on(census, after);
        sys.on("/bin/launchctl print gui/501", Ran::ok(""));
        sys.on("alice@gui$ /usr/bin/killall -u alice Dock", Ran::ok(""));
    };
    let sys = Fake::root();
    script(&sys, Ran::ok("124\n"));
    let reads = std::sync::Mutex::new(vec![1, 2, 2, 2]);
    let answers = relaunch_settled(&sys, &who, Session::Ask, &["Dock"], |_| {
        Ok::<_, String>(reads.lock().unwrap().remove(0))
    });
    assert_eq!(answers, [Ok(())]);
    assert_eq!(sleeps(&sys), 2 * LOOK_EVERY);
    let sys = Fake::root();
    script(
        &sys,
        Ran {
            status: Some(1),
            stdout: Vec::new(),
            stderr: Vec::new(),
        },
    );
    let why = relaunch_settled(&sys, &who, Session::Ask, &["Dock"], |_| Ok::<_, String>(()))
        .remove(0)
        .unwrap_err();
    assert!(why.contains("has not started again"), "{why}");
    assert_eq!(sleeps(&sys), TICKS - 1);
    let sys = Fake::root();
    script(&sys, Ran::ok("124\n"));
    let count = std::sync::atomic::AtomicU32::new(0);
    let why = relaunch_settled(&sys, &who, Session::Ask, &["Dock"], |_| {
        Ok::<_, String>(count.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
    })
    .remove(0)
    .unwrap_err();
    assert!(why.contains("Dock still rewrites what it reads"), "{why}");
    assert_eq!(sleeps(&sys), TICKS - 1);
    assert_eq!(kills(&sys), 1);
    let sys = Fake::root();
    script(&sys, Ran::ok("124\n"));
    let looks = std::sync::Mutex::new(vec![Err("tiles"), Err("tiles"), Ok(()), Ok(())]);
    let answers = relaunch_settled(&sys, &who, Session::Ask, &["Dock"], |_| {
        looks.lock().unwrap().remove(0).map_err(str::to_owned)
    });
    assert_eq!(answers, [Ok(())]);
    assert_eq!(sleeps(&sys), 3 * LOOK_EVERY);
    assert_eq!(spawned_sleeps(&sys), 0);
}

/// A `killall` that exits 1 after the census saw the process (a refused kill, or a
/// process that exited on its own first) vouches for nothing: the old process is
/// waited on as after any kill, so one that keeps running fails the activation, and one
/// that did exit counts only once its successor is up and still. Negative control: an
/// answer taken from `killall`'s status alone returns `Ok` with no wait.
#[test]
fn a_kill_that_signalled_not_every_process_is_waited_on_like_any_other() {
    let who = user("alice", 501);
    let census = "alice$ /usr/bin/pgrep -u 501 -x Dock";
    let script = |sys: &Fake| {
        sys.on(census, Ran::ok("123\n"));
        sys.on("/bin/launchctl print gui/501", Ran::ok(""));
        sys.on(
            "alice@gui$ /usr/bin/killall -u alice Dock",
            Ran::exit(1, "kill -TERM 123: Operation not permitted"),
        );
    };
    let sys = Fake::root();
    script(&sys);
    sys.alive(123);
    let why = relaunch_settled(&sys, &who, Session::Ask, &["Dock"], |_| Ok::<_, String>(()))
        .remove(0)
        .unwrap_err();
    assert!(
        why.contains("the previous Dock process has not exited")
            && why.ends_with("activation remains pending"),
        "{why}"
    );
    assert_eq!(sleeps(&sys), TICKS - 1);
    assert_eq!(kills(&sys), 1);

    let sys = Fake::root();
    script(&sys);
    sys.on(census, Ran::ok("124\n"));
    let reads = std::sync::Mutex::new(vec![1, 2, 2]);
    let answers = relaunch_settled(&sys, &who, Session::Ask, &["Dock"], |_| {
        Ok::<_, String>(reads.lock().unwrap().remove(0))
    });
    assert_eq!(answers, [Ok(())]);
    assert_eq!(sleeps(&sys), 2 * LOOK_EVERY);
    assert!(reads.lock().unwrap().is_empty());
}

/// Two equal reads count only under the same new processes: a consumer that started
/// again between them (a crash right after its start) is looked at again.
#[test]
fn two_equal_reads_settle_a_consumer_only_under_the_same_new_process() {
    let who = user("alice", 501);
    let census = "alice$ /usr/bin/pgrep -u 501 -x Dock";
    let sys = Fake::root();
    sys.on(census, Ran::ok("123\n"));
    sys.on(census, Ran::ok("124\n"));
    sys.on(census, Ran::ok("125\n"));
    sys.on("/bin/launchctl print gui/501", Ran::ok(""));
    sys.on("alice@gui$ /usr/bin/killall -u alice Dock", Ran::ok(""));
    let answers = relaunch_settled(&sys, &who, Session::Ask, &["Dock"], |_| {
        Ok::<_, String>("pins")
    });
    assert_eq!(answers, [Ok(())]);
    assert_eq!(sleeps(&sys), 2 * LOOK_EVERY);
}

/// Each consumer gets its own answer, and every one that was relaunched is waited on
/// whatever another does: a census that fails or a relaunch that cannot be made (it
/// signalled nothing, so nothing of it is awaited) does not cut short the wait for one
/// already killed, which a rollback would otherwise relaunch while it starts.
#[test]
fn every_relaunched_consumer_is_waited_on_whatever_another_does() {
    let who = user("alice", 501);
    let sys = Fake::root();
    sys.on("alice$ /usr/bin/pgrep -u 501 -x Dock", Ran::ok("123\n"));
    sys.on("alice$ /usr/bin/pgrep -u 501 -x Dock", Ran::ok("124\n"));
    sys.on("alice$ /usr/bin/pgrep -u 501 -x Finder", Ran::ok("200\n"));
    sys.refuse_complete_stdout("alice$ /usr/bin/pgrep -u 501 -x Finder", "incomplete");
    sys.on(
        "alice$ /usr/bin/pgrep -u 501 -x NotificationCenter",
        Ran::ok("300\n"),
    );
    sys.on("/bin/launchctl print gui/501", Ran::ok(""));
    sys.on("alice@gui$ /usr/bin/killall -u alice Dock", Ran::ok(""));
    sys.on(
        "alice@gui$ /usr/bin/killall -u alice NotificationCenter",
        Ran::exit(2, "injected"),
    );
    let looked = std::sync::Mutex::new(Vec::new());
    let answers = relaunch_settled(&sys, &who, Session::Ask, &RELAUNCHED, |process| {
        looked.lock().unwrap().push(process.to_owned());
        Ok::<_, String>(())
    });
    assert_eq!(answers[0], Ok(()));
    let finder = answers[1].clone().unwrap_err();
    assert!(
        finder.starts_with("cannot read every Finder process"),
        "{finder}"
    );
    let center = answers[2].clone().unwrap_err();
    assert!(
        center.contains("killall -u alice NotificationCenter"),
        "{center}"
    );
    assert_eq!(*looked.lock().unwrap(), ["Dock", "Dock"]);
    assert_eq!(sleeps(&sys), LOOK_EVERY);
}

/// While the old process runs, the wait looks at it in this process alone: each tick is
/// one in-process sleep, and neither a census nor a `sleep` child runs until it exited.
/// Negative control: the old wait spawned `/bin/sleep 1` per look and read the census each
/// time.
#[test]
fn an_old_process_still_running_is_waited_on_in_process_without_a_spawn() {
    let who = user("alice", 501);
    let census = "alice$ /usr/bin/pgrep -u 501 -x Dock";
    let sys = Fake::root();
    sys.on(census, Ran::ok("123\n"));
    sys.on(census, Ran::ok("124\n"));
    sys.alive(123);
    sys.on("/bin/launchctl print gui/501", Ran::ok(""));
    sys.on("alice@gui$ /usr/bin/killall -u alice Dock", Ran::ok(""));
    let ticks = std::sync::atomic::AtomicU32::new(0);
    sys.on_sleep(move |fake, period| {
        assert_eq!(period, TICK);
        if ticks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == 3 {
            fake.exited(123);
        }
    });
    let answers = relaunch_settled(&sys, &who, Session::Ask, &["Dock"], |_| {
        Ok::<_, String>("pins")
    });
    assert_eq!(answers, [Ok(())]);
    let calls = sys.calls();
    let killed = calls.iter().position(|c| c.contains("killall")).unwrap();
    let looked = killed
        + 1
        + calls[killed + 1..]
            .iter()
            .position(|c| c == census)
            .unwrap();
    assert_eq!(
        calls[killed + 1..looked],
        ["sleep 20ms", "sleep 20ms", "sleep 20ms"],
        "{calls:?}"
    );
    assert_eq!(spawned_sleeps(&sys), 0);
    assert_eq!(kills(&sys), 1);
}

/// In-process sleeps ([`System::sleep`]).
fn sleeps(sys: &Fake) -> u32 {
    let count = sys
        .calls()
        .iter()
        .filter(|call| call.starts_with("sleep "))
        .count();
    u32::try_from(count).unwrap()
}

/// `sleep` children: none since the wait sleeps in this process.
fn spawned_sleeps(sys: &Fake) -> usize {
    sys.calls()
        .iter()
        .filter(|call| call.contains("/bin/sleep"))
        .count()
}

fn kills(sys: &Fake) -> usize {
    sys.calls()
        .iter()
        .filter(|call| call.contains("killall"))
        .count()
}
