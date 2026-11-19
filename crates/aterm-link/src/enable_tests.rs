// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_accepted_retirement_waits_for_its_actual_publication() {
    let (send, reply) = std::sync::mpsc::channel();
    send.send(fabric::RetireProgress {
        states: BTreeMap::from([("tf1xtvre000".to_owned(), "exited".to_owned())]),
        refusals_read: true,
        ..fabric::RetireProgress::default()
    })
    .unwrap();
    let outcome = await_retirement(
        &["tf1xtvre000"],
        Instant::now() - Duration::from_secs(6),
        |_| Ok(reply.try_recv().unwrap_or_default()),
        || {},
    );
    assert_eq!(outcome.done, 1);
    assert!(outcome.pending.is_empty());
    assert!(outcome.refused.is_empty());
    assert!(outcome.refusals_read);

    send.send(fabric::RetireProgress {
        states: BTreeMap::from([("tf1xtvre000".to_owned(), "exited".to_owned())]),
        ..fabric::RetireProgress::default()
    })
    .unwrap();
    let expired = await_retirement(
        &["tf1xtvre000"],
        Instant::now() - Duration::from_secs(61),
        |_| panic!("an expired wait does not observe another result"),
        || panic!("an expired wait never pauses"),
    );
    assert_eq!(expired.done, 0);
    assert_eq!(expired.pending, ["tf1xtvre000"]);
    assert_eq!(
        reply
            .try_recv()
            .unwrap()
            .states
            .get("tf1xtvre000")
            .map(String::as_str),
        Some("exited")
    );

    let reasons = BTreeMap::from([("tf1xtvre000".to_owned(), "fixture refusal".to_owned())]);
    let mut progress = Some(fabric::RetireProgress {
        refused: reasons.clone(),
        refusals_read: true,
        ..fabric::RetireProgress::default()
    });
    let refused = await_retirement(
        &["tf1xtvre000"],
        Instant::now(),
        |_| Ok(progress.take().expect("one actual refusal")),
        || {},
    );
    assert_eq!(refused.done, 0);
    assert!(refused.pending.is_empty());
    assert_eq!(refused.refused, reasons);
    assert!(refused.refusals_read);
}

#[test]
#[cfg(target_os = "macos")]
fn native_service_retirement_waits_for_its_original_child_to_finish() {
    let program = aterm_uds::deadline::stand_in(Path::new("/bin/cat"));
    let mut command = aterm_uds::spawnfd::command(program);
    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let input = std::cell::RefCell::new(Some(aterm_uds::deadline::cat_echoes(&mut child)));
    let child = std::cell::RefCell::new(child);
    assert!(!until_gone_at(
        &LiveLaunchd,
        || child.borrow_mut().try_wait().unwrap().is_some(),
        Instant::now() - Duration::from_secs(61),
    ));
    assert!(child.borrow_mut().try_wait().unwrap().is_none());
    let inspected = std::cell::Cell::new(false);
    let completed = until_gone_at(
        &LiveLaunchd,
        || {
            if inspected.replace(true) {
                drop(input.borrow_mut().take());
            }
            child.borrow_mut().try_wait().unwrap().is_some()
        },
        Instant::now() - Duration::from_secs(31),
    );
    drop(input.borrow_mut().take());
    let mut exit = aterm_uds::deadline::ExitWait::on_pid(child.borrow().id());
    let cleanup = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = child.borrow_mut().try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            Instant::now() < cleanup,
            "private cat did not exit after EOF"
        );
        exit.wait(cleanup.saturating_duration_since(Instant::now()));
    }
    assert!(completed);
}

#[cfg(unix)]
#[test]
fn required_local_child_waits_for_a_scheduled_completion() {
    let program = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
    let mut cmd = aterm_uds::spawnfd::command(&program);
    cmd.args(["-c", "printf ready"]);
    let out = required_output_after(&mut cmd, Duration::from_secs(31))
        .expect("required local work completes before the hang detector");
    assert!(out.status.success());
    assert_eq!(out.stdout, b"ready");

    let mut refused = aterm_uds::spawnfd::command(&program);
    refused.args(["-c", "printf refused; exit 7"]);
    let out = required_output_after(&mut refused, Duration::ZERO).unwrap();
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(out.stdout, b"refused");

    let scratch = crate::test_scratch_dir("en");
    let marker = scratch.path().join("never-started");
    let mut expired = aterm_uds::spawnfd::command(&program);
    expired
        .args(["-c", "printf unexpected > \"$1\"", "stand-in"])
        .arg(&marker);
    let result = required_output_after(&mut expired, Duration::from_secs(61));
    assert!(matches!(
        result,
        Err(aterm_uds::deadline::RunError::TimedOut)
    ));
    assert!(!marker.exists(), "expiry must not start the child");
}

#[test]
fn an_armed_supervisor_can_be_scheduled_after_the_old_observation_budget() {
    let mut reads = 0;
    let (state, _) = wait_for_arm_state(
        || {
            reads += 1;
            if reads == 1 {
                ("absent".to_owned(), "-".to_owned())
            } else {
                ("connected".to_owned(), "-".to_owned())
            }
        },
        Instant::now() - Duration::from_secs(6),
        |_| {},
    );
    assert_eq!(state, "connected");
    assert_eq!(reads, 2);

    let mut reads = 0;
    let (state, _) = wait_for_arm_state(
        || {
            reads += 1;
            ("absent".to_owned(), "-".to_owned())
        },
        Instant::now() - Duration::from_secs(61),
        |_| panic!("the hang detector is exhausted"),
    );
    assert_eq!(state, "absent");
    assert_eq!(reads, 1);

    let (state, reason) = wait_for_arm_state(
        || ("stalled".to_owned(), "state-locked".to_owned()),
        Instant::now(),
        |_| panic!("a state-locked bridge is already observed"),
    );
    assert_eq!(
        (state.as_str(), reason.as_str()),
        ("stalled", "state-locked")
    );
}

/// A [`Found`] of a root under a path nothing holds, its broker (pid 4242) at the root's own
/// socket, kept by `service`'s job (which runs it), an update having replaced its build
/// where `replaced` says.
fn found(service: Service, replaced: Option<&str>) -> Found {
    let p = Paths {
        home: PathBuf::from("/nonexistent/home"),
        login_home: None,
        root: PathBuf::from("/nonexistent/home/.local/share/aterm-fabric"),
        aterm: "/nonexistent/bin/aterm".to_string(),
        fleet: DEFAULT_FLEET.to_string(),
        config: None,
        rendezvous: None,
        durability: None,
    };
    Found {
        view: fabric::BrokerView {
            endpoint: p.sock(),
            pid: Some(4242),
            replaced: replaced.map(str::to_string),
            ..fabric::BrokerView::default()
        },
        p,
        node: Some("n-0123456789abcdef".to_string()),
        job_program: Lasting::Stays,
        command: Vec::new(),
        command_program: Lasting::Stays,
        job: (service != Service::None).then_some(4242),
        loaded: Ok(service != Service::None),
        disabled: false,
        answers: Ok(()),
        service,
    }
}

/// A root `p` under `dir`, nothing configured.
fn scratch_paths(dir: &Path) -> Paths {
    Paths {
        home: dir.to_path_buf(),
        login_home: None,
        root: dir.join("root"),
        aterm: "/nonexistent/bin/aterm".to_string(),
        fleet: DEFAULT_FLEET.to_string(),
        config: None,
        rendezvous: None,
        durability: None,
    }
}

/// `aterm fabric off`'s record is written once (a second off says it stands already), a
/// configured command reads on over it, and `aterm fabric on` removes it and says nothing
/// where there is none.
/// NEGATIVE CONTROL: an `unset_in` that reads the record before the command reads a
/// configured command as the person's off.
#[test]
fn an_off_record_is_written_once_and_an_on_removes_it() {
    let dir = crate::test_scratch_dir("en");
    let p = scratch_paths(dir.path());
    let mut out = Out::kept();
    off_record_step(&p, &mut out);
    assert!(!out.failed, "{}", out.said());
    assert!(p.root.join(OFF_RECORD).exists(), "off records it");
    assert_eq!(
        unset_in(&p.root, None, true),
        None,
        "a command still reads on"
    );
    let mut out = Out::kept();
    off_record_step(&p, &mut out);
    assert!(out.said().contains("already"), "{}", out.said());
    let mut out = Out::kept();
    assert!(
        record_step(&p, Service::Launchd, &mut out),
        "{}",
        out.said()
    );
    assert!(!p.root.join(OFF_RECORD).exists(), "on removes it");
    let mut out = Out::kept();
    assert!(record_step(&p, Service::Launchd, &mut out));
    assert!(out.said().is_empty(), "nothing said with no record");
}

/// AN OFF OUTLIVES ITS FABRIC ROOT: `aterm fabric off` keeps its record beside aterm.toml too,
/// so a person who deletes the fabric root after an off (it keeps the node id, the secret, the
/// grants and the bus log) still reads off, and the converge pass leaves it; `aterm fabric on`
/// removes both records. aterm.toml's folder keeps its mode (the record is empty).
/// NEGATIVE CONTROL: with `off_recorded` reading the root's record alone, the deleted root
/// reads as never turned off and the pass turns the fabric on over the person's off. A
/// symlinked root or config folder neither reads, writes, nor unlinks a record in its target.
#[test]
fn an_off_outlives_the_fabric_root_it_was_recorded_in() {
    let dir = crate::test_scratch_dir("en");
    let config_dir = dir.path().join("cfg/aterm");
    std::fs::create_dir_all(&config_dir).expect("aterm.toml's folder");
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        let mode = || {
            std::fs::metadata(&config_dir)
                .expect("mode")
                .permissions()
                .mode()
        };
        std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o755))
            .expect("0755");
        move || mode() & 0o777
    };
    let p = Paths {
        config: Some(config_dir.join("aterm.toml")),
        ..scratch_paths(dir.path())
    };
    let kept = kept_off_record(&p).expect("beside aterm.toml");
    let mut out = Out::kept();
    off_record_step(&p, &mut out);
    assert!(!out.failed, "{}", out.said());
    assert!(p.root.join(OFF_RECORD).exists(), "in the root");
    assert!(kept.exists(), "beside aterm.toml");
    #[cfg(unix)]
    assert_eq!(mode(), 0o755, "aterm.toml's folder keeps its mode");
    std::fs::remove_dir_all(&p.root).expect("the person tidies the root away");
    let off = unset_in(&p.root, Some(&kept), false);
    assert!(
        matches!(&off, Some(Unset::PersonOff(why)) if why.contains(&*kept.to_string_lossy())),
        "{off:?}"
    );
    let left = by_default_in(&p.root, Some(&kept), &|| Ok(false), |_| {
        panic!("no steps over their off")
    });
    assert!(matches!(left, Ok(ByDefault::Left(_))), "{left:?}");
    let mut out = Out::kept();
    off_record_step(&p, &mut out);
    assert!(!out.failed, "{}", out.said());
    let mut out = Out::kept();
    assert!(
        record_step(&p, Service::Launchd, &mut out),
        "{}",
        out.said()
    );
    assert!(!p.root.join(OFF_RECORD).exists(), "on removes the root's");
    assert!(!kept.exists(), "on removes the one beside aterm.toml");
    assert!(
        matches!(unset_in(&p.root, Some(&kept), false), Some(Unset::Never(_))),
        "on again: the pass may turn it on"
    );

    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::remove_dir_all(&p.root).expect("remove root");
    std::fs::remove_dir_all(&config_dir).expect("remove config dir");
    std::os::unix::fs::symlink(&outside, &p.root).expect("symlink root");
    std::os::unix::fs::symlink(&outside, &config_dir).expect("symlink config dir");
    let mut out = Out::kept();
    off_record_step(&p, &mut out);
    assert!(out.failed, "symlinked root must fail: {}", out.said());
    assert!(
        !outside.join(OFF_RECORD).exists(),
        "symlinked folders must not write into outside"
    );
    std::fs::write(outside.join(OFF_RECORD), b"keep").expect("outside off");
    std::fs::write(outside.join(MINT_SECRET), b"keep").expect("outside secret");
    assert!(
        matches!(unset_in(&p.root, Some(&kept), false), Some(Unset::Never(_))),
        "symlinked folders must not read outside state as ours"
    );
    let mut out = Out::kept();
    assert!(
        !record_step(&p, Service::Launchd, &mut out),
        "symlinked folders must fail record_step"
    );
    assert!(out.failed);
    assert!(
        outside.join(OFF_RECORD).exists(),
        "symlinked folders must not unlink outside record"
    );
}

/// A root an earlier `on` left without a node id (one that predates it, or set up by hand)
/// still reads as the person's off where it keeps the mint secret or the node grants.
/// NEGATIVE CONTROL: a read that looks for the node id alone turns such a root on over an off.
#[test]
fn a_root_with_no_node_id_that_an_earlier_on_left_reads_as_the_persons_off() {
    for kept in [MINT_SECRET, "node.cap"] {
        let dir = crate::test_scratch_dir("en");
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("link-state")).expect("state");
        std::fs::write(root.join("link-state/lock"), b"").expect("lock");
        assert!(
            matches!(unset_in(&root, None, false), Some(Unset::Never(_))),
            "{kept}: state alone is no off"
        );
        std::fs::write(root.join(kept), b"inert").expect("kept");
        let read = unset_in(&root, None, false);
        assert!(
            matches!(&read, Some(Unset::PersonOff(why)) if why.contains(kept)),
            "{kept}: {read:?}"
        );
        std::fs::write(root.join(DEFAULT_RECORD), b"").expect("by default");
        assert!(
            matches!(unset_in(&root, None, false), Some(Unset::Never(_))),
            "{kept}: a root the pass began turning on"
        );
    }
}

/// The node id an `on`'s steps write, in `root`.
fn node_written(root: &Path) {
    std::fs::create_dir_all(root.join("link-state")).expect("state");
    std::fs::write(root.join("link-state/node"), "n-0123456789abcdef\n").expect("node");
}

/// The converge pass's turn-on, its steps a stand-in for `on`'s: the record stands while they
/// run and goes once they finished, so a command the person clears after reads as their off;
/// steps that fail leave it, so the root reads as never turned off and the next pass finishes
/// it.
/// NEGATIVE CONTROL: a turn-on that leaves no record before its steps reads its own failed
/// steps as the person's off for good, and one that never takes it out turns the fabric on
/// again over a command the person cleared.
#[test]
fn a_turn_on_by_default_records_itself_until_its_steps_finish() {
    let no_command = || Ok(false);
    let dir = crate::test_scratch_dir("en");
    let root = dir.path().join("root");
    let done = by_default_in(&root, None, &no_command, |out| {
        assert!(root.join(DEFAULT_RECORD).exists(), "left before the steps");
        node_written(&root);
        out.ok("node", "n-0123456789abcdef");
        Ok(())
    })
    .expect("turned on");
    assert!(
        matches!(&done, ByDefault::TurnedOn(said) if said.contains("node")),
        "{done:?}"
    );
    assert!(
        !root.join(DEFAULT_RECORD).exists(),
        "taken out once it finished"
    );
    let read = unset_in(&root, None, false);
    assert!(
        matches!(&read, Some(Unset::PersonOff(_))),
        "a command the person cleared after: {read:?}"
    );
    let left = by_default_in(&root, None, &no_command, |_| {
        panic!("no steps over their off")
    });
    assert!(matches!(left, Ok(ByDefault::Left(_))), "{left:?}");

    let dir = crate::test_scratch_dir("en");
    let root = dir.path().join("root");
    let failed = by_default_in(&root, None, &no_command, |out| {
        node_written(&root);
        out.fail("broker", "did not start");
        Err(ExitCode::FAILURE)
    });
    assert!(
        matches!(&failed, Err(said) if said.contains("did not start")),
        "{failed:?}"
    );
    assert!(root.join(DEFAULT_RECORD).exists(), "kept for the next pass");
    assert!(
        matches!(unset_in(&root, None, false), Some(Unset::Never(_))),
        "the next pass finishes it"
    );
}

/// The turn-on reads the state again under the off's lock: an off recorded since the pass read
/// it, an off running now (holding the lock) and a command set since each leave the steps
/// unrun.
/// NEGATIVE CONTROL: a turn-on that trusts the pass's first read runs its steps (and removes
/// the off's record) over an off made since; one that skips the lock runs them midway through
/// an off.
#[test]
fn a_turn_on_by_default_stands_aside_for_an_off_made_since_or_running() {
    let no_command = || Ok(false);
    let unrun = |_: &mut Out| -> Result<(), ExitCode> { panic!("no steps") };

    let dir = crate::test_scratch_dir("en");
    let root = dir.path().join("root");
    let p = scratch_paths(dir.path());
    let mut out = Out::kept();
    off_record_step(&p, &mut out);
    let left = by_default_in(&root, None, &no_command, unrun);
    assert!(
        matches!(&left, Ok(ByDefault::Left(why)) if why.contains("aterm fabric off")),
        "{left:?}"
    );
    assert!(root.join(OFF_RECORD).exists(), "the off's record stands");

    let dir = crate::test_scratch_dir("en");
    let root = dir.path().join("root");
    let off = switch_lock(&root, Duration::ZERO).expect("the off's lock");
    let left = by_default_in(&root, None, &no_command, unrun);
    assert!(
        matches!(&left, Ok(ByDefault::Left(why)) if why.contains("runs now")),
        "{left:?}"
    );
    assert!(!root.join(DEFAULT_RECORD).exists(), "nothing written");
    drop(off);

    let on = by_default_in(&root, None, &|| Ok(true), unrun);
    assert!(matches!(on, Ok(ByDefault::On)), "{on:?}");
    assert!(!root.join(DEFAULT_RECORD).exists(), "nothing written");
}

/// A dry run says the record's step and writes nothing.
#[test]
fn a_dry_run_off_or_on_writes_and_removes_no_record() {
    let dir = crate::test_scratch_dir("en");
    let p = scratch_paths(dir.path());
    let mut out = Out::new(true);
    out.kept = Some(std::cell::RefCell::new(Vec::new()));
    off_record_step(&p, &mut out);
    assert!(out.said().contains("would"), "{}", out.said());
    assert!(!p.root.exists(), "a dry off makes nothing");
    std::fs::create_dir_all(&p.root).expect("root");
    std::fs::write(p.root.join(OFF_RECORD), b"").expect("record");
    let mut out = Out::new(true);
    out.kept = Some(std::cell::RefCell::new(Vec::new()));
    assert!(record_step(&p, Service::Launchd, &mut out));
    assert!(p.root.join(OFF_RECORD).exists(), "a dry on keeps it");
}

/// An absent broker on `service`'s root (nothing answers on its socket and its job unloaded).
fn absent(service: Service) -> Found {
    let mut at = found(service, None);
    at.view.pid = None;
    at.job = None;
    at.loaded = Ok(false);
    at.answers = Err("connect: Connection refused (os error 61)".to_string());
    at
}

/// `at` with its fabric root at `root`, and its broker's endpoint with it where it named the
/// root's socket: an apply takes the switch lock in the root.
fn rooted(mut at: Found, root: &Path) -> Found {
    let ours = at.view.endpoint == at.p.sock();
    at.p.root = root.to_path_buf();
    if ours {
        at.view.endpoint = at.p.sock();
    }
    at
}

/// The converge pass's broker renewal: with no `[fabric] command` nothing is looked at and
/// nothing runs; a broker on the build on disk reads current; one on a replaced build is
/// renewed once by the broker step (a look leaves it due); one the person keeps
/// (`--service none`, or one started by hand that is not the process `on`'s job runs, with
/// that job not loaded or running another), or one at another socket, is left as it is.
/// NEGATIVE CONTROLS: a renewal that reads no command as due runs the step with the fabric
/// off; one that takes any broker of a root with `on`'s job for that job's steps a broker the
/// person started by hand. Absent sockets, `--service none` with no broker and an unavailable
/// supervisor are checked by
/// `a_broker_that_does_not_answer_is_due_where_the_job_on_installed_serves_none`.
#[test]
fn a_broker_is_renewed_only_where_the_fabric_is_on_and_it_runs_a_replaced_build() {
    use std::cell::Cell;
    let command = || Some(words("aterm link serve --broker /nonexistent/bus.sock"));
    let (looks, steps) = (Cell::new(0), Cell::new(0));
    let dir = crate::test_scratch_dir("en");
    aterm_uds::private_file::ensure_private_dir(&dir.path().join("root")).expect("the root");
    let renew = |command: Option<Vec<String>>, renew: Renew, at: Found| {
        renew_with(
            command,
            renew,
            |_: &[String]| {
                looks.set(looks.get() + 1);
                Ok(rooted(at, &dir.path().join("root")))
            },
            |_: &Found, _: Steps| {
                steps.set(steps.get() + 1);
                Ok("broker done (re)started launchd".to_string())
            },
        )
    };
    let off = renew(
        None,
        Renew::Apply,
        found(Service::Launchd, Some("/old/aterm")),
    );
    assert_eq!(off, Ok(BrokerRenewal::Off));
    assert_eq!(
        (looks.get(), steps.get()),
        (0, 0),
        "command unset: nothing read or run"
    );

    let current = renew(command(), Renew::Apply, found(Service::Launchd, None));
    assert!(
        matches!(current, Ok(BrokerRenewal::Current(_))),
        "{current:?}"
    );
    assert_eq!((looks.get(), steps.get()), (1, 0), "current: no step");

    let due = renew(
        command(),
        Renew::Look,
        found(Service::Launchd, Some("/old/aterm")),
    );
    assert!(matches!(due, Ok(BrokerRenewal::Due { .. })), "{due:?}");
    assert_eq!(steps.get(), 0, "a look runs no step");

    let renewed = renew(
        command(),
        Renew::Apply,
        found(Service::Launchd, Some("/old/aterm")),
    );
    assert_eq!(
        renewed,
        Ok(BrokerRenewal::Renewed(
            "broker done (re)started launchd".to_string()
        ))
    );
    assert_eq!(steps.get(), 1, "replaced: one renewal");

    let theirs = renew(
        command(),
        Renew::Apply,
        found(Service::None, Some("/old/aterm")),
    );
    assert!(matches!(theirs, Ok(BrokerRenewal::Kept(_))), "{theirs:?}");
    let mut elsewhere = found(Service::Launchd, Some("/old/aterm"));
    elsewhere.view.endpoint = "/elsewhere/bus.sock".to_string();
    let elsewhere = renew(command(), Renew::Apply, elsewhere);
    assert!(
        matches!(elsewhere, Ok(BrokerRenewal::Kept(_))),
        "{elsewhere:?}"
    );
    for job in [None, Some(4343)] {
        let mut by_hand = found(Service::Launchd, Some("/old/aterm"));
        by_hand.job = job;
        let by_hand = renew(command(), Renew::Apply, by_hand);
        assert!(
            matches!(&by_hand, Ok(BrokerRenewal::Kept(why)) if why.contains("not the job")),
            "{job:?}: {by_hand:?}"
        );
    }
    assert_eq!(steps.get(), 1, "a broker the person keeps is left as it is");
}

/// The update measured on m22 left `on`'s launchd job unloaded, and the next pass read the
/// missing broker as current. Where no broker answers on the root's socket, the job `on`
/// installed is due (a look) and stepped once (an apply) where launchd has not loaded it,
/// holds it running nothing, or runs a broker that does not answer, and where systemd runs
/// none. A broker that answers reads current whether or not the listener scan named it. It is
/// left as it is under `--service none`, at another socket, for a process the person started
/// that holds the socket, where which process holds it is unknown (the scan could not tell),
/// where launchd or systemd has the job disabled (or masked) or cannot be asked about it, and
/// where `aterm fabric off`'s record stands in the root, which keeps a replaced broker too.
/// NEGATIVE CONTROLS: a renewal that reads a broker nobody answers for as current (as
/// measured), one blind to the off record, to a disabled job, to a login launchd cannot be
/// asked in, to an answering broker the scan missed, or to an unknown holder each fail it.
#[test]
fn a_broker_that_does_not_answer_is_due_where_the_job_on_installed_serves_none() {
    use std::cell::Cell;
    let steps = Cell::new(0);
    let dir = crate::test_scratch_dir("en");
    let root = dir.path().join("root");
    aterm_uds::private_file::ensure_private_dir(&root).expect("the root");
    let renew = |renew: Renew, at: Found| {
        renew_with(
            Some(words("aterm link serve --broker /nonexistent/bus.sock")),
            renew,
            |_: &[String]| Ok(rooted(at, &root)),
            |_: &Found, _: Steps| {
                steps.set(steps.get() + 1);
                Ok("broker done loaded launchd".to_string())
            },
        )
    };
    let silent = |service: Service, loaded: bool, job: Option<u32>, pid: Option<u32>| {
        let mut at = found(service, None);
        at.view.pid = pid;
        at.answers = Err("connect: Connection refused (os error 61)".to_string());
        at.loaded = Ok(loaded);
        at.job = job;
        at
    };
    let sock = rooted(found(Service::Launchd, None), &root).p.sock();
    for (service, loaded, job, pid, says) in [
        (
            Service::Launchd,
            false,
            None,
            None,
            "launchd has not loaded",
        ),
        (Service::Launchd, true, None, None, "runs no process of it"),
        (Service::Launchd, true, Some(4343), None, "though launchd"),
        (
            Service::Launchd,
            true,
            Some(4242),
            Some(4242),
            "though launchd",
        ),
        (Service::Systemd, false, None, None, "systemd --user"),
    ] {
        let case = format!("{service:?} loaded={loaded} job={job:?} pid={pid:?}");
        let look = renew(Renew::Look, silent(service, loaded, job, pid));
        assert!(
            matches!(&look, Ok(BrokerRenewal::Due { from: why, .. })
                if why.starts_with(&format!("no broker answers on {sock} (connect: "))
                    && why.contains(says)),
            "{case}: {look:?}"
        );
        let before = steps.get();
        let applied = renew(Renew::Apply, silent(service, loaded, job, pid));
        assert!(
            matches!(&applied, Ok(BrokerRenewal::Renewed(_))),
            "{case}: {applied:?}"
        );
        assert_eq!(steps.get(), before + 1, "{case}: one step");
    }
    let stepped = steps.get();

    let mut unnamed = found(Service::Launchd, None);
    unnamed.view.pid = None;
    let unnamed = renew(Renew::Apply, unnamed);
    assert!(
        matches!(&unnamed, Ok(BrokerRenewal::Current(_))),
        "a broker that answers, though the scan named no process: {unnamed:?}"
    );
    let theirs = renew(Renew::Apply, silent(Service::None, false, None, None));
    assert!(
        matches!(&theirs, Ok(BrokerRenewal::Kept(why)) if why.contains("start it yourself")),
        "{theirs:?}"
    );
    let by_hand = renew(
        Renew::Apply,
        silent(Service::Launchd, false, None, Some(999)),
    );
    assert!(
        matches!(&by_hand, Ok(BrokerRenewal::Kept(why)) if why.contains("not the job")),
        "{by_hand:?}"
    );
    for (loaded, job) in [(false, None), (true, Some(4242))] {
        let mut unknown = silent(Service::Launchd, loaded, job, None);
        unknown.view.pid_unknown =
            Some("the listener scan cannot tell (connection refused)".to_string());
        let unknown = renew(Renew::Apply, unknown);
        assert!(
            matches!(&unknown, Ok(BrokerRenewal::Kept(why))
                if why.contains(&format!("which process holds {sock} is unknown"))
                    && why.contains("connection refused")),
            "job {job:?}: {unknown:?}"
        );
    }
    let mut disabled = silent(Service::Launchd, false, None, None);
    disabled.disabled = true;
    let disabled = renew(Renew::Apply, disabled);
    assert!(
        matches!(&disabled, Ok(BrokerRenewal::Kept(why)) if why.contains("disabled")),
        "{disabled:?}"
    );
    let mut masked = silent(Service::Systemd, false, None, None);
    (masked.job, masked.loaded, masked.disabled) =
        systemd_state(Ok(systemd_shown("MainPID=0\nUnitFileState=masked\n")));
    let masked = renew(Renew::Apply, masked);
    assert!(
        matches!(&masked, Ok(BrokerRenewal::Kept(why)) if why.contains("systemctl --user enable")),
        "{masked:?}"
    );
    let mut no_bus = silent(Service::Systemd, false, None, None);
    (no_bus.job, no_bus.loaded, no_bus.disabled) =
        systemd_state(Err("no systemd user manager answers this login".to_string()));
    let no_bus = renew(Renew::Apply, no_bus);
    assert!(
        matches!(&no_bus, Ok(BrokerRenewal::Kept(why)) if why.contains("no systemd user manager")),
        "{no_bus:?}"
    );
    let mut unasked = silent(Service::Launchd, false, None, None);
    unasked.loaded = Err("launchd has no gui/501 domain in this login".to_string());
    let unasked = renew(Renew::Apply, unasked);
    assert!(
        matches!(&unasked, Ok(BrokerRenewal::Kept(why)) if why.contains("no gui/501 domain")),
        "{unasked:?}"
    );
    let mut elsewhere = silent(Service::Launchd, false, None, None);
    elsewhere.view.endpoint = "/elsewhere/bus.sock".to_string();
    let elsewhere = renew(Renew::Apply, elsewhere);
    assert!(
        matches!(&elsewhere, Ok(BrokerRenewal::Kept(why)) if why.contains("not its root's")),
        "{elsewhere:?}"
    );
    std::fs::write(root.join(OFF_RECORD), b"").expect("the person's off");
    for off in [
        silent(Service::Launchd, false, None, None),
        found(Service::Launchd, Some("/old/aterm")),
    ] {
        let off = renew(Renew::Apply, off);
        assert!(
            matches!(&off, Ok(BrokerRenewal::Kept(why)) if why.starts_with("off by your choice")),
            "{off:?}"
        );
    }
    assert_eq!(
        steps.get(),
        stepped,
        "what the person keeps is never stepped"
    );
}

/// A renewal's apply steps only under the switch `aterm fabric off` and `on` hold: one that
/// holds it now leaves the broker as it is (a look still reads it due), and one that starts
/// while the step runs finds it held. A lock that cannot be made (the root is a file, or gone,
/// which the renewal does not make) is said beside the step's lines.
/// NEGATIVE CONTROLS: a renewal that takes no lock steps midway through an off (which stops
/// the broker the step starts); one that lets it go before the step lets an off land midway
/// through it; one that reads any lock failure as held never renews a broker whose root
/// cannot hold the lock.
#[test]
fn a_broker_renewal_steps_only_under_the_switch_off_and_on_hold() {
    let dir = crate::test_scratch_dir("en");
    let root = dir.path().join("root");
    let command = || Some(words("aterm link serve"));
    let replaced = |root: &Path| rooted(found(Service::Launchd, Some("/old/aterm")), root);

    let off = switch_lock(&root, Duration::ZERO).expect("an off's lock");
    let kept = renew_with(
        command(),
        Renew::Apply,
        |_: &[String]| Ok(replaced(&root)),
        |_: &Found, _: Steps| panic!("no step while an off holds the switch"),
    );
    assert!(
        matches!(&kept, Ok(BrokerRenewal::Kept(why)) if why.contains("runs now")),
        "{kept:?}"
    );
    let due = renew_with(
        command(),
        Renew::Look,
        |_: &[String]| Ok(replaced(&root)),
        |_: &Found, _: Steps| panic!("a look runs no step"),
    );
    assert!(matches!(due, Ok(BrokerRenewal::Due { .. })), "{due:?}");
    drop(off);

    let renewed = renew_with(
        command(),
        Renew::Apply,
        |_: &[String]| Ok(replaced(&root)),
        |at: &Found, _: Steps| {
            let off = switch_lock(&at.p.root, Duration::ZERO);
            assert!(
                matches!(&off, Err(e) if e.kind() == io::ErrorKind::WouldBlock),
                "an off that starts during the step waits for it: {:?}",
                off.as_ref().err()
            );
            Ok("broker done (re)started launchd".to_string())
        },
    );
    assert_eq!(
        renewed,
        Ok(BrokerRenewal::Renewed(
            "broker done (re)started launchd".to_string()
        ))
    );
    assert!(
        switch_lock(&root, Duration::ZERO).is_ok(),
        "let go once the step ends"
    );

    let file = dir.path().join("a-file");
    std::fs::write(&file, b"").expect("a file where the root goes");
    let unlocked = renew_with(
        command(),
        Renew::Apply,
        |_: &[String]| Ok(replaced(&file)),
        |_: &Found, _: Steps| Ok("broker done (re)started launchd".to_string()),
    );
    assert!(
        matches!(&unlocked, Ok(BrokerRenewal::Renewed(said))
            if said.starts_with("broker done (re)started launchd (without ")),
        "{unlocked:?}"
    );
    let gone = dir.path().join("gone");
    let unlocked = renew_with(
        command(),
        Renew::Apply,
        |_: &[String]| Ok(replaced(&gone)),
        |_: &Found, _: Steps| Ok("broker done (re)started launchd".to_string()),
    );
    assert!(
        matches!(&unlocked, Ok(BrokerRenewal::Renewed(said)) if said.contains("(without ")),
        "{unlocked:?}"
    );
    assert!(!gone.exists(), "a root that is gone is not made");
}

/// A person's `aterm fabric on` runs its steps under the switch: one the converge pass's
/// turn-on or renewal, or another `on` or `off`, holds past the wait fails the command with
/// what to do and runs no step; one free is held while the steps run. A dry run takes none.
/// NEGATIVE CONTROLS: an `on` that takes no lock writes the root midway through an off or a
/// turn-on (its steps run while the switch is held), and one that lets it go before its steps
/// lets an off land midway through them.
#[test]
fn a_persons_fabric_on_runs_its_steps_under_the_switch() {
    let dir = crate::test_scratch_dir("en");
    let prepared = Prepared {
        p: scratch_paths(dir.path()),
        job_durability: None,
        durability_job: None,
        mesh: None,
    };
    let root = prepared.p.root.clone();

    let held = switch_lock(&root, Duration::ZERO).expect("a turn-on's lock");
    let mut out = Out::kept();
    let code = on_prepared(&prepared, &mut out, Duration::ZERO, |_| {
        panic!("no step while the switch is held")
    });
    assert_eq!(code, ExitCode::FAILURE);
    assert!(
        out.said().contains("run `aterm fabric on` again"),
        "{}",
        out.said()
    );
    drop(held);

    let mut out = Out::kept();
    let code = on_prepared(&prepared, &mut out, Duration::ZERO, |_| {
        let off = switch_lock(&root, Duration::ZERO);
        assert!(
            matches!(&off, Err(e) if e.kind() == io::ErrorKind::WouldBlock),
            "an off that starts during the steps waits for them: {:?}",
            off.as_ref().err()
        );
        Err(ExitCode::from(7))
    });
    assert_eq!(code, ExitCode::from(7), "the steps' own exit");
    assert!(
        switch_lock(&root, Duration::ZERO).is_ok(),
        "let go once the steps end"
    );

    let dry = crate::test_scratch_dir("en");
    let mut out = Out::new(true);
    let lock = persons_switch(&dry.path().join("root"), "on", Duration::ZERO, &mut out);
    assert!(matches!(lock, Ok(None)), "a dry run takes no lock");
    assert!(!dry.path().join("root").exists(), "a dry run makes no root");
}

/// `aterm fabric on --service none` records that the person runs the broker, and the renewal
/// keeps a root with that record as it is, even where a plist an earlier `on` installed is
/// still there; an `on` that hands the broker to launchd takes the record out, and a dry run
/// writes none.
/// NEGATIVE CONTROLS: an `on --service none` that records nothing, or a renewal blind to the
/// record, loads the leftover plist over the broker the person keeps; an `on` that leaves the
/// record keeps a job it installed from ever being renewed.
#[test]
fn a_broker_the_person_keeps_with_service_none_is_never_renewed() {
    let dir = crate::test_scratch_dir("en");
    let p = scratch_paths(dir.path());
    aterm_uds::private_file::ensure_private_dir(&p.root).expect("root, 0700 as `on` makes it");
    let record = p.root.join(HAND_RECORD);

    let mut out = Out::new(true);
    out.kept = Some(std::cell::RefCell::new(Vec::new()));
    assert!(hand_record_step(&p, Service::None, &mut out));
    assert!(out.said().contains("would"), "{}", out.said());
    assert!(!record.exists(), "a dry run writes no record");

    let mut out = Out::kept();
    assert!(
        hand_record_step(&p, Service::None, &mut out),
        "{}",
        out.said()
    );
    assert!(record.exists(), "`on --service none` records it");

    let renew = |renew: Renew| {
        renew_with(
            Some(words("aterm link serve")),
            renew,
            |_: &[String]| Ok(rooted(absent(Service::Launchd), &p.root)),
            |_: &Found, _: Steps| panic!("no step for a broker the person keeps"),
        )
    };
    for how in [Renew::Look, Renew::Apply] {
        let kept = renew(how);
        assert!(
            matches!(&kept, Ok(BrokerRenewal::Kept(why)) if why.contains("--service none")),
            "{how:?}: {kept:?}"
        );
    }

    let mut out = Out::kept();
    assert!(
        record_step(&p, Service::Launchd, &mut out),
        "{}",
        out.said()
    );
    assert!(!record.exists(), "an `on` under launchd takes it out");
    let due = renew_with(
        Some(words("aterm link serve")),
        Renew::Look,
        |_: &[String]| Ok(rooted(absent(Service::Launchd), &p.root)),
        |_: &Found, _: Steps| panic!("a look runs no step"),
    );
    assert!(matches!(due, Ok(BrokerRenewal::Due { .. })), "{due:?}");
}

/// A program aterm wrote moves to the installed app only where it names no regular file: gone,
/// a dangling link, a path under a file, a directory. One that names a regular file stays,
/// whatever file it is: the installed app, a link to it, another build, a link to that. With no
/// installed app known (or one that names no regular file) a program that names none is
/// nowhere. A relative path, and one that cannot be read (a link loop), stay.
/// NEGATIVE CONTROLS: a judge that moves a present file other than the app (the rule before
/// this) moves `other` onto it, which downgrades a newer broker, flips the job between two
/// copies on alternating passes and undoes a fabric pointed at a dev build; the read before
/// that kept whatever program the job named, a gone one included, and so leaves the job on a
/// file that no longer starts.
#[test]
fn a_program_aterm_wrote_moves_only_where_it_names_no_file() {
    let scratch = crate::test_scratch_dir("en");
    let dir = scratch.path();
    let app = dir.join("installed-aterm");
    std::fs::write(&app, b"the installed app").expect("an app");
    let other = dir.join("other-aterm");
    std::fs::write(&other, b"another build").expect("another build");
    let via = dir.join("linked-aterm");
    std::os::unix::fs::symlink(&app, &via).expect("a link to the app");
    let via_other = dir.join("linked-other-aterm");
    std::os::unix::fs::symlink(&other, &via_other).expect("a link to another build");
    let dangling = dir.join("dangling-aterm");
    std::os::unix::fs::symlink(dir.join("removed"), &dangling).expect("a dangling link");
    let looped = dir.join("loop-a");
    std::os::unix::fs::symlink(dir.join("loop-b"), &looped).expect("a loop");
    std::os::unix::fs::symlink(&looped, dir.join("loop-b")).expect("a loop");
    let gone = dir.join("removed/aterm");
    let under_a_file = other.join("aterm");
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let installed = Some(app.as_path());
    let moved = |from: &Path| {
        Lasting::Moves(Moved {
            from: s(from),
            to: s(&app),
        })
    };
    for present in [&app, &via, &other, &via_other] {
        assert_eq!(
            lasting(&s(present), installed),
            Lasting::Stays,
            "{} names a file",
            present.display()
        );
    }
    for named in [&gone, &dangling, &under_a_file, &dir.to_path_buf()] {
        assert_eq!(
            lasting(&s(named), installed),
            moved(named),
            "{}",
            named.display()
        );
    }
    assert_eq!(lasting(&s(&looped), installed), Lasting::Stays, "unread");
    assert_eq!(lasting("aterm", installed), Lasting::Stays, "relative");
    assert_eq!(lasting(&s(&other), None), Lasting::Stays, "no app known");
    assert_eq!(lasting(&s(&gone), None), Lasting::Nowhere(s(&gone)));
    for no_app in [dir.join("no-app"), dir.to_path_buf()] {
        assert_eq!(
            lasting(&s(&gone), Some(&no_app)),
            Lasting::Nowhere(s(&gone)),
            "an installed app that names no regular file: {}",
            no_app.display()
        );
    }
}

/// The job's program judged as `look_at` judges it: one that is gone, with the installed app
/// known, puts that app in `p`, so the broker step's plist and arguments name it and the step
/// reloads the job from them (its arguments changed: never a kickstart in place, which would
/// start the gone program again); an app path that is not a plain word runs through `on`'s
/// program link in the root, which the program step points at the app. With no installed app
/// known the gone program stays in `p` (the renewal then leaves the job serving), and a program
/// that names another build than the app stays there too.
/// NEGATIVE CONTROL: the read before this kept the job's own program in `p`, so the plist the
/// step writes names the gone file and reads current against the one on disk: no rewrite.
#[test]
fn a_job_whose_program_moves_is_rewritten_onto_the_installed_app() {
    let scratch = crate::test_scratch_dir("en");
    let dir = scratch.path();
    let spaced = dir.join("a b");
    std::fs::create_dir_all(&spaced).expect("scratch");
    let gone = dir.join("removed/aterm").to_string_lossy().into_owned();
    for app in [dir.join("aterm"), spaced.join("aterm")] {
        std::fs::write(&app, b"#!/bin/sh\n").expect("an app");
        let shown = app.to_string_lossy().into_owned();
        let was = Paths {
            aterm: gone.clone(),
            ..scratch_paths(dir)
        };
        let old = was.plist_text();
        let mut p = scratch_paths(dir);
        assert_eq!(
            job_lasting(&mut p, gone.clone(), Some(&app)),
            Lasting::Moves(Moved {
                from: gone.clone(),
                to: shown.clone(),
            })
        );
        assert_eq!(p.aterm, shown);
        assert!(
            launchd_rewrite(&was, Some(old.clone()), true, "").is_none(),
            "the gone program's own plist reads current"
        );
        let text = launchd_rewrite(&p, Some(old), true, "")
            .expect("rewritten, then reloaded")
            .text;
        let program = plist_arguments(&text).first().cloned();
        if argv_safe(&shown).is_ok() {
            assert_eq!(program.as_deref(), Some(shown.as_str()));
        } else {
            let link = p.program_link();
            assert_eq!(program, Some(link.to_string_lossy().into_owned()));
            aterm_uds::private_file::ensure_private_dir(&p.root).expect("the root");
            let mut out = Out::kept();
            assert!(program_step(&p, &mut out), "{}", out.said());
            assert_eq!(std::fs::read_link(&link).expect("the program link"), app);
        }
    }
    let mut p = scratch_paths(dir);
    assert_eq!(
        job_lasting(&mut p, gone.clone(), None),
        Lasting::Nowhere(gone.clone())
    );
    assert_eq!(p.aterm, gone, "nothing to point it at");
    let other = dir.join("other-aterm");
    std::fs::write(&other, b"#!/bin/sh\n").expect("another build");
    let other = other.to_string_lossy().into_owned();
    let mut p = scratch_paths(dir);
    assert_eq!(
        job_lasting(&mut p, other.clone(), Some(&dir.join("aterm"))),
        Lasting::Stays
    );
    assert_eq!(
        p.aterm, other,
        "a job on another build that starts keeps it"
    );
}

/// `[fabric] command`'s program is judged only where aterm wrote the command: read from
/// aterm.toml, and word for word what `on` writes for the root, fleet and node with that
/// program (the `aterm-link` shim's form included); there a gone one moves to the installed
/// app, and the command `on` writes with the app reads as staying, so the next pass leaves it.
/// A person's command (one more word, another fleet, another node, another root's socket),
/// one from `$ATERM_FABRIC_COMMAND`, and any of a root that holds no node id stay, gone program
/// and all.
/// NEGATIVE CONTROL: a judge that reads the program alone, with no `written_by_on`, rewrites a
/// person's command, and drops the words they chose for `on`'s.
#[test]
fn a_persons_fabric_command_is_never_rewritten() {
    let scratch = crate::test_scratch_dir("en");
    let dir = scratch.path();
    let app = dir.join("aterm");
    std::fs::write(&app, b"#!/bin/sh\n").expect("an app");
    let shown = app.to_string_lossy().into_owned();
    let p = scratch_paths(dir);
    let node = "n-0123456789abcdef";
    let gone = dir.join("removed/aterm").to_string_lossy().into_owned();
    let written = |aterm: &str| {
        Paths {
            aterm: aterm.to_string(),
            ..p.clone()
        }
        .bridge_command(node)
    };
    let ours = written(&gone);
    let judged = |command: &[String], in_file: bool, node: Option<&str>| {
        command_lasting(&p, node, command, in_file, Some(&app))
    };
    assert_eq!(
        judged(&ours, true, Some(node)),
        Lasting::Moves(Moved {
            from: gone.clone(),
            to: shown.clone(),
        })
    );
    let shim = written(&dir.join("removed/aterm-link").to_string_lossy());
    assert!(!shim.contains(&"link".to_string()), "{shim:?}");
    assert!(matches!(judged(&shim, true, Some(node)), Lasting::Moves(_)));
    assert_eq!(
        judged(&written(&shown), true, Some(node)),
        Lasting::Stays,
        "the rewritten command"
    );

    let mut more = ours.clone();
    more.push("--verbose".to_string());
    let mut fleet = ours.clone();
    let at = fleet.iter().position(|w| w == "--fleet").expect("--fleet") + 1;
    fleet[at] = "lab".to_string();
    let mut other_node = ours.clone();
    *other_node.last_mut().expect("--accept-from's node") = "n-fedcba9876543210".to_string();
    let elsewhere = Paths {
        root: dir.join("elsewhere"),
        aterm: gone.clone(),
        ..p.clone()
    }
    .bridge_command(node);
    for persons in [more, fleet, other_node, elsewhere] {
        assert_ne!(persons, ours);
        assert_eq!(
            judged(&persons, true, Some(node)),
            Lasting::Stays,
            "{persons:?}"
        );
    }
    assert_eq!(judged(&ours, false, Some(node)), Lasting::Stays, "the env");
    assert_eq!(judged(&ours, true, None), Lasting::Stays, "no node id");
}

/// A program aterm wrote that names no file is due though the broker answers on its build: a
/// job's program that is gone (the broker still maps the file it started from, so it also reads
/// replaced), and a `[fabric] command` aterm wrote; a look names each, the old path then the
/// installed app's, and what the apply leaves, and an apply steps once. A job's program that is
/// gone with no installed app known is left as it is, a look and an apply alike, with no step,
/// its reason saying what was read: a broker that still serves is left serving, and one that
/// does not answer is said not to; a command that is gone with none known reads left too, never
/// current. A broker the person started by hand is still kept.
/// NEGATIVE CONTROLS: the renewal before this read a broker that answers as current whatever
/// its job named, so `sync.fabric` recorded ok while no new window's bridge could start;
/// reading a gone program as replaced with no guard for nowhere to go restarts the job onto a
/// program that no longer exists and stops the broker that serves (the step runs and panics);
/// a reason that says the broker still serves where none answers misreports the socket.
#[test]
fn a_program_that_moves_is_due_and_one_with_nowhere_to_go_is_left_as_it_is() {
    use std::cell::Cell;
    let steps = Cell::new(0);
    let dir = crate::test_scratch_dir("en");
    let root = dir.path().join("root");
    aterm_uds::private_file::ensure_private_dir(&root).expect("the root");
    let app = "/Applications/aterm.app/Contents/MacOS/aterm";
    let gone = "/nonexistent/removed/aterm";
    let renew = |renew: Renew, at: Found| {
        renew_with(
            Some(words("aterm link serve")),
            renew,
            |_: &[String]| Ok(rooted(at, &root)),
            |_: &Found, _: Steps| {
                steps.set(steps.get() + 1);
                Ok("broker done reloaded launchd".to_string())
            },
        )
    };
    let moves = |from: &str| {
        Lasting::Moves(Moved {
            from: from.to_string(),
            to: app.to_string(),
        })
    };
    let job_moved = |from: &str, replaced: Option<&str>| {
        let mut at = found(Service::Launchd, replaced);
        at.job_program = moves(from);
        at.p.aterm = app.to_string();
        at
    };

    let look = renew(Renew::Look, job_moved(gone, Some(gone)));
    let Ok(BrokerRenewal::Due { from, to }) = &look else {
        panic!("a gone job program is due: {look:?}");
    };
    assert_eq!(
        from,
        &format!(
            "the broker (pid 4242) runs a replaced build ({gone} names no file now); the broker \
             job runs {gone}, which names no file now (the installed app is {app})"
        )
    );
    assert_eq!(to, &format!("the broker job runs {app}, the installed app"));
    let applied = renew(Renew::Apply, job_moved(gone, Some(gone)));
    assert!(
        matches!(&applied, Ok(BrokerRenewal::Renewed(_))),
        "{applied:?}"
    );
    assert_eq!(steps.get(), 1, "one step");

    let mut both = job_moved(gone, Some(gone));
    both.command_program = moves(gone);
    let both = renew(Renew::Look, both);
    assert!(
        matches!(&both, Ok(BrokerRenewal::Due { to, .. })
        if to == &format!(
            "the broker job runs {app}, the installed app; `[fabric] command` runs {app}, \
             the installed app"
        )),
        "{both:?}"
    );
    let mut command = found(Service::Launchd, None);
    command.command_program = moves(gone);
    let look = renew(Renew::Look, command);
    assert!(
        matches!(&look, Ok(BrokerRenewal::Due { from, to })
            if from == &format!(
                "`[fabric] command` runs {gone}, which names no file now (the installed app is \
                 {app})"
            ) && to == &format!("`[fabric] command` runs {app}, the installed app")),
        "{look:?}"
    );

    let mut by_hand = job_moved(gone, Some(gone));
    by_hand.job = Some(4343);
    let by_hand = renew(Renew::Apply, by_hand);
    assert!(
        matches!(&by_hand, Ok(BrokerRenewal::Kept(why)) if why.contains("not the job")),
        "{by_hand:?}"
    );
    assert_eq!(steps.get(), 1, "nothing stepped since");

    let nowhere_to_go = |answers: Result<(), String>| {
        let mut at = rooted(found(Service::Launchd, Some(gone)), &root);
        at.job_program = Lasting::Nowhere(gone.to_string());
        at.p.aterm = gone.to_string();
        at.answers = answers;
        at
    };
    for (answers, says) in [
        (Ok(()), "start none, so it is left serving"),
        (
            Err("connect: Connection refused (os error 61)".to_string()),
            "would start none, as its program names no file, so it is left as it is",
        ),
    ] {
        for how in [Renew::Look, Renew::Apply] {
            let kept = renew_with(
                Some(words("aterm link serve")),
                how,
                |_: &[String]| Ok(nowhere_to_go(answers.clone())),
                |_: &Found, _: Steps| panic!("no restart onto a program that is gone"),
            );
            assert!(
                matches!(&kept, Ok(BrokerRenewal::Kept(why))
                    if why.contains(&format!(
                        "the broker job runs {gone}, which names no file now, and no installed"
                    )) && why.contains(says)
                        && (answers.is_ok() || !why.contains("serv"))),
                "{how:?} {answers:?}: {kept:?}"
            );
        }
    }

    let mut nowhere = found(Service::Launchd, None);
    nowhere.command_program = Lasting::Nowhere(gone.to_string());
    let left = renew(Renew::Apply, nowhere);
    assert!(
        matches!(&left, Ok(BrokerRenewal::Kept(why))
            if why.starts_with(&format!("`[fabric] command` runs {gone}, which names no file now"))),
        "a command with nowhere to go is not current: {left:?}"
    );
    assert_eq!(steps.get(), 1, "nothing stepped since");
}

/// A `[fabric] command` aterm wrote whose program names no file is judged apart from the broker:
/// whatever keeps the broker as it is (the hand record `on --service none` wrote, no job of
/// `on`'s, a job launchd has disabled or cannot be asked about, a socket whose holder is
/// unknown, a broker that is not the job's process), a look reads the command due, naming what
/// keeps the broker beside what the apply leaves, and an apply steps the command alone.
/// NEGATIVE CONTROL: a renewal that returns the broker's kept reason before it judges the
/// command (as the one before this did) reads each of these Kept, so a command whose program is
/// gone is never rewritten and no new window's bridge starts: the KI's symptom.
#[test]
fn a_command_aterm_wrote_is_rewritten_whatever_keeps_the_broker() {
    let dir = crate::test_scratch_dir("en");
    let root = dir.path().join("root");
    aterm_uds::private_file::ensure_private_dir(&root).expect("the root");
    let by_hand = dir.path().join("by-hand");
    aterm_uds::private_file::ensure_private_dir(&by_hand).expect("a root the person runs");
    aterm_uds::private_file::write_private(&by_hand, HAND_RECORD, b"").expect("by hand");
    let app = "/Applications/aterm.app/Contents/MacOS/aterm";
    let gone = "/nonexistent/removed/aterm";
    let moving = |mut at: Found, root: &Path| {
        at.command_program = Lasting::Moves(Moved {
            from: gone.to_string(),
            to: app.to_string(),
        });
        rooted(at, root)
    };
    // Each broker exit, as the look reads it: the hand record (in its own root), no job of
    // `on`'s, a disabled job, a supervisor that cannot be asked, an unknown holder, a broker
    // that is not the job's process.
    let exit = |case: &str| {
        let mut at = found(Service::Launchd, Some("/old/aterm"));
        match case {
            "by hand" => at = found(Service::Launchd, None),
            "no job" => at = found(Service::None, Some("/old/aterm")),
            "disabled" => at.disabled = true,
            "unasked" => {
                at.loaded = Err("launchd has no gui/501 domain in this login".to_string());
            }
            "unknown holder" => {
                at.view.pid_unknown = Some("the listener scan cannot tell".to_string());
            }
            "not the job" => at.job = Some(4343),
            other => panic!("no such exit: {other}"),
        }
        at
    };
    for (case, at_root, why) in [
        ("by hand", &by_hand, "--service none"),
        ("no job", &root, "no job of `aterm fabric on`'s keeps it"),
        ("disabled", &root, "launchd has"),
        ("unasked", &root, "no gui/501 domain"),
        ("unknown holder", &root, "is unknown"),
        ("not the job", &root, "not the job"),
    ] {
        let look = renew_with(
            Some(words("aterm link serve")),
            Renew::Look,
            |_: &[String]| Ok(moving(exit(case), at_root)),
            |_: &Found, _: Steps| panic!("a look steps nothing"),
        );
        assert!(
            matches!(&look, Ok(BrokerRenewal::Due { from, to })
                if from == &format!(
                    "`[fabric] command` runs {gone}, which names no file now (the installed \
                     app is {app})"
                ) && to.starts_with(&format!(
                    "`[fabric] command` runs {app}, the installed app; left as it is: "
                )) && to.contains(why)),
            "{case}: {look:?}"
        );
        let applied = renew_with(
            Some(words("aterm link serve")),
            Renew::Apply,
            |_: &[String]| Ok(moving(exit(case), at_root)),
            |_: &Found, steps: Steps| {
                assert_eq!(
                    steps,
                    Steps {
                        broker: false,
                        command: true,
                    },
                    "{case}: the command alone"
                );
                Ok("config ok updated [fabric] command".to_string())
            },
        );
        assert!(
            matches!(&applied, Ok(BrokerRenewal::Renewed(said))
                if said.starts_with("config ok updated [fabric] command; left as it is: ")
                    && said.contains(why)),
            "{case}: {applied:?}"
        );
    }
}

/// A scratch fabric for a renewal's look and apply: a root (0700) holding a node id, aterm.toml
/// and the rendezvous file beside it, and the installed app at `<dir>/app/aterm`.
struct World {
    p: Paths,
    node: &'static str,
    app: String,
}

impl World {
    fn new(dir: &Path) -> Self {
        let config_dir = dir.join("cfg/aterm");
        std::fs::create_dir_all(&config_dir).expect("aterm.toml's folder");
        let p = Paths {
            config: Some(config_dir.join("aterm.toml")),
            rendezvous: Some(dir.join("run/fabric.toml")),
            ..scratch_paths(dir)
        };
        aterm_uds::private_file::ensure_private_dir(&p.root).expect("the root");
        let node = "n-0123456789abcdef";
        std::fs::create_dir_all(p.state()).expect("the state dir");
        std::fs::write(p.state().join("node"), node).expect("the node id");
        let app = dir.join("app/aterm");
        std::fs::create_dir_all(app.parent().expect("app/")).expect("app/");
        std::fs::write(&app, b"#!/bin/sh\n").expect("the installed app");
        Self {
            p,
            node,
            app: app.to_string_lossy().into_owned(),
        }
    }

    /// The bridge command `on` writes for this root with `program` as its aterm.
    fn written(&self, program: &str) -> Vec<String> {
        Paths {
            aterm: program.to_string(),
            ..self.p.clone()
        }
        .bridge_command(self.node)
    }

    fn configure(&self, command: &[String]) {
        let (toml, _) = set_fabric_command("", command).expect("a command");
        std::fs::write(self.p.config.as_ref().expect("aterm.toml"), toml).expect("aterm.toml");
    }

    fn configured(&self) -> Option<Vec<String>> {
        let text = std::fs::read_to_string(self.p.config.as_ref().expect("aterm.toml"))
            .expect("aterm.toml");
        fabric::command_in_toml(&text).expect("a [fabric] table")
    }

    /// The launchd job `on` installs for this root, running `program`.
    fn install_job(&self, program: &str) {
        let job = Paths {
            aterm: program.to_string(),
            ..self.p.clone()
        };
        std::fs::write(self.p.plist(), job.plist_text()).expect("the job's plist");
    }

    fn look(&self, command: &[String], service: Service, asks: &Asks<'_>) -> Result<Found, String> {
        look_from(
            self.p.clone(),
            command,
            service,
            true,
            Some(Path::new(&self.app)),
            asks,
        )
    }
}

/// A file at `<dir>/<name>`, made, as a path.
fn a_file(dir: &Path, name: &str) -> String {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().expect("a folder")).expect("its folder");
    std::fs::write(&path, b"#!/bin/sh\n").expect("a file");
    path.to_string_lossy().into_owned()
}

/// THE APPLY, END TO END BUT FOR THE SUPERVISOR AND THE WINDOWS: a look of a scratch root whose
/// `[fabric] command` aterm wrote runs a program that is gone reads it due, and the apply
/// rewrites aterm.toml and the rendezvous file to the installed app and arms the windows with
/// that command, the broker the look found answering, with no broker step while the job's
/// program still names a file; the next pass reads it current. Where the job's program is gone
/// too, the broker step runs first, on the installed app, `on`'s order, then the command is
/// rewritten and the windows armed and waited for on the broker it started.
/// NEGATIVE CONTROLS: a step that runs the broker step for a command alone panics the stand-in;
/// one that rewrites the command before the broker step finds aterm.toml rewritten in the
/// stand-in; a rewrite that writes the config but not the rendezvous file or the windows leaves
/// them naming the gone program; one that asks the socket again under launchd, where the look
/// asked it, panics the stand-in.
#[test]
fn an_apply_steps_the_broker_before_the_command_and_only_what_is_due() {
    use std::cell::Cell;
    let scratch = crate::test_scratch_dir("en");
    let dir = scratch.path();
    let world = World::new(dir);
    let gone = dir.join("removed/aterm").to_string_lossy().into_owned();
    let kept = a_file(dir, "kept/aterm");
    world.install_job(&kept);
    let ours = world.written(&gone);
    let want = world.written(&world.app);
    world.configure(&ours);
    let supervisor = |_: Service, _: &str| (Some(4242), Ok(true), false);
    let broker = |view: &mut fabric::BrokerView| {
        view.pid = Some(4242);
        Ok(())
    };
    let asks = Asks {
        supervisor: &supervisor,
        broker: &broker,
    };
    let look = renew_with(
        Some(ours.clone()),
        Renew::Look,
        |command: &[String]| world.look(command, Service::Launchd, &asks),
        |_: &Found, _: Steps| panic!("a look steps nothing"),
    );
    assert_eq!(
        look,
        Ok(BrokerRenewal::Due {
            from: format!(
                "`[fabric] command` runs {gone}, which names no file now (the installed app is {})",
                world.app
            ),
            to: format!("`[fabric] command` runs {}, the installed app", world.app),
        })
    );

    let panes = Panes::default();
    let reach = Reach {
        broker: &|_, _, _, _| panic!("only the command moves: no broker step"),
        answers: &|_| panic!("the look asked the socket under launchd"),
        windows: &panes,
    };
    let renewed = renew_with(
        Some(ours.clone()),
        Renew::Apply,
        |command: &[String]| world.look(command, Service::Launchd, &asks),
        |at: &Found, steps: Steps| step_with(at, steps, &reach),
    );
    assert!(
        matches!(&renewed, Ok(BrokerRenewal::Renewed(said))
            if said.contains("ok updated [fabric] command")),
        "{renewed:?}"
    );
    assert_eq!(world.configured(), Some(want.clone()), "aterm.toml");
    let rendezvous = std::fs::read_to_string(world.p.rendezvous.as_ref().expect("its path"))
        .expect("the rendezvous file");
    assert_eq!(
        Rendezvous::parse(&rendezvous)
            .expect("a rendezvous")
            .command,
        want,
        "the rendezvous file"
    );
    assert_eq!(
        panes.armed(),
        vec![(want.clone(), ArmWait::Connected)],
        "the windows, waited for: the look found the broker answering"
    );
    let next = renew_with(
        Some(want.clone()),
        Renew::Look,
        |command: &[String]| world.look(command, Service::Launchd, &asks),
        |_: &Found, _: Steps| panic!("nothing is due"),
    );
    assert!(matches!(next, Ok(BrokerRenewal::Current(_))), "{next:?}");

    world.install_job(&gone);
    world.configure(&ours);
    let panes = Panes::default();
    let stepped = Cell::new(false);
    let broker_step = |p: &Paths, _: &str, service: Service, _: &mut Out| {
        assert_eq!(
            world.configured(),
            Some(ours.clone()),
            "the broker step runs before the command is rewritten"
        );
        assert!(
            panes.armed().is_empty(),
            "and before any window is attached"
        );
        assert_eq!(
            (p.aterm.as_str(), service),
            (world.app.as_str(), Service::Launchd)
        );
        stepped.set(true);
        true
    };
    let reach = Reach {
        broker: &broker_step,
        answers: &|_| panic!("the broker step says whether a broker answers"),
        windows: &panes,
    };
    let renewed = renew_with(
        Some(ours.clone()),
        Renew::Apply,
        |command: &[String]| world.look(command, Service::Launchd, &asks),
        |at: &Found, steps: Steps| {
            assert_eq!(
                steps,
                Steps {
                    broker: true,
                    command: true,
                }
            );
            step_with(at, steps, &reach)
        },
    );
    assert!(
        matches!(renewed, Ok(BrokerRenewal::Renewed(_))),
        "{renewed:?}"
    );
    assert!(stepped.get(), "the broker step ran");
    assert_eq!(world.configured(), Some(want.clone()), "then the command");
    assert_eq!(
        panes.armed(),
        vec![(want, ArmWait::Connected)],
        "then the windows, waited for: the broker step went through"
    );
}

/// Each part an apply steps is stepped on its own: a command rewrite that fails (aterm.toml's
/// folder refuses the config service's write lock) still lets the broker step that is due run,
/// and a broker step that fails still lets the command be rewritten, with the rendezvous file,
/// and the windows attached to it but not waited for, no broker answering for them to connect
/// to; either reads as the apply's failure, what each part did said.
/// NEGATIVE CONTROLS: steps chained by `&&` (as the one before this was) skip the broker step
/// after the failed rewrite, or the rewrite after the failed broker step, so a job whose program
/// is gone keeps running it, or a command whose program is gone stays; waiting after a failed
/// broker step waits a minute per window on a broker that does not answer, and fails each.
#[test]
fn a_failed_part_skips_none_of_the_other_parts_steps() {
    use std::cell::Cell;
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = crate::test_scratch_dir("en");
    let dir = scratch.path();
    let world = World::new(dir);
    let gone = dir.join("removed/aterm").to_string_lossy().into_owned();
    world.install_job(&gone);
    let ours = world.written(&gone);
    let want = world.written(&world.app);
    world.configure(&ours);
    let supervisor = |_: Service, _: &str| (Some(4242), Ok(true), false);
    let broker = |view: &mut fabric::BrokerView| {
        view.pid = Some(4242);
        Ok(())
    };
    let asks = Asks {
        supervisor: &supervisor,
        broker: &broker,
    };
    let both = |at: &Found, steps: Steps, reach: &Reach<'_>| {
        assert_eq!(
            steps,
            Steps {
                broker: true,
                command: true,
            }
        );
        step_with(at, steps, reach)
    };

    let folder = world
        .p
        .config
        .as_ref()
        .and_then(|config| config.parent())
        .expect("aterm.toml's folder")
        .to_path_buf();
    let chmod = |mode| {
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(mode)).expect("chmod");
    };
    let stepped = Cell::new(false);
    let unwritten = Panes::default();
    let reach = Reach {
        broker: &|_, _, _, _| {
            stepped.set(true);
            true
        },
        answers: &|_| panic!("the broker step says whether a broker answers"),
        windows: &unwritten,
    };
    chmod(0o555);
    let renewed = renew_with(
        Some(ours.clone()),
        Renew::Apply,
        |command: &[String]| world.look(command, Service::Launchd, &asks),
        |at: &Found, steps: Steps| both(at, steps, &reach),
    );
    chmod(0o755);
    assert!(
        matches!(&renewed, Err(said)
            if said.contains("config FAILED") && !said.contains("config done ")),
        "the lock refused, so no edit landed: {renewed:?}"
    );
    assert!(stepped.get(), "the broker step that is due ran");
    assert!(
        unwritten.armed().is_empty(),
        "no command was written to attach the windows to"
    );
    assert_eq!(
        world.configured(),
        Some(ours.clone()),
        "aterm.toml unwritten"
    );

    let panes = Panes::default();
    let reach = Reach {
        broker: &|_, _, _, out| {
            out.fail("broker", "the stand-in's broker did not start");
            false
        },
        answers: &|_| panic!("the broker step says whether a broker answers"),
        windows: &panes,
    };
    let renewed = renew_with(
        Some(ours),
        Renew::Apply,
        |command: &[String]| world.look(command, Service::Launchd, &asks),
        |at: &Found, steps: Steps| both(at, steps, &reach),
    );
    assert!(
        matches!(&renewed, Err(said)
            if said.contains("broker FAILED") && said.contains(UNWAITED)),
        "{renewed:?}"
    );
    assert_eq!(
        world.configured(),
        Some(want.clone()),
        "the command rewritten"
    );
    let rendezvous = std::fs::read_to_string(world.p.rendezvous.as_ref().expect("its path"))
        .expect("the rendezvous file");
    assert_eq!(
        Rendezvous::parse(&rendezvous)
            .expect("a rendezvous")
            .command,
        want,
        "the rendezvous file"
    );
    assert_eq!(
        panes.armed(),
        vec![(want, ArmWait::Attached)],
        "the windows attached, not waited for"
    );
}

/// Where no broker step runs, the windows with no bridge are attached to the rewritten command
/// whatever answers, and waited for to connect only where a broker answers: on the look's word
/// where it asked the socket (the hand record keeps the broker, and the look found one
/// answering, or none), and under the node service, whose look asks nothing of the broker, on
/// the socket's word when the command is rewritten. Where none answers, aterm.toml and the
/// rendezvous file are still rewritten, the apply goes through, and the line says the windows
/// are attached and not waited for.
/// NEGATIVE CONTROLS: arming nothing where no broker answers leaves the bridgeless windows so
/// for good, since the next pass reads the command current and arms nothing; waiting whatever
/// answers waits a minute per window on a socket nobody serves, and fails each; waiting on the
/// look's `answers` under the node service, which reads `Ok` unasked, does the same.
#[test]
fn the_windows_are_waited_for_only_where_a_broker_answers_after_the_steps() {
    use std::cell::Cell;
    let scratch = crate::test_scratch_dir("en");
    let dir = scratch.path();
    let world = World::new(dir);
    let gone = dir.join("removed/aterm").to_string_lossy().into_owned();
    world.install_job(&a_file(dir, "kept/aterm"));
    aterm_uds::private_file::write_private(&world.p.root, HAND_RECORD, b"").expect("by hand");
    let ours = world.written(&gone);
    let want = world.written(&world.app);
    let supervisor = |_: Service, _: &str| (None, Ok(false), false);
    let refuse = Asks {
        supervisor: &|_, _| panic!("no supervisor is asked under the node service"),
        broker: &|_| panic!("no broker is asked under the node service"),
    };
    for (service, look_answers, now_answers, wait) in [
        (Service::Launchd, false, None, ArmWait::Attached),
        (Service::Launchd, true, None, ArmWait::Connected),
        (Service::Node, true, Some(false), ArmWait::Attached),
        (Service::Node, true, Some(true), ArmWait::Connected),
    ] {
        let case = format!("{service:?}, look {look_answers}, now {now_answers:?}");
        world.configure(&ours);
        let broker = |_: &mut fabric::BrokerView| {
            if look_answers {
                Ok(())
            } else {
                Err("connect: no broker on the stand-in's socket".to_string())
            }
        };
        let asks = Asks {
            supervisor: &supervisor,
            broker: &broker,
        };
        let asked = Cell::new(0);
        let answers = |_: &Found| {
            asked.set(asked.get() + 1);
            now_answers.expect("the look's answer stands where it asked the socket")
        };
        let panes = Panes::default();
        let reach = Reach {
            broker: &|_, _, _, _| panic!("{case}: the broker is kept, no broker step"),
            answers: &answers,
            windows: &panes,
        };
        let renewed = renew_with(
            Some(ours.clone()),
            Renew::Apply,
            |command: &[String]| {
                world.look(
                    command,
                    service,
                    if service == Service::Node {
                        &refuse
                    } else {
                        &asks
                    },
                )
            },
            |at: &Found, steps: Steps| {
                assert_eq!(
                    steps,
                    Steps {
                        broker: false,
                        command: true,
                    },
                    "{case}"
                );
                step_with(at, steps, &reach)
            },
        );
        let Ok(BrokerRenewal::Renewed(said)) = &renewed else {
            panic!("{case}: {renewed:?}");
        };
        assert_eq!(world.configured(), Some(want.clone()), "{case}: aterm.toml");
        assert_eq!(
            panes.armed(),
            vec![(want.clone(), wait)],
            "{case}: the windows"
        );
        assert_eq!(
            said.contains(UNWAITED),
            wait == ArmWait::Attached,
            "{case}: {said}"
        );
        assert_eq!(asked.get(), usize::from(now_answers.is_some()), "{case}");
    }
}

/// Stand-in windows for an apply's attach ([`Windows`]): one running window with no bridge (pid
/// 4242), beside, where `already` names a command, one armed with it already (pid 4343). A
/// window's control socket answers `fabric attach` OK and `fabric status` connected, each
/// request recorded with the window it went to; a pause panics, as the bridge connects at the
/// first ask.
#[derive(Default)]
struct Panes {
    already: Option<Vec<String>>,
    asked: std::cell::RefCell<Vec<(u32, String)>>,
}

impl Windows for Panes {
    fn list(&self, _: &mut Out) -> (Vec<Instance>, Option<usize>) {
        let window = |pid, command: Option<Vec<String>>| Instance {
            pid,
            sock: format!("stand-in-{pid}.sock"),
            supervised: command.is_some(),
            state: if command.is_some() { "connected" } else { "-" }.to_string(),
            reason: "-".to_string(),
            command,
        };
        let mut listed = vec![window(4242, None)];
        listed.extend(
            self.already
                .clone()
                .map(|command| window(4343, Some(command))),
        );
        let running = listed.len();
        (listed, Some(running))
    }

    fn open<'a>(&'a self, inst: &Instance) -> io::Result<Request<'a>> {
        let pid = inst.pid;
        Ok(Box::new(move |line| {
            self.asked.borrow_mut().push((pid, line.to_string()));
            Ok(crate::ctl::Reply::Status(
                if line == "fabric status" {
                    "OK fabric state=connected reason=-"
                } else {
                    "OK attached command=-"
                }
                .to_string(),
            ))
        }))
    }

    fn pause(&self, _: Duration) {
        panic!("the stand-in's bridge connects at the first ask")
    }
}

impl Panes {
    /// Each command a window was attached to, with whether it was then waited for: its
    /// `fabric status` asked after the attach.
    fn armed(&self) -> Vec<(Vec<String>, ArmWait)> {
        let asked = self.asked.borrow();
        asked
            .iter()
            .enumerate()
            .filter_map(|(at, (pid, line))| {
                let argv = line.strip_prefix("fabric attach ")?;
                let waited = asked
                    .get(at + 1)
                    .is_some_and(|(next, line)| next == pid && line == "fabric status");
                Some((
                    argv.split(' ').map(str::to_owned).collect(),
                    if waited {
                        ArmWait::Connected
                    } else {
                        ArmWait::Attached
                    },
                ))
            })
            .collect()
    }
}

/// `arm_instances` attaches each listed window that has no bridge, over that window's own
/// control socket, and waits for it as it is told: under `ArmWait::Connected` the window's
/// `fabric status` is asked after its attach, under `ArmWait::Attached` nothing is. A window
/// armed with this command already is said as armed, and neither opened nor attached. The
/// windows armed for the proof are the one armed already and, where it was waited for, the one
/// that connected.
/// NEGATIVE CONTROLS: an `arm_instances` that hands `attach_window` `ArmWait::Connected`
/// whatever it is told asks the window's status under `Attached`, which with no broker fails
/// the window a minute later; one that attaches an armed window sends it a second attach.
#[test]
fn arm_instances_attaches_the_bridgeless_windows_and_waits_as_told() {
    let argv = words(
        "/Applications/aterm.app/Contents/MacOS/aterm link serve --fleet local --broker /b.sock",
    );
    for (wait, for_the_proof) in [
        (ArmWait::Connected, vec![4242, 4343]),
        (ArmWait::Attached, vec![4343]),
    ] {
        let panes = Panes {
            already: Some(argv.clone()),
            ..Panes::default()
        };
        let mut out = Out::kept();
        let (running, armed) = arm_instances(&argv, wait, &panes, &mut out);
        let said = out.said();
        assert_eq!(running, Some(2), "{wait:?}");
        assert_eq!(
            panes.armed(),
            vec![(argv.clone(), wait)],
            "{wait:?}: {said}"
        );
        assert!(
            panes.asked.borrow().iter().all(|(pid, _)| *pid == 4242),
            "{wait:?}: the armed window is not asked"
        );
        let mut pids: Vec<u32> = armed.iter().map(|inst| inst.pid).collect();
        pids.sort_unstable();
        assert_eq!(pids, for_the_proof, "{wait:?}: {said}");
        assert!(!out.failed, "{wait:?}: {said}");
        assert!(
            said.contains("instance already 4343 is armed with this command"),
            "{wait:?}: {said}"
        );
    }
}

/// A window with no bridge is sent `fabric attach` with the command under either wait. Under
/// `ArmWait::Connected` it is then asked `fabric status` until its bridge connects, said as
/// connected and counted armed; under `ArmWait::Attached` nothing more is asked and nothing
/// waited on, and it is said as attached and not waited for, no failure. An attach the window
/// refuses is said as that answer and fails, under either wait, and nothing more is asked: it is
/// never read as a bridge that did not connect.
/// NEGATIVE CONTROLS: an `Attached` that waits asks `fabric status` and pauses (the stand-ins
/// panic), and with no broker would fail each window a minute later as not connected; one that
/// sends no attach leaves the window bridgeless; a refused attach that went on to the wait
/// would ask `fabric status` and say the bridge did not connect instead of the refusal.
#[test]
fn a_window_is_attached_and_waited_for_only_as_its_wait_says() {
    use crate::ctl::Reply;
    let argv = words(
        "/Applications/aterm.app/Contents/MacOS/aterm link serve --fleet local --broker /b.sock",
    );
    let attach = format!("fabric attach {}", argv.join(" "));

    let mut asked = Vec::new();
    let mut out = Out::kept();
    let state = attach_window(
        4242,
        &argv,
        ArmWait::Attached,
        &mut |line: &str| {
            asked.push(line.to_string());
            Ok(Reply::Status("OK attached command=x".to_string()))
        },
        |_| panic!("no wait where no broker answers"),
        &mut out,
    );
    let said = out.said();
    assert_eq!(state, None, "{said}");
    assert_eq!(asked, [attach.clone()], "the attach alone");
    assert!(!out.failed, "{said}");
    assert!(
        said.contains("instance - 4242 attached, not waited for"),
        "{said}"
    );

    let mut asked = Vec::new();
    let mut out = Out::kept();
    let state = attach_window(
        4242,
        &argv,
        ArmWait::Connected,
        &mut |line: &str| {
            asked.push(line.to_string());
            Ok(Reply::Status(if line == "fabric status" {
                "OK fabric state=connected reason=-".to_string()
            } else {
                "OK attached command=x".to_string()
            }))
        },
        |_| panic!("connected at the first ask"),
        &mut out,
    );
    let said = out.said();
    assert_eq!(state.as_deref(), Some("connected"), "{said}");
    assert_eq!(asked, [attach.clone(), "fabric status".to_string()]);
    assert!(!out.failed, "{said}");
    assert!(
        said.contains("instance ok 4242 fabric=connected after "),
        "{said}"
    );

    for wait in [ArmWait::Attached, ArmWait::Connected] {
        let mut asked = Vec::new();
        let mut out = Out::kept();
        let state = attach_window(
            4242,
            &argv,
            wait,
            &mut |line: &str| {
                asked.push(line.to_string());
                Ok(Reply::Status(
                    "ERR fabric not executable program=%2Fx reason=not-found".to_string(),
                ))
            },
            |_| panic!("a refused attach is not waited on"),
            &mut out,
        );
        let said = out.said();
        assert_eq!(state, None, "{wait:?}: {said}");
        assert_eq!(
            asked,
            [attach.clone()],
            "{wait:?}: nothing after the refusal"
        );
        assert!(out.failed, "{wait:?}: {said}");
        assert!(
            said.contains("4242: fabric attach answered ERR fabric not executable"),
            "{wait:?}: {said}"
        );
        assert!(
            !said.contains("did not connect") && !said.contains("not waited for"),
            "{wait:?}: {said}"
        );
    }
}

/// A save by another writer between `config_step`'s read of aterm.toml and its write (a
/// Settings save of another key, which `ConfigEdit::replace` refuses as a change since the read)
/// is no failure: the step reads aterm.toml again and writes the judged command over what the
/// save left, the save's key kept; a save in that window that changed the judged command itself
/// leaves it as it is, said as kept; a save in the second window too leaves a judged command for
/// the next pass, said as aterm.toml changed again (not as the command changed), and fails
/// `on`'s, which judges none, as a write error does. No outcome loses the other writer's save.
/// The `done` line is said, and counted, once, for the edit that landed, and not at all where
/// none did.
/// NEGATIVE CONTROLS: a step that reads the refusal as a failed write (as the one before this
/// did) says FAILED of an aterm.toml that a Settings save touched, and the renewal fails though
/// nothing went wrong; one that writes over without reading again loses the save; one that says
/// `done` before its replace says it of a refused edit, and counts a change none made.
#[test]
fn a_save_between_the_read_and_the_write_is_read_again_not_failed() {
    let scratch = crate::test_scratch_dir("en");
    let dir = scratch.path();
    let world = World::new(dir);
    let config = world.p.config.clone().expect("aterm.toml");
    let gone = dir.join("removed/aterm").to_string_lossy().into_owned();
    let ours = world.written(&gone);
    let want = world.written(&world.app);
    let save = {
        let config = config.clone();
        move |key: &str| {
            let text = std::fs::read_to_string(&config).expect("aterm.toml");
            std::fs::write(&config, format!("{key} = \"set\"\n{text}")).expect("a save");
        }
    };
    let holds = |key: &str| {
        std::fs::read_to_string(&config)
            .expect("aterm.toml")
            .starts_with(&format!("{key} = \"set\"\n"))
    };

    for judged in [Some(ours.as_slice()), None] {
        world.configure(&ours);
        let saved = save.clone();
        between(move || saved("theme"));
        let mut out = Out::kept();
        let set = config_step(Some(&config), judged, &want, &mut out);
        let said = out.said();
        assert_eq!(set, ConfigSet::Set, "{judged:?}: {said}");
        assert!(!out.failed, "{said}");
        let lines: Vec<&str> = said.split("; ").collect();
        let shown = config.display();
        assert_eq!(
            lines.first().copied(),
            Some(
                format!(
                    "config - {shown} changed between this step's read and its write: reading it \
                     again"
                )
                .as_str()
            ),
            "the refused edit says nothing of a write: {said}"
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("config done "))
                .count(),
            1,
            "{said}"
        );
        assert!(
            lines[1].starts_with(&format!("config done update [fabric] command in {shown}")),
            "the landed edit's done line follows the refusal: {said}"
        );
        assert_eq!(out.changed, 1, "one change: {said}");
        assert_eq!(world.configured(), Some(want.clone()), "{judged:?}");
        assert!(holds("theme"), "the save's key kept");
    }

    world.configure(&ours);
    let mut persons = world.written(&a_file(dir, "mine/aterm"));
    persons.push("--verbose".to_string());
    let (theirs, mine) = (persons.clone(), world.p.clone());
    between(move || {
        let (toml, _) = set_fabric_command("", &theirs).expect("a command");
        std::fs::write(mine.config.as_ref().expect("aterm.toml"), toml).expect("a save");
    });
    let mut out = Out::kept();
    let set = config_step(Some(&config), Some(&ours), &want, &mut out);
    let said = out.said();
    assert_eq!(set, ConfigSet::Changed, "{said}");
    assert!(!out.failed, "{said}");
    assert!(
        said.contains("kept [fabric] command in ")
            && said.contains("changed since it was read; the next pass judges it"),
        "{said}"
    );
    assert!(!said.contains("config done "), "{said}");
    assert_eq!(out.changed, 0, "{said}");
    assert_eq!(
        world.configured(),
        Some(persons),
        "the person's command stays"
    );

    for (judged, want_set) in [
        (Some(ours.as_slice()), ConfigSet::Changed),
        (None, ConfigSet::Failed),
    ] {
        world.configure(&ours);
        let (first, second) = (save.clone(), save.clone());
        between(move || {
            first("theme");
            between(move || second("font"));
        });
        let mut out = Out::kept();
        let set = config_step(Some(&config), judged, &want, &mut out);
        let said = out.said();
        assert_eq!(set, want_set, "{judged:?}: {said}");
        assert_eq!(out.failed, want_set == ConfigSet::Failed, "{said}");
        let verdict = if judged.is_some() { "kept" } else { "FAILED" };
        assert!(
            said.ends_with(&format!(
                "config {verdict} {} changed again between this step's read and its write; {}",
                config.display(),
                if judged.is_some() {
                    "the next pass judges [fabric] command"
                } else {
                    "run this again"
                }
            )),
            "{judged:?}: {said}"
        );
        assert!(
            !said.contains("config done ") && !said.contains("changed since it was read"),
            "{judged:?}: no edit landed, and the command did not change: {said}"
        );
        assert_eq!(out.changed, 0, "{judged:?}: {said}");
        assert_eq!(world.configured(), Some(ours.clone()), "{judged:?}: left");
        assert!(holds("font"), "the second save kept");
    }
}

/// The rewrite is a compare-and-swap: a `[fabric] command` aterm.toml no longer holds as the
/// look read it (a person's edit, or an `off` or `on`, between the look and the step) is left
/// as it is, said as kept, and neither the rendezvous file nor any window is touched; the
/// renewal is no failure, and the next pass judges what is there.
/// NEGATIVE CONTROL: a rewrite that sets the command over whatever aterm.toml holds, as `on`
/// does, replaces the person's newer command with the installed app's, and writes the
/// rendezvous file and arms the windows with it.
#[test]
fn a_command_rewrite_leaves_one_that_changed_since_the_look() {
    let scratch = crate::test_scratch_dir("en");
    let dir = scratch.path();
    let world = World::new(dir);
    let gone = dir.join("removed/aterm").to_string_lossy().into_owned();
    world.install_job(&a_file(dir, "kept/aterm"));
    let ours = world.written(&gone);
    world.configure(&ours);
    let mut persons = world.written(&a_file(dir, "mine/aterm"));
    persons.push("--verbose".to_string());
    let supervisor = |_: Service, _: &str| (Some(4242), Ok(true), false);
    let broker = |view: &mut fabric::BrokerView| {
        view.pid = Some(4242);
        Ok(())
    };
    let asks = Asks {
        supervisor: &supervisor,
        broker: &broker,
    };
    let panes = Panes::default();
    let reach = Reach {
        broker: &|_, _, _, _| panic!("only the command moves: no broker step"),
        answers: &|_| panic!("the look asked the socket under launchd"),
        windows: &panes,
    };
    let renewed = renew_with(
        Some(ours),
        Renew::Apply,
        |command: &[String]| {
            let at = world.look(command, Service::Launchd, &asks);
            world.configure(&persons);
            at
        },
        |at: &Found, steps: Steps| step_with(at, steps, &reach),
    );
    assert!(
        matches!(&renewed, Ok(BrokerRenewal::Renewed(said))
            if said.contains("config kept [fabric] command in ")
                && said.contains("changed since it was read; the next pass judges it")),
        "{renewed:?}"
    );
    assert_eq!(
        world.configured(),
        Some(persons),
        "the person's command stays"
    );
    assert!(
        !world.p.rendezvous.as_ref().expect("its path").exists(),
        "no rendezvous file written"
    );
    assert!(panes.armed().is_empty(), "no window armed");
}

/// Under the node service a look judges the command alone: it asks neither the supervisor nor
/// the socket, and does not judge the job's program, though a plist an earlier build installed
/// names a program that is gone; the command aterm wrote, whose program is gone, still moves to
/// the installed app. The same look under launchd asks each once and judges the job's program.
/// NEGATIVE CONTROLS: a look that reads the job and the broker under the node service panics
/// the stand-ins; one that leaves before judging the command (as the renewal's early exit for
/// the node service once did) reads the command as staying. The launchd look is the control
/// that the stand-ins are live.
#[test]
fn under_the_node_service_a_look_judges_the_command_alone() {
    use std::cell::Cell;
    let scratch = crate::test_scratch_dir("en");
    let dir = scratch.path();
    let world = World::new(dir);
    let gone = dir.join("removed/aterm").to_string_lossy().into_owned();
    world.install_job(&gone);
    let ours = world.written(&gone);
    world.configure(&ours);
    let moved = Lasting::Moves(Moved {
        from: gone.clone(),
        to: world.app.clone(),
    });
    let refuse = Asks {
        supervisor: &|_, _| panic!("no supervisor is asked under the node service"),
        broker: &|_| panic!("no broker is asked under the node service"),
    };
    let at = world.look(&ours, Service::Node, &refuse).expect("the look");
    assert_eq!(at.service, Service::Node);
    assert_eq!(at.command_program, moved, "the command is judged");
    assert_eq!(at.job_program, Lasting::Stays, "the job's program is not");
    assert_eq!(
        (at.job, at.loaded.clone(), at.disabled, at.answers.clone()),
        (None, Ok(false), false, Ok(()))
    );
    assert_eq!(at.view.pid, None, "no holder read");

    let asked = Cell::new(0);
    let supervisor = |_: Service, _: &str| {
        asked.set(asked.get() + 1);
        (Some(4242), Ok(true), false)
    };
    let broker = |_: &mut fabric::BrokerView| {
        asked.set(asked.get() + 1);
        Ok(())
    };
    let asks = Asks {
        supervisor: &supervisor,
        broker: &broker,
    };
    let at = world
        .look(&ours, Service::Launchd, &asks)
        .expect("the look");
    assert_eq!(asked.get(), 2, "the supervisor and the socket, once each");
    assert_eq!(at.job_program, moved, "the job's program is judged");
    assert_eq!(at.p.aterm, world.app);
    assert_eq!(at.command_program, moved);
}

/// `aterm fabric off` boots out a job `print` did not say of: a print that said nothing is
/// not read as an unloaded job (whose plist the off would remove while launchd kept its
/// broker running), and the bootout's failure is excused only where `print` then says
/// launchd holds no such job. A job print says is absent, with no plist, is not installed.
/// NEGATIVE CONTROLS: an off that reads an unread print as unloaded runs no bootout of the
/// loaded job; one that excuses every failed bootout hides a job it did not stop; one that
/// excuses none fails an off of a job that was not loaded.
#[test]
fn an_off_boots_out_a_job_print_did_not_say_of() {
    let dir = crate::test_scratch_dir("en");
    let plist = dir.path().join("job.plist");

    std::fs::write(&plist, "old").expect("plist");
    let loaded = FakeLaunchd::new(true, 0);
    loaded.unread.set(1);
    let mut out = Out::kept();
    off_launchd(&loaded, FAKE_LABEL, &plist, &mut out);
    assert_eq!(loaded.calls(), ["print", "bootout"]);
    assert!(!loaded.held(), "booted out");
    assert!(!out.failed, "{}", out.said());
    assert!(!plist.exists(), "removed");

    std::fs::write(&plist, "old").expect("plist");
    let unloaded = FakeLaunchd::new(false, 0);
    unloaded.unread.set(1);
    let mut out = Out::kept();
    off_launchd(&unloaded, FAKE_LABEL, &plist, &mut out);
    assert_eq!(unloaded.calls(), ["print", "bootout", "print"]);
    assert!(!out.failed, "no such job: {}", out.said());
    assert!(!plist.exists(), "removed");

    let silent = FakeLaunchd::new(false, 0);
    silent.unread.set(2);
    let mut out = Out::kept();
    off_launchd(&silent, FAKE_LABEL, &plist, &mut out);
    assert!(
        out.failed && out.said().contains("No such process"),
        "print never said: {}",
        out.said()
    );

    let absent = FakeLaunchd::new(false, 0);
    let mut out = Out::kept();
    off_launchd(&absent, FAKE_LABEL, &plist, &mut out);
    assert_eq!(absent.calls(), ["print"]);
    assert!(out.said().contains("not installed"), "{}", out.said());
}

/// The broker step reloads a job launchd loaded from other arguments than the plist on disk
/// (one written while the job stayed loaded), since a kickstart restarts what launchd loaded:
/// the reload boots it out, writes the plist again and bootstraps it. A job loaded from the
/// plist's arguments, one not loaded, or a `print` that lists none is read as the file's; a
/// plist that is not `on`'s is rewritten as before. The step reads launchd through the seam
/// (`launchd_read`), so the arguments it judges are the ones `print` listed.
/// NEGATIVE CONTROLS: a step that reads only the file kickstarts the older definition; one
/// whose read hands the rewrite no printed arguments does too.
#[test]
fn a_job_loaded_from_older_arguments_is_reloaded_not_kickstarted() {
    let dir = crate::test_scratch_dir("en");
    let p = scratch_paths(dir.path());
    let current = p.plist_text();
    let argv = p.broker_argv();
    let printed = |args: &[String]| {
        format!(
            "gui/501/{} = {{\n\tstate = running\n\tprogram = {}\n\targuments = {{\n{}\t}}\n\n\t\
             inherited environment = {{\n\t\tA => b\n\t}}\n}}\n",
            p.label(),
            args[0],
            args.iter()
                .map(|a| format!("\t\t{a}\n"))
                .collect::<String>()
        )
    };
    assert_eq!(printed_arguments(&printed(&argv)), Some(argv.clone()));
    assert_eq!(
        printed_arguments("gui/501/x = {\n\tstate = running\n}\n"),
        None
    );

    let older = argv[..argv.len() - 2].to_vec();
    let reload = launchd_rewrite(&p, Some(current.clone()), true, &printed(&older))
        .expect("loaded from older arguments: reloaded");
    assert_eq!(reload.text, current);
    assert_eq!(reload.old.as_deref(), Some(current.as_str()));
    for (old, loaded, said, case) in [
        (
            Some(current.clone()),
            true,
            printed(&argv),
            "the plist's arguments",
        ),
        (Some(current.clone()), false, printed(&older), "not loaded"),
        (
            Some(current.clone()),
            true,
            "state = running\n".to_string(),
            "none listed",
        ),
    ] {
        assert!(launchd_rewrite(&p, old, loaded, &said).is_none(), "{case}");
    }
    assert!(
        launchd_rewrite(&p, Some("old".to_string()), true, &printed(&argv)).is_some(),
        "a plist that is not on's"
    );
    assert!(launchd_rewrite(&p, None, false, "").is_some(), "no plist");

    let lc = FakeLaunchd::new(true, 1);
    *lc.plist.borrow_mut() = current.clone();
    let up = launchd_up(
        &lc,
        FAKE_LABEL,
        Path::new(FAKE_PLIST),
        (true, None),
        Some(&reload),
    );
    assert_eq!(up, Ok(Up::Loaded(None)), "{:?}", lc.calls());
    assert_eq!(lc.count("kickstart"), 0, "{:?}", lc.calls());
    assert_eq!(lc.running_from().as_deref(), Some(current.as_str()));

    // The step's own read of launchd (`launchd_read`, the live one's `print` behind the seam):
    // the arguments the job was loaded from are the ones the rewrite is judged on.
    assert_eq!(plist_arguments(&current), argv);
    let older_plist = argv[argv.len() - 2..]
        .iter()
        .fold(current.clone(), |text, a| {
            text.replace(&format!("    <string>{a}</string>\n"), "")
        });
    assert_eq!(plist_arguments(&older_plist), older);
    let lc = FakeLaunchd::new(true, 0);
    *lc.loaded_from.borrow_mut() = older_plist;
    let read = launchd_read(&lc, &p, FAKE_LABEL, Some(current.clone()));
    assert!(
        matches!(&read, Ok((true, None, Some(r))) if r.text == current),
        "a job loaded from older arguments is reloaded"
    );
    *lc.loaded_from.borrow_mut() = current.clone();
    let read = launchd_read(&lc, &p, FAKE_LABEL, Some(current.clone()));
    assert!(
        matches!(read, Ok((true, None, None))),
        "one loaded from the plist's"
    );
    lc.loaded.set(false);
    let read = launchd_read(&lc, &p, FAKE_LABEL, Some(current.clone()));
    assert!(matches!(read, Ok((false, None, None))), "one not loaded");
    lc.unread.set(1);
    let read = launchd_read(&lc, &p, FAKE_LABEL, Some(current.clone()));
    assert!(
        matches!(read, Err(LaunchdJob::Unread)),
        "a print that did not say"
    );
    assert_eq!(lc.count("print"), 4, "{:?}", lc.calls());
}

/// systemd's unit is written before `daemon-reload`, `enable` and `restart`; a step that fails
/// after the write puts the unit it replaced back, so the next pass reads the unit as changed
/// and runs the step again; a unit with no file before is kept, and one already on disk is
/// not written.
/// NEGATIVE CONTROL: a step with no put-back leaves the new unit beside the process the old
/// one started, which the next pass reads as current.
#[test]
fn a_systemd_restart_that_fails_puts_the_unit_it_replaced_back() {
    use std::cell::RefCell;
    let (calls, disk) = (
        RefCell::new(Vec::<String>::new()),
        RefCell::new(b"old".to_vec()),
    );
    let (calls, disk) = (&calls, &disk);
    let failing = |verb: &'static str| {
        move |args: &[&str]| {
            calls.borrow_mut().push(args[0].to_string());
            if args[0] == verb {
                Err(format!(
                    "systemctl --user {} exited 1: failed",
                    args.join(" ")
                ))
            } else {
                Ok(())
            }
        }
    };
    let write = |bytes: &[u8]| {
        calls.borrow_mut().push("write".to_string());
        *disk.borrow_mut() = bytes.to_vec();
        Ok::<(), String>(())
    };

    let why = systemd_up(&failing("restart"), &write, "u", Some(b"old"), b"new")
        .expect_err("the restart failed");
    assert!(
        why.contains("restart u exited 1") && why.contains("put back"),
        "{why}"
    );
    assert_eq!(
        *calls.borrow(),
        ["write", "daemon-reload", "enable", "restart", "write"]
    );
    assert_eq!(*disk.borrow(), b"old", "the next pass reads it as changed");

    calls.borrow_mut().clear();
    *disk.borrow_mut() = Vec::new();
    let why = systemd_up(&failing("enable"), &write, "u", None, b"new").expect_err("failed");
    assert!(!why.contains("put back"), "{why}");
    assert_eq!(*calls.borrow(), ["write", "daemon-reload", "enable"]);
    assert_eq!(*disk.borrow(), b"new", "no unit before: kept");

    calls.borrow_mut().clear();
    assert_eq!(
        systemd_up(&failing("none"), &write, "u", Some(b"new"), b"new"),
        Ok(())
    );
    assert_eq!(*calls.borrow(), ["daemon-reload", "enable", "restart"]);
}

/// `print-disabled`'s rows name a label disabled as macOS 13 and later print it (`=>
/// disabled`) and as earlier ones did (`=> true`); an enabled row, another label, or one that
/// only starts with it, is not.
#[test]
fn a_disabled_job_is_read_off_print_disabled() {
    let text = "\tdisabled services = {\n\
                \t\t\"com.example.other\" => disabled\n\
                \t\t\"systems.alab.astream-broker.n-1\" => enabled\n\
                \t\t\"systems.alab.astream-broker.n-2\" => disabled\n\
                \t\t\"systems.alab.astream-broker.n-3\" => true\n\
                \t\t\"systems.alab.astream-broker.n-4x\" => disabled\n\
                \t}\n";
    assert!(!disabled_in(text, "systems.alab.astream-broker.n-1"));
    assert!(disabled_in(text, "systems.alab.astream-broker.n-2"));
    assert!(disabled_in(text, "systems.alab.astream-broker.n-3"));
    assert!(!disabled_in(text, "systems.alab.astream-broker.n-4"));
    assert!(!disabled_in(text, "systems.alab.astream-broker.n-5"));
}

/// What launchd and systemd say of the job, read fail-closed: only `print`'s 113 is "no such
/// job", any other failure says nothing; a unit a person disabled or masked is theirs; a
/// systemd that does not answer, like a launchd `print` or `print-disabled` that says
/// nothing, is not "stopped" or "enabled".
/// NEGATIVE CONTROLS: `print`'s other failures read as absent (the first review's finding),
/// `mask` not read as a person's choice, and a `MainPID=0` read as a pid each fail it.
#[test]
fn a_supervisor_that_does_not_say_is_not_read_as_a_stopped_job() {
    let running = "\tstate = running\n\tpid = 4242\n";
    assert_eq!(job_of(Some(0), running), LaunchdJob::Loaded(Some(4242)));
    assert_eq!(
        job_of(Some(0), "\tstate = not running\n"),
        LaunchdJob::Loaded(None)
    );
    assert_eq!(job_of(Some(113), ""), LaunchdJob::Absent);
    assert_eq!(job_of(Some(112), ""), LaunchdJob::NoGuiLogin);
    for code in [Some(1), Some(5), Some(37), Some(125), None] {
        assert_eq!(job_of(code, ""), LaunchdJob::Unread, "{code:?}");
    }
    let label = "systems.alab.astream-broker.x";
    assert_eq!(launchd_state(LaunchdJob::Absent, label), (None, Ok(false)));
    assert_eq!(
        launchd_state(LaunchdJob::Loaded(Some(7)), label),
        (Some(7), Ok(true))
    );
    for job in [LaunchdJob::Unread, LaunchdJob::NoGuiLogin] {
        let (pid, loaded) = launchd_state(job, label);
        assert!(pid.is_none() && loaded.is_err(), "{job:?}: {loaded:?}");
    }

    assert_eq!(
        systemd_shown("MainPID=4242\nUnitFileState=enabled\n"),
        (Some(4242), false)
    );
    assert_eq!(
        systemd_shown("UnitFileState=disabled\nMainPID=0\n"),
        (None, true)
    );
    for state in ["masked", "masked-runtime"] {
        assert_eq!(
            systemd_shown(&format!("MainPID=0\nUnitFileState={state}\n")),
            (None, true),
            "{state}"
        );
    }
    for state in ["static", "linked", ""] {
        assert_eq!(
            systemd_shown(&format!("MainPID=0\nUnitFileState={state}\n")),
            (None, false),
            "{state}"
        );
    }
    assert_eq!(
        with_disabled((Some(7), Ok(true)), Ok(true)),
        (Some(7), Ok(true), true)
    );
    let (_, loaded, disabled) = with_disabled(
        (Some(7), Ok(true)),
        Err("`launchctl print-disabled gui/501` exited 1".to_string()),
    );
    assert!(loaded.is_err() && !disabled, "{loaded:?}");
    assert_eq!(
        systemd_state(Ok((Some(9), false))),
        (Some(9), Ok(true), false)
    );
    let (pid, loaded, disabled) = systemd_state(Err("no user bus".to_string()));
    assert!(pid.is_none() && loaded.is_err() && !disabled);
}

/// A socket a process holds is asked twice before it reads as unanswered; one nobody holds
/// once. NEGATIVE CONTROL: asking once fails it.
#[test]
fn a_held_socket_is_asked_again_before_it_reads_unanswered() {
    use std::cell::Cell;
    let asks = Cell::new(0);
    let pauses = Cell::new(0);
    let busy_once = || {
        asks.set(asks.get() + 1);
        if asks.get() == 1 {
            Err("hello: Resource temporarily unavailable".to_string())
        } else {
            Ok(())
        }
    };
    assert_eq!(
        asked_twice(true, busy_once, || pauses.set(pauses.get() + 1)),
        Ok(())
    );
    assert_eq!((asks.get(), pauses.get()), (2, 1));

    asks.set(0);
    let never = || {
        asks.set(asks.get() + 1);
        Err(format!("connect: refused {}", asks.get()))
    };
    assert_eq!(
        asked_twice(true, never, || ()),
        Err("connect: refused 1; asked again: connect: refused 2".to_string())
    );
    asks.set(0);
    assert_eq!(
        asked_twice(false, never, || panic!(
            "nothing holds the socket: no pause"
        )),
        Err("connect: refused 1".to_string())
    );
    assert_eq!(asks.get(), 1);
}

/// A view whose holder is unknown is asked twice, as one naming its holder is; a view nobody
/// holds once. NEGATIVE CONTROL: asking by `view.pid.is_some()` asks the unknown one once.
#[test]
fn an_unknown_holder_is_asked_twice_as_a_named_one_is() {
    use std::cell::Cell;
    let asks = Cell::new(0);
    let pauses = Cell::new(0);
    let never = || {
        asks.set(asks.get() + 1);
        Err("hello: timed out".to_string())
    };
    let unknown = fabric::BrokerView {
        pid_unknown: Some("2 processes hold its listener (71, 72)".to_string()),
        ..fabric::BrokerView::default()
    };
    let named = fabric::BrokerView {
        pid: Some(71),
        ..fabric::BrokerView::default()
    };
    for (view, what) in [(&unknown, "an unknown holder"), (&named, "a named holder")] {
        asks.set(0);
        pauses.set(0);
        assert!(answered(view, never, || pauses.set(pauses.get() + 1)).is_err());
        assert_eq!((asks.get(), pauses.get()), (2, 1), "{what} is asked twice");
    }
    asks.set(0);
    let nobody = fabric::BrokerView::default();
    assert!(answered(&nobody, never, || panic!(
        "nothing holds the socket: no pause"
    ))
    .is_err());
    assert_eq!(asks.get(), 1, "a socket nobody holds is asked once");
}

/// A kept `Out` prints nothing and hands back what it said; a step it ran without a node id
/// fails, naming the remedy.
#[test]
fn a_kept_out_hands_back_its_lines_and_a_root_with_no_node_is_not_stepped() {
    let mut out = Out::kept();
    out.already("plist", "/x.plist");
    out.done("broker", "would restart", "restarted");
    assert_eq!(out.said(), "plist already /x.plist; broker done restarted");
    let mut nodeless = found(Service::Launchd, Some("/old/aterm"));
    nodeless.node = None;
    let refused = step_on(
        &nodeless,
        Steps {
            broker: true,
            command: false,
        },
    );
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.contains("`aterm fabric on` provisions one")),
        "{refused:?}"
    );
}

fn words(line: &str) -> Vec<String> {
    aterm_types::fabric_command::split_line(line)
}

const FAKE_GONE_POLLS: u32 = 600;

/// launchd as the measured Mac's behaved, in memory: `bootout` returns at once while the job
/// stays held for `lingers` more `print`s, a `bootstrap` of a label launchd still holds fails
/// as launchd fails it (`5: Input/output error`), and `kickstart -k` restarts only a loaded
/// job, the process it killed running `lingers` more looks. The plist on disk starts as
/// `old`. Each verb is recorded by its first word, a plist write as `write`.
struct FakeLaunchd {
    clock: std::cell::Cell<Instant>,
    loaded: std::cell::Cell<bool>,
    /// The plist text the loaded job was loaded from.
    loaded_from: std::cell::RefCell<String>,
    /// The plist on disk.
    plist: std::cell::RefCell<String>,
    /// The `print`s a booted-out job is still held for.
    letting_go: std::cell::Cell<u32>,
    /// How many `print`s each bootout leaves it held for.
    lingers: u32,
    /// The `print`s that say nothing ([`LaunchdJob::Unread`]).
    unread: std::cell::Cell<u32>,
    /// Bootstraps that fail even with the label free, before one loads.
    refusals: std::cell::Cell<u32>,
    /// The refused bootstrap (counted from 1) that loads the job all the same; 0 for none.
    loads_anyway: std::cell::Cell<u32>,
    /// The bootstraps refused so far.
    refused: std::cell::Cell<u32>,
    /// Whether a kickstart fails even for a loaded job (a disabled one: `119`).
    kick_refused: std::cell::Cell<bool>,
    /// Whether a plist write fails.
    write_fails: std::cell::Cell<bool>,
    /// The looks the process a `kickstart -k` killed still runs for.
    exiting: std::cell::Cell<u32>,
    /// The pid `print` names while launchd holds the job; `None` names none.
    pid: std::cell::Cell<Option<u32>>,
    calls: std::cell::RefCell<Vec<String>>,
    pauses: std::cell::Cell<u32>,
}

impl FakeLaunchd {
    fn new(loaded: bool, lingers: u32) -> Self {
        Self {
            clock: std::cell::Cell::new(Instant::now()),
            loaded: std::cell::Cell::new(loaded),
            loaded_from: std::cell::RefCell::new("old".to_string()),
            plist: std::cell::RefCell::new("old".to_string()),
            letting_go: std::cell::Cell::new(0),
            lingers,
            unread: std::cell::Cell::new(0),
            refusals: std::cell::Cell::new(0),
            loads_anyway: std::cell::Cell::new(0),
            refused: std::cell::Cell::new(0),
            kick_refused: std::cell::Cell::new(false),
            write_fails: std::cell::Cell::new(false),
            exiting: std::cell::Cell::new(0),
            pid: std::cell::Cell::new(None),
            calls: std::cell::RefCell::new(Vec::new()),
            pauses: std::cell::Cell::new(0),
        }
    }

    fn held(&self) -> bool {
        self.loaded.get() || self.letting_go.get() > 0
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }

    fn count(&self, verb: &str) -> usize {
        self.calls.borrow().iter().filter(|c| *c == verb).count()
    }

    fn on_disk(&self) -> String {
        self.plist.borrow().clone()
    }

    fn running_from(&self) -> Option<String> {
        self.loaded.get().then(|| self.loaded_from.borrow().clone())
    }
}

/// The `ProgramArguments` of plist `text`, as `on` writes them (one `<string>` a line); none
/// for a text that is no such plist.
fn plist_arguments(text: &str) -> Vec<String> {
    let Some((_, rest)) = text.split_once("<key>ProgramArguments</key>") else {
        return Vec::new();
    };
    let array = rest.split_once("</array>").map_or(rest, |(array, _)| array);
    array
        .lines()
        .filter_map(|l| l.trim().strip_prefix("<string>")?.strip_suffix("</string>"))
        .map(|a| {
            a.replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&apos;", "'")
                .replace("&amp;", "&")
        })
        .collect()
}

impl Launchd for FakeLaunchd {
    fn print(&self, label: &str) -> (Option<i32>, String) {
        self.calls.borrow_mut().push("print".to_string());
        if self.unread.get() > 0 {
            self.unread.set(self.unread.get() - 1);
            return (None, String::new());
        }
        let held = self.held();
        self.letting_go.set(self.letting_go.get().saturating_sub(1));
        if !held {
            return (Some(LAUNCHCTL_NO_SERVICE), String::new());
        }
        // launchd lists the arguments of the plist it loaded the job from, where it has some.
        let args = plist_arguments(&self.loaded_from.borrow());
        let listed = if args.is_empty() {
            String::new()
        } else {
            format!(
                "\targuments = {{\n{}\t}}\n",
                args.iter()
                    .map(|a| format!("\t\t{a}\n"))
                    .collect::<String>()
            )
        };
        let pid = self
            .pid
            .get()
            .map_or_else(String::new, |pid| format!("\tpid = {pid}\n"));
        (
            Some(0),
            format!("gui/501/{label} = {{\n\tstate = running\n{pid}{listed}}}\n"),
        )
    }

    fn run(&self, args: &[&str]) -> Result<(), String> {
        self.calls.borrow_mut().push(args[0].to_string());
        let failed = |code: i32, why: &str| {
            Err(format!("launchctl {} exited {code}: {why}", args.join(" ")))
        };
        let load = || {
            self.loaded.set(true);
            *self.loaded_from.borrow_mut() = self.on_disk();
        };
        match args[0] {
            "bootout" if self.loaded.get() => {
                self.loaded.set(false);
                self.letting_go.set(self.lingers);
                Ok(())
            }
            "bootout" => failed(3, "Boot-out failed: 3: No such process"),
            "bootstrap" if self.held() => failed(5, "Bootstrap failed: 5: Input/output error"),
            "bootstrap" if self.refusals.get() > 0 => {
                self.refusals.set(self.refusals.get() - 1);
                self.refused.set(self.refused.get() + 1);
                if self.loads_anyway.get() == self.refused.get() {
                    load();
                }
                failed(5, "Bootstrap failed: 5: Input/output error")
            }
            "bootstrap" => {
                load();
                Ok(())
            }
            "kickstart" if self.kick_refused.get() => failed(119, "Service is disabled"),
            "kickstart" if self.loaded.get() => {
                self.exiting.set(self.lingers);
                Ok(())
            }
            "kickstart" => failed(113, "Could not find service"),
            other => panic!("launchctl {other} is not the broker step's"),
        }
    }

    fn write(&self, plist: &Path, text: &str) -> Result<(), String> {
        self.calls.borrow_mut().push("write".to_string());
        if self.write_fails.get() {
            return Err(format!(
                "{}: Permission denied (os error 13)",
                plist.display()
            ));
        }
        *self.plist.borrow_mut() = text.to_string();
        Ok(())
    }

    fn running(&self, pid: u32) -> bool {
        assert_eq!(pid, 4242, "only the pid the job ran is waited for");
        let running = self.exiting.get() > 0;
        self.exiting.set(self.exiting.get().saturating_sub(1));
        running
    }

    fn pause(&self, pause: Duration) {
        self.pauses.set(self.pauses.get() + 1);
        self.clock.set(self.clock.get() + pause);
    }

    fn now(&self) -> Instant {
        self.clock.get()
    }
}

const FAKE_PLIST: &str = "/nonexistent/LaunchAgents/systems.alab.astream-broker.x.plist";
const FAKE_LABEL: &str = "systems.alab.astream-broker.x";

#[test]
fn required_removal_accepts_the_observation_after_three_hundred_polls() {
    let manager = FakeLaunchd::new(false, 0);
    manager.letting_go.set(301);
    assert!(
        let_go(&manager, FAKE_LABEL),
        "the actual later absent observation is accepted"
    );
    assert_eq!(manager.calls().len(), 302);
    assert_eq!(manager.pauses.get(), 301);
}

fn rewrite(old: Option<&str>) -> Rewrite {
    Rewrite {
        text: "new".to_string(),
        old: old.map(str::to_string),
    }
}

/// The renewal measured on m22 after an update: a loaded job booted out and bootstrapped at
/// once failed (launchd's bootout returns before the job is gone) and left the job unloaded.
/// The step now boots a job whose plist changed out, waits for launchd to let it go, and only
/// then writes the plist and bootstraps it, once more after a failed bootstrap; an unchanged
/// plist restarts in place with `kickstart -k`, never unloaded, once the process it killed
/// exits, and a kickstart of a job launchd let go meanwhile loads the plist; a job is never
/// booted out twice, so one a failed bootstrap loaded all the same stays loaded.
/// NEGATIVE CONTROLS: a step that bootstraps with no wait (`let_go` answering `true` at once)
/// bootstraps against a job launchd still holds; one that boots the job out again before its
/// retry (as the handoff's did) unloads the job a failed bootstrap loaded; one that takes a
/// kickstart's return for the old process's exit does not wait for it; one that retries with
/// no pause, or writes the plist before the bootout, is caught by the calls.
#[test]
fn a_reload_waits_for_launchd_to_let_the_job_go_and_an_unchanged_job_restarts_in_place() {
    let (plist, label) = (Path::new(FAKE_PLIST), FAKE_LABEL);

    let changed = FakeLaunchd::new(true, 3);
    assert_eq!(
        launchd_up(
            &changed,
            label,
            plist,
            (true, Some(4242)),
            Some(&rewrite(Some("old")))
        ),
        Ok(Up::Loaded(None))
    );
    assert_eq!(
        changed.calls(),
        [
            "bootout",
            "print",
            "print",
            "print",
            "print",
            "write",
            "bootstrap"
        ],
        "written and bootstrapped once launchd let the job go"
    );
    assert_eq!(changed.pauses.get(), 3, "the wait polled");
    assert_eq!(changed.running_from().as_deref(), Some("new"));

    // `print` stops naming the job before launchd takes the label again: the first bootstrap
    // fails, and the step tries once more, a pause later, never booting the job out again.
    let busy = FakeLaunchd::new(true, 1);
    busy.refusals.set(1);
    let up = launchd_up(
        &busy,
        label,
        plist,
        (true, Some(4242)),
        Some(&rewrite(Some("old"))),
    );
    assert!(
        matches!(&up, Ok(Up::Loaded(Some(first))) if first.contains("launchctl bootstrap")
            && first.contains("5: Input/output error")),
        "{up:?}"
    );
    assert_eq!(
        (busy.count("bootout"), busy.count("bootstrap")),
        (1, 2),
        "{:?}",
        busy.calls()
    );
    assert_eq!(
        busy.pauses.get(),
        2,
        "the wait's look, then the retry's pause"
    );
    assert_eq!(busy.running_from().as_deref(), Some("new"));

    // A bootstrap that fails and loads the job all the same: launchd holds the plist's job,
    // which stays loaded for the wait for the broker to judge.
    let loaded_anyway = FakeLaunchd::new(true, 1);
    loaded_anyway.refusals.set(1);
    loaded_anyway.loads_anyway.set(1);
    let up = launchd_up(
        &loaded_anyway,
        label,
        plist,
        (true, Some(4242)),
        Some(&rewrite(Some("old"))),
    );
    assert!(
        matches!(&up, Ok(Up::Held(failed)) if failed.contains("5: Input/output error")),
        "{up:?}"
    );
    assert_eq!(
        (
            loaded_anyway.count("bootout"),
            loaded_anyway.count("bootstrap")
        ),
        (1, 1),
        "never booted out twice: {:?}",
        loaded_anyway.calls()
    );
    assert_eq!(loaded_anyway.running_from().as_deref(), Some("new"));

    // `kickstart -k` returns while the process it killed still runs: the step waits for it.
    let unchanged = FakeLaunchd::new(true, 3);
    assert_eq!(
        launchd_up(&unchanged, label, plist, (true, Some(4242)), None),
        Ok(Up::InPlace)
    );
    assert_eq!(unchanged.calls(), ["kickstart"], "never unloaded");
    assert_eq!(
        unchanged.pauses.get(),
        3,
        "the killed process was waited for"
    );
    let lingering = FakeLaunchd::new(true, 3 * FAKE_GONE_POLLS);
    let why = launchd_up(&lingering, label, plist, (true, Some(4242)), None)
        .expect_err("the killed process never exits");
    assert!(
        why.contains("still ran") && why.contains("kickstart -k"),
        "{why}"
    );
    assert_eq!(lingering.calls(), ["kickstart"], "never unloaded: {why}");

    // The job went between the look and the kickstart: the plist is loaded, nothing booted out.
    let went = FakeLaunchd::new(false, 3);
    let up = launchd_up(&went, label, plist, (true, Some(4242)), None);
    assert!(
        matches!(&up, Ok(Up::Loaded(Some(first))) if first.contains("launchctl kickstart -k")),
        "{up:?}"
    );
    assert_eq!(went.calls(), ["kickstart", "print", "bootstrap"]);

    let unloaded = FakeLaunchd::new(false, 3);
    assert_eq!(
        launchd_up(&unloaded, label, plist, (false, None), None),
        Ok(Up::Loaded(None))
    );
    assert_eq!(unloaded.calls(), ["bootstrap"], "nothing to boot out");
    let unloaded = FakeLaunchd::new(false, 3);
    assert_eq!(
        launchd_up(
            &unloaded,
            label,
            plist,
            (false, None),
            Some(&rewrite(Some("old")))
        ),
        Ok(Up::Loaded(None))
    );
    assert_eq!(unloaded.calls(), ["write", "bootstrap"]);
    assert_eq!(unloaded.running_from().as_deref(), Some("new"));
}

/// The step leaves no job it found loaded unloaded, nor a plist newer than the job launchd
/// holds: a job launchd never lets go is neither rewritten nor bootstrapped over; a plist
/// that does not load is replaced by the one it replaced, loaded again; a write that fails
/// loads the plist as it was; a kickstart refused for a loaded job (a disabled one) leaves it
/// loaded; a `print` that says nothing is waited out, never read as the job gone; and a
/// bootstrap that keeps failing names each verb's error and says the plist is not loaded.
/// NEGATIVE CONTROLS: a step with no rollback, one that writes the plist before the job is
/// gone, one that boots out a job whose kickstart was refused, and one that reads a silent
/// `print` as the job gone each fail it.
#[test]
fn a_load_that_fails_never_strands_the_job() {
    let (plist, label) = (Path::new(FAKE_PLIST), FAKE_LABEL);
    let eio = "Bootstrap failed: 5: Input/output error";

    let refused = FakeLaunchd::new(true, 2);
    refused.refusals.set(2);
    let why = launchd_up(
        &refused,
        label,
        plist,
        (true, Some(4242)),
        Some(&rewrite(Some("old"))),
    )
    .expect_err("the new plist does not load");
    assert!(why.contains("put back") && why.contains(eio), "{why}");
    assert!(
        why.contains(&format!(
            "launchctl bootstrap gui/{} {} exited 5: {eio}",
            uid(),
            plist.display()
        )),
        "the failing verb and its stderr: {why}"
    );
    assert_eq!(refused.on_disk(), "old", "{why}");
    assert_eq!(refused.running_from().as_deref(), Some("old"), "{why}");
    assert_eq!(
        (refused.count("bootout"), refused.count("bootstrap")),
        (1, 3),
        "{:?}",
        refused.calls()
    );
    assert_eq!(
        refused.pauses.get(),
        4,
        "two looks, the retry's pause and the put-back's"
    );

    // The plist put back does not load either: said so, and never called the new one.
    let refused_again = FakeLaunchd::new(true, 2);
    refused_again.refusals.set(3);
    let why = launchd_up(
        &refused_again,
        label,
        plist,
        (true, Some(4242)),
        Some(&rewrite(Some("old"))),
    )
    .expect_err("neither plist loads");
    assert!(
        why.contains("(the plist it replaced, put back) is installed but launchd has not")
            && why.ends_with(eio),
        "{why}"
    );
    assert_eq!(refused_again.on_disk(), "old", "{why}");
    assert_eq!(refused_again.running_from(), None, "{why}");

    // The plist put back fails to bootstrap and loads all the same: launchd holds that one.
    let held_old = FakeLaunchd::new(true, 2);
    held_old.refusals.set(3);
    held_old.loads_anyway.set(3);
    let why = launchd_up(
        &held_old,
        label,
        plist,
        (true, Some(4242)),
        Some(&rewrite(Some("old"))),
    )
    .expect_err("the new plist does not load");
    assert!(
        why.contains("put back, and launchd holds the job after a bootstrap of it"),
        "{why}"
    );
    assert_eq!(held_old.running_from().as_deref(), Some("old"), "{why}");

    // `print` does not say after the bootstraps fail: launchd may hold the new plist's job,
    // so the old one is not written over it.
    let unsaid = FakeLaunchd::new(false, 0);
    unsaid.refusals.set(2);
    unsaid.unread.set(2);
    let why = launchd_up(
        &unsaid,
        label,
        plist,
        (false, None),
        Some(&rewrite(Some("old"))),
    )
    .expect_err("the new plist does not load");
    assert!(why.contains("(or `launchctl print` did not say)"), "{why}");
    assert_eq!(
        unsaid.calls(),
        ["write", "bootstrap", "print", "bootstrap", "print"],
        "{why}"
    );
    assert_eq!(unsaid.on_disk(), "new", "{why}");

    let fresh = FakeLaunchd::new(false, 0);
    fresh.refusals.set(2);
    let why = launchd_up(&fresh, label, plist, (false, None), Some(&rewrite(None)))
        .expect_err("nothing to put back");
    assert!(
        why.contains("is installed but launchd has not loaded it") && why.ends_with(eio),
        "the last verb's error ends it: {why}"
    );
    assert_eq!(
        fresh.calls(),
        ["write", "bootstrap", "print", "bootstrap", "print"]
    );

    let unwritable = FakeLaunchd::new(true, 1);
    unwritable.write_fails.set(true);
    let why = launchd_up(
        &unwritable,
        label,
        plist,
        (true, Some(4242)),
        Some(&rewrite(Some("old"))),
    )
    .expect_err("the plist could not be written");
    assert!(
        why.contains("could not be written") && why.contains("Permission denied"),
        "{why}"
    );
    assert_eq!(unwritable.running_from().as_deref(), Some("old"), "{why}");
    // The plist as it was fails to bootstrap and loads all the same: still not written.
    let unwritable = FakeLaunchd::new(true, 1);
    unwritable.write_fails.set(true);
    unwritable.refusals.set(1);
    unwritable.loads_anyway.set(1);
    let why = launchd_up(
        &unwritable,
        label,
        plist,
        (true, Some(4242)),
        Some(&rewrite(Some("old"))),
    )
    .expect_err("the plist could not be written");
    assert!(
        why.contains("could not be written") && why.contains("5: Input/output error"),
        "{why}"
    );
    assert_eq!(unwritable.running_from().as_deref(), Some("old"), "{why}");

    let disabled = FakeLaunchd::new(true, 3);
    disabled.kick_refused.set(true);
    let why = launchd_up(&disabled, label, plist, (true, Some(4242)), None)
        .expect_err("kickstart refused");
    assert!(
        why.contains("left loaded as it was") && why.contains("119: Service is disabled"),
        "{why}"
    );
    assert_eq!(
        disabled.calls(),
        ["kickstart", "print"],
        "no bootout: {why}"
    );
    assert!(disabled.loaded.get());
    // A kickstart refused while `print` says nothing: the job may be held, so it is left.
    let unsaid = FakeLaunchd::new(true, 3);
    unsaid.kick_refused.set(true);
    unsaid.unread.set(1);
    let why =
        launchd_up(&unsaid, label, plist, (true, Some(4242)), None).expect_err("kickstart refused");
    assert!(why.contains("left loaded as it was"), "{why}");
    assert_eq!(unsaid.calls(), ["kickstart", "print"], "no bootout: {why}");

    // `print` says nothing twice while launchd still lets the job go: waited out.
    let silent = FakeLaunchd::new(true, 1);
    silent.unread.set(2);
    assert_eq!(
        launchd_up(
            &silent,
            label,
            plist,
            (true, Some(4242)),
            Some(&rewrite(Some("old")))
        ),
        Ok(Up::Loaded(None)),
        "{:?}",
        silent.calls()
    );
    assert_eq!(silent.count("bootstrap"), 1);

    let stuck = FakeLaunchd::new(true, 3 * FAKE_GONE_POLLS);
    let why = launchd_up(
        &stuck,
        label,
        plist,
        (true, Some(4242)),
        Some(&rewrite(Some("old"))),
    )
    .expect_err("launchd never let it go");
    assert!(
        why.contains("launchd still held") && why.contains("neither rewritten nor loaded"),
        "{why}"
    );
    assert_eq!(
        (stuck.count("write"), stuck.count("bootstrap")),
        (0, 0),
        "no write, no bootstrap over a job launchd holds: {why}"
    );
    assert_eq!(stuck.on_disk(), "old");
    assert!(
        stuck.pauses.get() > 0,
        "the retained job was observed again"
    );
}

/// The wait for a starting broker connects until one answers and names its process once,
/// after it answers: no poll that finds nothing names anything.
#[test]
fn a_scheduled_local_broker_is_observed_before_startup_is_refused() {
    let mut polls = 0;
    let view = poll_broker_at(
        || {
            polls += 1;
            if polls == 1 {
                fabric::BrokerView::default()
            } else {
                fabric::BrokerView {
                    reachable: true,
                    attached: true,
                    head: Some(0),
                    ..fabric::BrokerView::default()
                }
            }
        },
        |view| view.pid = Some(71),
        BIND_DEADLINE,
        BIND_DEADLINE,
        Instant::now() - Duration::from_secs(6),
    )
    .expect("the broker completes its local startup");
    assert_eq!(polls, 2);
    assert_eq!(view.pid, Some(71));
    let mut looks = 0;
    let expired = poll_broker_at(
        || {
            looks += 1;
            fabric::BrokerView::default()
        },
        |_| panic!("an unanswered broker must not be named"),
        BIND_DEADLINE,
        BIND_DEADLINE,
        Instant::now() - Duration::from_secs(61),
    );
    assert!(expired.is_err());
    assert_eq!(looks, 1);
}

#[test]
fn the_wait_names_the_broker_once_it_answers() {
    let answered = || fabric::BrokerView {
        reachable: true,
        attached: true,
        head: Some(0),
        ..fabric::BrokerView::default()
    };
    let polls = std::cell::Cell::new(0);
    let named = std::cell::RefCell::new(Vec::new());
    let view = poll_broker(
        || {
            polls.set(polls.get() + 1);
            if polls.get() < 3 {
                fabric::BrokerView::default()
            } else {
                answered()
            }
        },
        |view: &mut fabric::BrokerView| {
            named.borrow_mut().push(polls.get());
            view.pid = Some(71);
        },
        Duration::from_secs(60),
        BIND_DEADLINE,
    )
    .expect("an answer on the third poll");
    assert_eq!(
        named.take(),
        [3],
        "named once, after the poll that answered"
    );
    assert_eq!(view.pid, Some(71));
    let silent = poll_broker(
        || fabric::BrokerView {
            error: Some("connect: refused".to_string()),
            ..fabric::BrokerView::default()
        },
        |_: &mut fabric::BrokerView| panic!("a broker that never answered was named"),
        Duration::ZERO,
        BIND_DEADLINE,
    );
    assert_eq!(
        silent.err().and_then(|view| view.error).as_deref(),
        Some("connect: refused")
    );
}

/// An answered but unreadable broker ends the wait at its refusal bound, independently of
/// the caller's bound for binding.
#[test]
fn a_broker_that_answers_but_cannot_be_read_ends_the_wait_at_the_refusal_bound() {
    let unattached = fabric::BrokerView {
        reachable: true,
        ..fabric::BrokerView::default()
    };
    let skewed = fabric::BrokerView {
        protocol: Some(astream_broker::ProtocolMismatch {
            broker: astream_broker::proto::PROTO_VERSION + 1,
            client: astream_broker::proto::PROTO_VERSION,
        }),
        ..fabric::BrokerView::default()
    };
    for (what, view) in [("unattached", unattached), ("skewed", skewed)] {
        let polls = std::cell::Cell::new(0_u32);
        let ended = poll_broker(
            || {
                polls.set(polls.get() + 1);
                view.clone()
            },
            |_: &mut fabric::BrokerView| panic!("{what}: an unreadable broker was named"),
            Duration::from_secs(60),
            Duration::ZERO,
        );
        assert!(ended.is_err(), "{what}: an unreadable broker is no answer");
        assert!(
            polls.get() <= 2,
            "{what}: the wait ended at the refusal bound, not after {} polls toward the seam's",
            polls.get()
        );
    }
}

/// The wait's poll connects and names no process, though the listener scan finds this
/// process holding the socket it reached. The test reads the scan's holders rather than the
/// one broker it names: a child another test is spawning can hold a copy of the listener
/// until its exec closes it.
#[cfg(unix)]
#[test]
fn the_waits_poll_names_no_process() {
    let dir = aterm_tempfile::Builder::new()
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("a scratch folder short enough for sun_path");
    let p = paths_at(&dir.path().join("root"));
    std::fs::create_dir(&p.root).expect("the root");
    let listener = std::os::unix::net::UnixListener::bind(p.sock()).expect("bind");
    let closer = std::thread::spawn(move || {
        drop(aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        ));
        listener
    });
    let polled = poll_reach(&p, "n-a");
    assert!(!polled.read_ok(), "{polled:?}");
    assert_eq!(polled.pid, None, "the poll names nothing: {polled:?}");
    let scan = aterm_uds::holders::scan_listener_holders(Path::new(&p.sock()), &[])
        .expect("the listener scan");
    assert!(
        scan.holders.contains(&std::process::id()),
        "the scan finds this process holding the listener the poll reached: {scan:?}"
    );
    drop(closer.join().expect("the closer"));
}

#[test]
fn running_bridge_sees_a_stale_one_and_a_bare_pid() {
    let inst = |fabric: Option<&str>, supervised, pids: Vec<u32>| fabric::InstanceView {
        pid: 42,
        fabric: fabric.map(str::to_string),
        supervised,
        bridge_pids: Some(pids),
        node: Some("n-a".to_string()),
        ..fabric::InstanceView::default()
    };
    assert_eq!(
        running_bridge(&[inst(None, None, vec![99])], "n-a"),
        Some(42),
        "a bridge child exists: that is a running bridge"
    );
    assert_eq!(
        running_bridge(&[inst(Some("stale"), Some(false), vec![])], "n-a"),
        Some(42),
        "a stale link is a bridge that still owns the sequence"
    );
    assert_eq!(
        running_bridge(&[inst(Some("connected"), None, vec![])], "n-a"),
        Some(42)
    );
    assert_eq!(
        running_bridge(&[inst(Some("absent"), Some(false), vec![])], "n-a"),
        None
    );
    assert_eq!(running_bridge(&[], "n-a"), None);
    assert_eq!(
        running_bridge(&[inst(Some("connected"), Some(true), vec![99])], "n-ghost"),
        None,
        "n-a's bridge cannot retire n-ghost's rows"
    );
    let unread = fabric::InstanceView {
        node: None,
        ..inst(Some("connected"), Some(true), vec![99])
    };
    assert_eq!(
        running_bridge(&[unread], "n-a"),
        None,
        "a bridge whose node was not read is nobody's: the state dir's lock decides"
    );
    let standby = fabric::InstanceView {
        pid: 7,
        reason: Some("state-locked".to_string()),
        ..inst(Some("stalled"), Some(true), vec![98])
    };
    assert_eq!(
        running_bridge(
            &[
                standby.clone(),
                inst(Some("connected"), Some(true), vec![99])
            ],
            "n-a"
        ),
        Some(42)
    );
    assert_eq!(running_bridge(&[standby], "n-a"), None);
}

#[test]
fn write_atomic_never_writes_through_a_planted_name() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = crate::test_scratch_dir("plant");
    let dir = scratch.path();
    let shared = dir.join("shared");
    std::fs::create_dir_all(&shared).expect("scratch");
    let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
    let victim = dir.join("victim.txt");
    std::fs::write(&victim, "the operator's own file\n").expect("victim");
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let out = shared.join("m7.cap");
    let planted = shared.join(format!("m7.tmp.{}", std::process::id()));
    std::os::unix::fs::symlink(&victim, &planted).expect("plant");
    write_atomic(&out, b"eight grant lines\n", 0o600).expect("write");
    assert_eq!(
        std::fs::read_to_string(&victim).expect("victim"),
        "the operator's own file\n"
    );
    assert_eq!(
        mode(&victim),
        0o644,
        "the victim's mode was changed through the link"
    );
    let meta = std::fs::symlink_metadata(&out).expect("out");
    assert!(meta.file_type().is_file(), "{meta:?}");
    assert_eq!(mode(&out), 0o600);
    assert_eq!(std::fs::read(&out).expect("out"), b"eight grant lines\n");
    assert!(std::fs::symlink_metadata(&planted)
        .expect("planted")
        .file_type()
        .is_symlink());
    let mut names: Vec<String> = std::fs::read_dir(&shared)
        .expect("list")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "m7.cap".to_string(),
            format!("m7.tmp.{}", std::process::id())
        ],
        "a temp was left behind"
    );
    let target = dir.join("elsewhere");
    std::fs::write(&target, "untouched\n").expect("target");
    let linked = shared.join("fabric.toml");
    std::os::unix::fs::symlink(&target, &linked).expect("link");
    write_atomic(&linked, b"key\n", 0o600).expect("write over the link");
    assert_eq!(
        std::fs::read_to_string(&target).expect("target"),
        "untouched\n"
    );
    assert!(std::fs::symlink_metadata(&linked)
        .expect("linked")
        .file_type()
        .is_file());
}

fn paths_at(root: &Path) -> Paths {
    Paths {
        home: root.parent().expect("a parent").to_path_buf(),
        login_home: None,
        root: root.to_path_buf(),
        aterm: "/usr/local/bin/aterm".to_string(),
        fleet: "local".to_string(),
        config: None,
        rendezvous: None,
        durability: None,
    }
}

fn dir_mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::symlink_metadata(path)
        .expect("stat")
        .permissions()
        .mode()
        & 0o777
}

#[test]
fn the_root_step_makes_narrows_or_refuses_by_the_private_directory_rule() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = crate::test_scratch_dir("en");
    let root = dir.path().join("root");
    std::fs::create_dir(&root).expect("root");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let p = paths_at(&root);
    let state = p.state();

    let mut dry = Out::new(true);
    assert!(root_step(&p, &state, &mut dry));
    assert_eq!((dry.changed, dry.failed), (1, false));
    assert_eq!(dir_mode(&root), 0o755, "a dry run narrows nothing");
    assert!(!state.exists(), "a dry run makes nothing");

    let mut run = Out::new(false);
    assert!(root_step(&p, &state, &mut run));
    assert_eq!((run.changed, run.failed), (1, false));
    assert_eq!(dir_mode(&root), 0o700);
    assert_eq!(dir_mode(&state), 0o700);

    let mut again = Out::new(false);
    assert!(root_step(&p, &state, &mut again));
    assert_eq!((again.changed, again.failed), (0, false));

    let linked = dir.path().join("linked");
    std::os::unix::fs::symlink(&root, &linked).expect("link");
    let p = paths_at(&linked);
    let mut out = Out::new(false);
    assert!(!root_step(&p, &p.state(), &mut out));
    assert!(out.failed);
}

/// On macOS a 0700 root carrying an `everyone allow read` entry is open to every user
/// though its mode says otherwise: the root step refuses it, dry or not, and makes no state
/// directory in it; the mint secret's writer refuses that root as well.
#[cfg(target_os = "macos")]
#[test]
fn the_root_step_refuses_a_root_an_access_list_opens_to_others() {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    let dir = crate::test_scratch_dir("en");
    let root = dir.path().join("root");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .expect("root");
    let chmod = |args: &[&str]| {
        let ran = aterm_uds::spawnfd::command("/bin/chmod")
            .args(args)
            .arg(&root)
            .status()
            .expect("run chmod");
        assert!(ran.success(), "chmod {args:?}");
    };
    chmod(&["+a", "everyone allow read"]);
    let p = paths_at(&root);
    let state = p.state();
    for dry_run in [true, false] {
        let mut out = Out::new(dry_run);
        assert!(!root_step(&p, &state, &mut out), "dry_run={dry_run}");
        assert!(out.failed);
        assert!(!state.exists(), "dry_run={dry_run}");
    }
    assert_eq!(
        aterm_uds::private_file::write_private(&root, MINT_SECRET, b"secret")
            .expect_err("a root an access list opens")
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(!p.secret().exists());

    chmod(&["-N"]);
    let mut out = Out::new(false);
    assert!(root_step(&p, &state, &mut out));
    assert_eq!(dir_mode(&state), 0o700);
    aterm_uds::private_file::write_private(&root, MINT_SECRET, b"secret").expect("write");
    let meta = std::fs::symlink_metadata(p.secret()).expect("secret");
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
}

#[test]
fn the_label_is_derived_from_the_root_with_posix_cksum() {
    assert_eq!(
        cksum(b"/Users//example/.local/share/aterm-fabric"),
        2_100_958_200
    );
    assert_eq!(cksum(b""), 0xFFFF_FFFF);
    let home = PathBuf::from("/Users//example");
    let p = |root: &str| Paths {
        home: home.clone(),
        login_home: Some(home.clone()),
        root: PathBuf::from(root),
        aterm: "/usr/local/bin/aterm".to_string(),
        fleet: "local".to_string(),
        config: None,
        rendezvous: None,
        durability: None,
    };
    assert_eq!(p("/Users//example/.local/share/aterm-fabric").label(), LABEL);
    assert_eq!(
        p("/tmp/atfo-x/root").label(),
        format!("{LABEL}.{}", cksum(b"/tmp/atfo-x/root"))
    );
    assert_ne!(p("/tmp/atfo-x/root").label(), LABEL);
}

#[test]
fn review_two_roots_never_share_a_label() {
    let paths = |home: &str, login: Option<&str>| Paths {
        home: PathBuf::from(home),
        login_home: login.map(PathBuf::from),
        root: PathBuf::from(home).join(".local/share/aterm-fabric"),
        aterm: "/usr/local/bin/aterm".to_string(),
        fleet: "local".to_string(),
        config: None,
        rendezvous: None,
        durability: None,
    };
    let live = paths("/Users//example", Some("/Users//example"));
    let redirected = paths("/tmp/x", Some("/Users//example"));
    assert_eq!(live.label(), LABEL);
    assert_ne!(redirected.label(), live.label());
    assert_eq!(
        redirected.label(),
        format!("{LABEL}.{}", cksum(b"/tmp/x/.local/share/aterm-fabric"))
    );
    assert_ne!(paths("/Users//example", None).label(), LABEL);
}

/// THE LOGIN HOME AND THE UID ARE THE ONES `id` AND THE SHELL ANSWER, on
/// the live machine, though no child is run for them: [`login_home`] is
/// `~$(id -un)` as `sh` expands it with no `$HOME` to read, and [`uid`] is
/// `id -u`.
#[cfg(unix)]
#[test]
fn the_login_home_and_uid_are_what_id_and_the_shell_answer() {
    let run = |program: &str, args: &[&str]| {
        let out = aterm_uds::spawnfd::command(program)
            .args(args)
            .env_remove("HOME")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .unwrap_or_else(|e| panic!("run {program}: {e}"));
        assert!(out.status.success(), "{program} {args:?}: {out:?}");
        String::from_utf8(out.stdout)
            .expect("utf-8")
            .trim()
            .to_string()
    };
    let user = run("id", &["-un"]);
    let expanded = run("sh", &["-c", &format!("printf '%s' ~{user}")]);
    assert!(
        Path::new(&expanded).is_absolute(),
        "the shell expanded ~{user} to {expanded:?}"
    );
    assert_eq!(login_home(), Some(PathBuf::from(&expanded)), "~{user}");
    assert_eq!(uid(), run("id", &["-u"]));
}

#[test]
fn the_generated_broker_command_selects_the_open_policy() {
    use crate::durability::Durability;

    let paths = Paths {
        home: PathBuf::from("/fixture-home"),
        login_home: None,
        root: PathBuf::from("/fixture-root"),
        aterm: "/fixture-bin/aterm".to_string(),
        fleet: "local".to_string(),
        config: None,
        rendezvous: None,
        durability: None,
    };
    let expected = Durability::Ordered.options();
    let tier = expected.durability;
    for named in [None, Some(Durability::Ordered)] {
        let paths = Paths {
            durability: named,
            ..paths.clone()
        };
        let argv = paths.broker_argv();
        let serving = Durability::try_of_argv(&argv).expect("a serving broker command");
        let options = serving.options();
        assert_eq!(options.durability, tier);
        assert_eq!(options.escalation, expected.escalation);
    }

    let strict = Paths {
        durability: Some(Durability::Strict),
        ..paths.clone()
    };
    assert_ne!(
        Durability::try_of_argv(&strict.broker_argv())
            .unwrap()
            .tier(),
        tier
    );
    let mut unguarded = paths.broker_argv();
    let guard_at = unguarded
        .iter()
        .position(|word| word == "--secret-file")
        .unwrap();
    unguarded.drain(guard_at..guard_at + 2);
    assert_ne!(Durability::try_of_argv(&unguarded).unwrap().tier(), tier);
}

#[test]
fn the_tier_is_in_the_job_and_a_bare_on_keeps_it() {
    use crate::durability::Durability;
    let dir = crate::test_scratch_dir("en");
    let bare = Paths {
        home: dir.path().to_path_buf(),
        login_home: None,
        root: dir.path().join("root"),
        aterm: "/usr/local/bin/aterm".to_string(),
        fleet: "local".to_string(),
        config: None,
        rendezvous: None,
        durability: None,
    };
    let named = |tier| Paths {
        durability: Some(tier),
        ..bare.clone()
    };
    let relaxed = named(Durability::Relaxed);
    let argv = relaxed.broker_argv();
    assert_eq!(argv[argv.len() - 2..], ["--durability", "relaxed"]);
    assert!(!bare.broker_argv().iter().any(|w| w == "--durability"));
    assert_eq!(bare.broker_argv()[..], argv[..argv.len() - 2]);
    assert_eq!(
        bare.tier(),
        Durability::Ordered,
        "this build's default for `on`'s guarded broker"
    );
    assert_eq!(bare.tier(), crate::durability::default_for(true));
    let strict = named(Durability::Strict);
    let words = strict.broker_argv();
    assert_eq!(
        words[words.len() - 2..],
        ["--durability", "strict"],
        "a named strict is written"
    );
    assert!(!bare.plist_is_current(&relaxed.plist_text()));
    assert!(!relaxed.plist_is_current(&bare.plist_text()));
    assert!(!bare.plist_is_current(&strict.plist_text()));
    assert!(relaxed.plist_is_current(&relaxed.plist_text()));
    for paths in [&bare, &relaxed, &strict] {
        assert_eq!(
            Paths::served_log(&paths.broker_argv()),
            Some(paths.root.join(BUS_LOG)),
            "the job's argv serves the root's log on every tier: {:?}",
            paths.broker_argv()
        );
    }
    let sock = bare.sock();
    assert!(
        crate::broker_args::BrokerArgs::from_invocation(&argv).is_some_and(|args| args.serves(
            &aterm_uds::holders::SocketPath::of(std::path::Path::new(&sock))
        )),
        "the job's argv serves the broker socket: {argv:?}"
    );
    assert_eq!(Durability::of_argv(&argv), Durability::Relaxed);
    assert!(unit_argv(&relaxed.unit_text()).is_some_and(|w| w == argv));

    assert_eq!(kept_durability(&bare), Ok((None, None)));
    std::fs::create_dir_all(&bare.root).unwrap();
    for tier in [Durability::Relaxed, Durability::Ordered, Durability::Strict] {
        std::fs::write(bare.plist(), named(tier).plist_text()).unwrap();
        let (kept, job) = kept_durability(&bare).expect("read");
        assert_eq!(kept, Some(tier));
        assert!(job.is_some_and(|j| j.contains(&bare.plist().display().to_string())));
    }
    std::fs::write(bare.plist(), bare.plist_text()).unwrap();
    assert_eq!(kept_durability(&bare).expect("read").0, None);
}

#[test]
fn a_broker_running_another_tier_is_not_current() {
    use crate::durability::Durability;
    let p = |durability| Paths {
        home: PathBuf::from("/h"),
        login_home: None,
        root: PathBuf::from("/h/root"),
        aterm: "/usr/local/bin/aterm".to_string(),
        fleet: "local".to_string(),
        config: None,
        rendezvous: None,
        durability: Some(durability),
    };
    let view = |durability| fabric::BrokerView {
        guarded: Some(true),
        durability,
        ..fabric::BrokerView::default()
    };
    let (strict, ordered, relaxed) = (Durability::Strict, Durability::Ordered, Durability::Relaxed);
    assert_eq!(
        not_current(&p(strict), &view(Some(relaxed))),
        Some("it runs at durability relaxed")
    );
    assert_eq!(
        not_current(&p(relaxed), &view(Some(strict))),
        Some("it runs at durability strict")
    );
    assert_eq!(
        not_current(&p(strict), &view(Some(ordered))),
        Some("it runs at durability ordered")
    );
    assert_eq!(not_current(&p(relaxed), &view(Some(relaxed))), None);
    assert_eq!(not_current(&p(strict), &view(Some(strict))), None);
    assert_eq!(not_current(&p(ordered), &view(Some(ordered))), None);
    let bare = Paths {
        durability: None,
        ..p(strict)
    };
    let default = crate::durability::default_for(true);
    assert_eq!(not_current(&bare, &view(Some(default))), None);
    if default != strict {
        assert_eq!(
            not_current(&bare, &view(Some(strict))),
            Some("it runs at durability strict"),
            "a bare job no longer asks for strict"
        );
    }
    assert_eq!(not_current(&p(relaxed), &view(None)), None, "no tier read");
    let replaced = fabric::BrokerView {
        replaced: Some("/usr/local/bin/aterm".to_string()),
        ..view(Some(strict))
    };
    assert_eq!(
        not_current(&p(relaxed), &replaced),
        Some("it runs a replaced build")
    );
}

/// The node service's argv names no tier, so it is not current where the tier its log was
/// opened at (its snapshot, else its `Hello`) is not the one it hosts the root at: the
/// root's record, else a guarded root's default, whatever tier `p` names. NEGATIVE CONTROL:
/// the same view read as a broker's compares its argv, which names none, and is never not
/// current for its tier.
#[test]
fn the_node_service_is_not_current_at_another_tier_than_the_root_records() {
    use crate::durability::Durability;
    use astream_broker::{ServedInfo, ServedReason, Tier};
    let dir = crate::test_scratch_dir("en");
    let p = Paths {
        durability: Some(Durability::Strict),
        ..scratch_paths(dir.path())
    };
    aterm_uds::private_file::ensure_private_dir(&p.root).expect("the root");
    let tier = |d: Durability| match d {
        Durability::Strict => Tier::Strict,
        Durability::Ordered => Tier::Ordered,
        Durability::Relaxed => Tier::Relaxed,
    };
    let view = |host, requested: Durability| fabric::BrokerView {
        host,
        guarded: Some(true),
        served: Some(ServedInfo {
            requested: tier(requested),
            served: tier(requested),
            reason: ServedReason::None,
            horizon_ms: None,
            rules: 0,
        }),
        ..fabric::BrokerView::default()
    };
    let node = Some(fabric::Host::NodeService);
    write_durability_record(&p.root, Durability::Relaxed).expect("the root's record");
    assert_eq!(
        not_current(&p, &view(node, Durability::Strict)),
        Some("it runs at durability strict"),
        "the record, not `p`, names the tier the node service hosts"
    );
    assert_eq!(not_current(&p, &view(node, Durability::Relaxed)), None);
    aterm_uds::private_file::remove_private(&p.root, DURABILITY_RECORD).expect("no record");
    let default = crate::durability::resolve(None, true);
    assert_eq!(
        not_current(&p, &view(node, default)),
        None,
        "a guarded root's default"
    );
    let other = if default == Durability::Relaxed {
        Durability::Strict
    } else {
        Durability::Relaxed
    };
    assert!(not_current(&p, &view(node, other)).is_some());
    for host in [Some(fabric::Host::LinkBroker), None] {
        assert_eq!(
            not_current(&p, &view(host, other)),
            None,
            "NEGATIVE CONTROL: a broker's tier is its argv's ({host:?})"
        );
    }
}

/// `doctor` words its remedies for who keeps the bus: the node service where it serves the
/// socket, or, with the mesh on, where nothing is named; a broker job wherever a broker
/// serves it, the mesh is off, or `on --service none` left the root to its person
/// (`hand`). NEGATIVE CONTROL: the hand record changes nothing where a host is named.
#[test]
fn doctor_words_its_remedies_for_who_keeps_the_bus() {
    use fabric::Host::{LinkBroker, NodeService};
    let view = |mesh, host| fabric::BrokerView {
        mesh,
        host,
        ..fabric::BrokerView::default()
    };
    for (mesh, host, hand, want) in [
        (true, Some(NodeService), false, Keeper::Node),
        (true, Some(NodeService), true, Keeper::Node),
        (false, Some(NodeService), false, Keeper::Node),
        (true, Some(LinkBroker), false, Keeper::Job),
        (true, Some(LinkBroker), true, Keeper::Job),
        (true, None, false, Keeper::Node),
        (true, None, true, Keeper::Job),
        (false, None, false, Keeper::Job),
        (false, None, true, Keeper::Job),
    ] {
        assert_eq!(
            keeper_of(&view(mesh, host), hand),
            want,
            "mesh {mesh}, {host:?}, hand {hand}"
        );
    }
}

#[test]
fn the_plist_runs_the_broker_directly_and_reads_as_current_in_any_layout() {
    let p = Paths {
        home: PathBuf::from("/Users//example"),
        login_home: Some(PathBuf::from("/Users//example")),
        root: PathBuf::from("/Users//example/.local/share/aterm-fabric"),
        aterm: "/Users//example/.local/bin/aterm".to_string(),
        fleet: "local".to_string(),
        config: None,
        rendezvous: None,
        durability: None,
    };
    let text = p.plist_text();
    assert!(
        text.contains(
            "  <array>\n\
         \x20   <string>/Users//example/.local/bin/aterm</string>\n\
         \x20   <string>link</string>\n\
         \x20   <string>broker</string>\n\
         \x20   <string>/Users//example/.local/share/aterm-fabric/bus.sock</string>\n\
         \x20   <string>/Users//example/.local/share/aterm-fabric/bus.log</string>\n\
         \x20   <string>--secret-file</string>\n\
         \x20   <string>/Users//example/.local/share/aterm-fabric/mint.secret</string>\n\
         \x20 </array>\n"
        ),
        "{text}"
    );
    let unguarded = text.replace(
        "    <string>--secret-file</string>\n    \
         <string>/Users//example/.local/share/aterm-fabric/mint.secret</string>\n",
        "",
    );
    assert_ne!(unguarded, text);
    assert!(!p.plist_is_current(&unguarded), "{unguarded}");
    assert!(
        !text.contains("/bin/sh") && !text.contains("rm -f"),
        "{text}"
    );
    assert!(text.contains("  <key>Label</key><string>systems.alab.astream-broker</string>\n"));
    assert!(text.contains("  <key>KeepAlive</key><true/>\n"));
    assert!(
        text.contains(
            "  <key>EnvironmentVariables</key>\n\
         \x20 <dict>\n\
         \x20   <key>PATH</key><string>/Users//example/Library/Application Support/aterm/pkg/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>\n\
         \x20 </dict>\n"
        ),
        "{text}"
    );
    assert!(text.ends_with("</dict>\n</plist>\n"));
    assert!(p.plist_is_current(&text));
    assert_eq!(
        plist_program_arguments(&text).as_deref(),
        Some(p.broker_argv().as_slice())
    );
    let plistlib = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
         \t<key>KeepAlive</key>\n\t<true/>\n\
         \t<key>Label</key>\n\t<string>systems.alab.astream-broker</string>\n\
         \t<key>ProgramArguments</key>\n\t<array>\n\
         \t\t<string>/Users//example/.local/bin/aterm</string>\n\t\t<string>link</string>\n\t\t<string>broker</string>\n\
         \t\t<string>/Users//example/.local/share/aterm-fabric/bus.sock</string>\n\
         \t\t<string>/Users//example/.local/share/aterm-fabric/bus.log</string>\n\
         \t\t<string>--secret-file</string>\n\
         \t\t<string>/Users//example/.local/share/aterm-fabric/mint.secret</string>\n\t</array>\n\
         \t<key>RunAtLoad</key>\n\t<true/>\n</dict>\n</plist>\n";
    assert!(p.plist_is_current(plistlib));
    assert!(!p.plist_is_current(&text.replace("bus.log", "other.log")));
    assert!(
        !p.plist_is_current(&text.replace("astream-broker</string>", "astream-broker.1</string>"))
    );
    assert!(!p.plist_is_current(&text.replace(
        "<key>KeepAlive</key><true/>",
        "<key>KeepAlive</key><false/>"
    )));
    assert!(!p.plist_is_current("<plist/>"));
    let odd = Paths {
        root: PathBuf::from("/tmp/a&b<c"),
        login_home: None,
        ..p.clone()
    };
    let odd_text = odd.plist_text();
    assert!(
        odd_text.contains("<string>/tmp/a&amp;b&lt;c/bus.sock</string>"),
        "{odd_text}"
    );
    assert!(odd_text.contains("<key>WorkingDirectory</key><string>/tmp/a&amp;b&lt;c</string>"));
    assert!(!odd_text.contains("a&b"), "{odd_text}");
    assert!(odd.plist_is_current(&odd_text));
    assert_eq!(
        plist_program_arguments(&collapse_between_tags(&odd_text)).expect("argv")[3],
        "/tmp/a&b<c/bus.sock"
    );
    let shim = Paths {
        aterm: "/x/target/debug/aterm-link".to_string(),
        ..p.clone()
    };
    assert_eq!(
        shim.broker_argv(),
        [
            "/x/target/debug/aterm-link",
            "broker",
            "/Users//example/.local/share/aterm-fabric/bus.sock",
            "/Users//example/.local/share/aterm-fabric/bus.log",
            "--secret-file",
            "/Users//example/.local/share/aterm-fabric/mint.secret"
        ]
    );
    let unit = p.unit_text();
    assert!(!unit.contains("rm -f"), "{unit}");
    assert!(
        unit.contains(
            "\nStandardError=append:/Users//example/.local/share/aterm-fabric/broker.err\n"
        ),
        "{unit}"
    );
    assert!(
        p.plist_text().contains(
            "<key>StandardErrorPath</key><string>/Users//example/.local/share/aterm-fabric/\
             broker.err</string>"
        ),
        "the same file"
    );
    assert!(
        unit.contains(
            "ExecStart=/bin/sh -c \"exec '/Users//example/.local/bin/aterm' 'link' 'broker' \
             '/Users//example/.local/share/aterm-fabric/bus.sock' \
             '/Users//example/.local/share/aterm-fabric/bus.log' '--secret-file' \
             '/Users//example/.local/share/aterm-fabric/mint.secret'\"\n"
        ),
        "{unit}"
    );
    assert_eq!(shim.bridge_command("n-a")[1], "serve");
    assert_eq!(
        shim.label(),
        LABEL,
        "the root, not the binary, decides the label"
    );
}

#[test]
fn the_plist_follows_the_root() {
    let p = |root: &str, login: Option<&str>| Paths {
        home: PathBuf::from("/Users//example"),
        login_home: login.map(PathBuf::from),
        root: PathBuf::from(root),
        aterm: "/usr/local/bin/aterm".to_string(),
        fleet: "local".to_string(),
        config: None,
        rendezvous: None,
        durability: None,
    };
    let installed = p(
        "/Users//example/.local/share/aterm-fabric",
        Some("/Users//example"),
    );
    assert_eq!(
        installed.plist(),
        PathBuf::from("/Users//example/Library/LaunchAgents/systems.alab.astream-broker.plist")
    );
    let redirected = p("/tmp/atfo-x/root", Some("/Users//example"));
    assert_eq!(
        redirected.plist(),
        PathBuf::from(format!("/tmp/atfo-x/root/{}.plist", redirected.label()))
    );
    assert!(!redirected.plist().starts_with("/Users//example/Library"));
    let foreign = Paths {
        home: PathBuf::from("/tmp/x"),
        ..p("/tmp/x/.local/share/aterm-fabric", Some("/Users//example"))
    };
    assert!(foreign
        .plist()
        .starts_with("/tmp/x/.local/share/aterm-fabric/"));
    let redirected_home = Paths {
        home: PathBuf::from("/tmp/x"),
        ..installed
    };
    assert!(!redirected_home
        .plist()
        .starts_with("/Users//example/Library/LaunchAgents"));
}

#[test]
fn the_launchd_failure_hint_names_what_was_written() {
    let scratch = crate::test_scratch_dir("hint");
    let dir = scratch.path();
    let absent = launchd_failure_hint(dir, "systems.alab.astream-broker.7", "501");
    assert!(
        absent.contains("broker.err was not written")
            && absent.contains("launchctl print gui/501/systems.alab.astream-broker.7"),
        "{absent}"
    );
    let absent = systemd_failure_hint(dir, "astream-broker-7");
    assert!(
        absent.contains("broker.err was not written")
            && absent.contains("journalctl --user -u astream-broker-7"),
        "{absent}"
    );
    std::fs::write(dir.join("broker.err"), "").expect("empty");
    assert!(launchd_failure_hint(dir, "l", "501").contains("was not written"));
    std::fs::write(dir.join("broker.err"), " \n\n").expect("blank");
    assert_eq!(supervisor_last_word(dir), None, "whitespace is not a word");
    std::fs::write(
        dir.join("broker.err"),
        "bind: address in use\naterm link broker: open bus.log: refused\n\n",
    )
    .expect("err");
    let written = launchd_failure_hint(dir, "l", "501");
    assert!(
        written.starts_with(&format!(
            "read {} (written ",
            dir.join("broker.err").display()
        )) && written.ends_with("ago: aterm link broker: open bus.log: refused)"),
        "{written}"
    );
    assert_eq!(
        systemd_failure_hint(dir, "l").split(" (written ").next(),
        written.split(" (written ").next(),
        "a unit that writes broker.err is read the same way"
    );
    // A dated line (`aterm_uds::job_log::line`) is quoted without its date, and one that is only
    // a date is as blank as an empty one: the line before it is the last word.
    std::fs::write(
        dir.join("broker.err"),
        "2026-11-17T09:41:07Z aterm link broker: open bus.log: refused\n\
         2026-11-17T09:41:08Z\n2026-11-17T09:41:09Z \n",
    )
    .expect("dated");
    assert!(
        supervisor_last_word(dir)
            .is_some_and(|last| last.ends_with("ago: aterm link broker: open bus.log: refused")),
        "{:?}",
        supervisor_last_word(dir)
    );
    std::fs::write(dir.join("broker.err"), "2026-11-17T09:41:08Z\n").expect("a date alone");
    assert_eq!(
        supervisor_last_word(dir),
        None,
        "a date alone is not a word"
    );
}

/// A broker job's definition and its stand-in in `root`.
fn job_paths(root: &Path) -> Paths {
    Paths {
        home: PathBuf::from("/Users//example"),
        login_home: Some(PathBuf::from("/Users//example")),
        root: root.to_path_buf(),
        aterm: "/Users//example/.local/bin/aterm".to_string(),
        fleet: "local".to_string(),
        config: None,
        rendezvous: None,
        durability: None,
    }
}

/// The full name of [`the_fabric_jobs_log_stays_under_the_bound_through_a_burst_of_refused_attaches`].
const FABRIC_BURST: &str =
    "enable::tests::the_fabric_jobs_log_stays_under_the_bound_through_a_burst_of_refused_attaches";

/// The line astream's broker writes to its standard error for each refused attach.
const REFUSED_ATTACH: &str =
    "astream-broker: warn: refused Attach for grant 7: the grant was revoked";

/// **THE FABRIC JOB'S LOG STAYS UNDER THE BOUND THROUGH A BURST OF REFUSED ATTACHES.** A
/// stand-in broker (this binary run again in a scratch root, its standard output and error opened
/// as the plist names them, or as the `systemd --user` unit does, standard output then the
/// journal) keeps its job's files (`keep_broker_log` of its socket, the broker verb's first step)
/// and writes astream's `refused Attach for grant` line straight to standard error about twice
/// the cap's worth: the kept log's thread cuts `broker.err` back under `job_log::CAP`, saying so.
/// NEGATIVE CONTROL: the same burst with nothing kept grows `broker.err` past the cap.
#[test]
fn the_fabric_jobs_log_stays_under_the_bound_through_a_burst_of_refused_attaches() {
    use aterm_uds::job_log::{seam, CAP};
    if let Some((stage, dir)) = seam::stage() {
        let p = job_paths(&dir);
        let outputs = if stage.ends_with("unit") {
            seam::unit_outputs(&p.unit_text())
        } else {
            seam::plist_outputs(&p.plist_text())
        };
        seam::bound_every(std::time::Duration::from_millis(100));
        seam::open_as_service_manager(&outputs).unwrap();
        if stage.starts_with("kept") {
            let kept = keep_broker_log(&p.sock());
            assert_eq!(kept.stdout(), outputs.stdout.is_some());
            assert!(kept.stderr());
        }
        for _ in 0..seam::BURST {
            eprintln!("{REFUSED_ATTACH}");
        }
        if stage.starts_with("kept") {
            assert!(
                seam::until_bounded(&outputs.files()),
                "the kept log stayed past the cap for a minute"
            );
        }
        std::process::exit(0);
    }
    let scratch = crate::test_scratch_dir("job-log");
    for (stage, control) in [("kept", "unkept"), ("kept-unit", "unkept-unit")] {
        let run = |stage: &str| {
            let dir = scratch.path().join(stage);
            std::fs::create_dir(&dir).unwrap();
            aterm_uds::job_log::seam::rerun(FABRIC_BURST, stage, &dir).unwrap();
            std::fs::read_to_string(dir.join("broker.err")).unwrap()
        };
        let kept = run(stage);
        assert!(kept.len() as u64 <= CAP, "{stage}: {} > {CAP}", kept.len());
        // A refusal written while the thread cuts can land before the cut's own line.
        assert!(
            kept.contains("was cut here"),
            "{stage}: the thread's cut says so"
        );
        assert!(run(control).len() as u64 > CAP, "{control} is unbounded");
    }
}

#[test]
fn the_undo_carries_the_environment_that_scoped_the_run() {
    assert_eq!(undo_command("aterm", &[]), "aterm fabric off");
    assert_eq!(
        undo_command(
            "aterm-link",
            &[
                ("ATERM_FABRIC_HOME", Some("/tmp/x/root".to_string())),
                ("XDG_CONFIG_HOME", None),
                ("XDG_RUNTIME_DIR", Some(String::new())),
                ("ATERM_BIN", Some("/x/aterm-link".to_string())),
            ]
        ),
        "ATERM_FABRIC_HOME=/tmp/x/root ATERM_BIN=/x/aterm-link aterm-link fabric off"
    );
    assert_eq!(
        undo_command(
            "aterm",
            &[("ATERM_BIN", Some("/a b/aterm (dev)/aterm".to_string()))]
        ),
        "ATERM_BIN='/a b/aterm (dev)/aterm' aterm fabric off",
        "a value with a space in it is one shell word"
    );
}

#[test]
fn the_config_edit_touches_only_the_command_key() {
    let line = "aterm link serve --fleet local --broker /b.sock";
    let cmd = &words(line);
    let before = "font_px = 12.0\n\n[fabric]\n# a comment\ncommand = \"old\"\npresence = \"meta\"\n\n[keys]\nx = 1\n";
    let (after, changed) = set_fabric_command(before, cmd).expect("ok");
    assert!(changed);
    assert_eq!(
        after,
        format!(
            "font_px = 12.0\n\n[fabric]\ncommand = \"{line}\"\nreceipts = true\n# a comment\npresence = \"meta\"\n\n[keys]\nx = 1\n"
        )
    );
    assert_eq!(fabric::command_in_toml(&after), Ok(Some(cmd.clone())));
    assert_eq!(fabric::receipts_in_toml(&after), Ok(Some(true)));
    assert!(!set_fabric_command(&after, cmd).expect("ok").1);
    let off = format!("[fabric]\ncommand = \"{line}\"\nreceipts = false\n");
    assert!(!set_fabric_command(&off, cmd).expect("ok").1);
    let (after, _) =
        set_fabric_command(&off, &words("aterm link serve --fleet x --broker /y")).expect("ok");
    assert_eq!(fabric::receipts_in_toml(&after), Ok(Some(false)));
    assert_eq!(after.matches("receipts").count(), 1);
    let multi = "[fabric]\ncommand = \"\"\"aterm link serve --fleet x \\\n   --broker /y\"\"\"\nk = 1\n[z]\n";
    let (after, _) = set_fabric_command(multi, cmd).expect("ok");
    assert_eq!(
        after,
        format!("[fabric]\ncommand = \"{line}\"\nreceipts = true\nk = 1\n[z]\n")
    );
    let (after, changed) = set_fabric_command("font_px = 1.0\n", cmd).expect("ok");
    assert!(changed);
    assert!(after.starts_with("font_px = 1.0\n\n[fabric]\n# Written by `aterm fabric on`"));
    assert_eq!(fabric::command_in_toml(&after), Ok(Some(cmd.clone())));
    assert_eq!(fabric::receipts_in_toml(&after), Ok(Some(true)));
    let (after, _) = set_fabric_command("", cmd).expect("ok");
    assert!(after.starts_with("[fabric]\n"));
    assert!(set_fabric_command("[fabric\ncommand = \"x\"\n", cmd).is_err());
    assert!(set_fabric_command("fabric.command = \"x\"\n", cmd).is_err());
    assert!(set_fabric_command("fabric = { command = \"x\" }\n", cmd).is_err());
    let (after, _) = set_fabric_command("[other]\nfabric.command = \"x\"\n", cmd).expect("ok");
    assert!(after.contains("[other]\nfabric.command = \"x\"\n\n[fabric]\n"));

    let (after, removed) = remove_fabric_table(before).expect("ok");
    assert_eq!(removed, ["[fabric]"]);
    assert_eq!(after, "font_px = 12.0\n\n[keys]\nx = 1\n");
    assert!(remove_fabric_table(&after).expect("ok").1.is_empty());
    assert_eq!(
        remove_fabric_table("[fabric]\ncommand = \"x\"\n"),
        Ok((String::new(), vec!["[fabric]".to_string()]))
    );
    let (after, _) = remove_fabric_table("[fabric]\ncommand = \"x\"\n[keys]\nx = 1\n").expect("ok");
    assert_eq!(after, "[keys]\nx = 1\n");
    let subs = "[fabric.presence]\nx = 1\n[a]\ny = 2\n[fabric] # on\ncommand = \"x\"\n[fabric.more]\nz = 3\n[keys]\nk = 1\n";
    let (after, removed) = remove_fabric_table(subs).expect("ok");
    assert_eq!(removed, ["[fabric.presence]", "[fabric]", "[fabric.more]"]);
    assert_eq!(after, "[a]\ny = 2\n[keys]\nk = 1\n");
    assert!(fabric::command_in_toml(&after).expect("valid").is_none());
    assert_eq!(
        remove_fabric_table("[fabric.presence]\nx = 1\n"),
        Ok((String::new(), vec!["[fabric.presence]".to_string()]))
    );
    assert!(remove_fabric_table("[fabrics]\nx = 1\n")
        .expect("ok")
        .1
        .is_empty());
    for broken in [
        "[fabric]\ncommand = \"x\"\n[keys\nbind = \"y\"\n[update]\nenabled = false\n",
        "[fabric]\ncommand = \"x\"\n[[keys]\nbind = \"y\"\n[update]\nenabled = false\n",
    ] {
        assert!(remove_fabric_table(broken).is_err(), "{broken}");
    }
}

#[test]
fn the_config_edit_reads_headers_and_keys_the_way_toml_does() {
    let cmd = &words("aterm link serve --fleet local --broker /b.sock");
    let person = "[update]\nenabled = false\n";
    for table in [
        "[ fabric ]\ncommand = \"old\"\n",
        "[\"fabric\"] # on\ncommand = \"old\"\n",
        "[fabric]\n\"command\" = \"old\"\n",
        "[ 'fabric' . presence ]\nx = 1\n[fabric]\ncommand = \"old\"\n",
    ] {
        let before = format!("{person}\n{table}");
        let (after, changed) = set_fabric_command(&before, cmd).expect(&before);
        assert!(changed);
        assert_eq!(
            fabric::command_in_toml(&after),
            Ok(Some(cmd.clone())),
            "{after}"
        );
        assert!(after.starts_with(person), "{after}");
        let (off, removed) = remove_fabric_table(&after).expect(&after);
        assert!(!removed.is_empty(), "{after}");
        assert_eq!(off, person, "{after}");
    }
    let in_a_string = "note = \"\"\"\n[fabric]\n\"\"\"\n";
    assert!(set_fabric_command(in_a_string, cmd).is_err());
    assert!(remove_fabric_table(in_a_string).is_err());
    assert!(remove_fabric_table("fabric.command = \"x\"\n").is_err());
    assert!(remove_fabric_table("fabric = { command = \"x\" }\n").is_err());
}

/// A `[fabric]` edit whose folder flush fails after its rename is in effect: `on`'s and
/// `off`'s config steps say ok beside a WARNING, not FAILED, and the run goes on; `on`'s says
/// its done line and counts the change. A flush that fails before the edit (its `.bak`'s) still
/// fails the step, aterm.toml unchanged, with no done line and no change counted.
/// NEGATIVE CONTROLS: with every replace error said as FAILED, as before, the step returned
/// false; a done line said only for an `Ok` replace drops the one of the edit in effect, and one
/// said for every replace error says it of the edit that never landed.
#[test]
fn an_edit_in_effect_whose_folder_flush_failed_is_ok_beside_a_warning() {
    use aterm_durable::seam::{FailingDirs, EIO};
    let dir = crate::test_scratch_dir("en");
    let config = dir.path().join("aterm.toml");
    std::fs::write(&config, "font_px = 12.0\n").expect("seed");
    let command =
        |config: &Path| fabric::command_in_toml(&std::fs::read_to_string(config).expect("config"));
    let cmd = &words("aterm link serve --fleet local --broker /b.sock");

    let mut on = Out::kept();
    {
        let _edits = FailingDirs::after(1, EIO);
        assert_eq!(
            config_step(Some(&config), None, cmd, &mut on),
            ConfigSet::Set,
            "the step is done"
        );
    }
    assert!(!on.failed, "nothing FAILED");
    assert_eq!(on.changed, 1, "the edit in effect is a change");
    assert!(
        on.said().contains(&format!(
            "config done add [fabric] command in {} (previous saved as .bak)",
            config.display()
        )),
        "its done line: {}",
        on.said()
    );
    assert_eq!(
        on.warned.get(),
        1,
        "one WARNING: a power loss may take it back"
    );
    assert_eq!(
        command(&config),
        Ok(Some(cmd.clone())),
        "the edit is in effect"
    );

    let mut off = Out::new(false);
    {
        let _edits = FailingDirs::after(1, EIO);
        off_config_step(Some(&config), &mut off);
    }
    assert!(!off.failed);
    assert_eq!(off.warned.get(), 1);
    assert_eq!(command(&config), Ok(None), "the removal is in effect");

    let mut early = Out::kept();
    {
        let _baks = FailingDirs::with(EIO);
        assert_eq!(
            config_step(Some(&config), None, cmd, &mut early),
            ConfigSet::Failed
        );
    }
    assert!(early.failed, "the .bak's flush failed before any edit");
    assert_eq!(early.warned.get(), 0);
    assert_eq!(early.changed, 0, "no edit landed");
    assert!(
        !early.said().contains("config done "),
        "no done line: {}",
        early.said()
    );
    assert_eq!(command(&config), Ok(None), "aterm.toml unchanged");
}

#[test]
fn the_config_is_written_through_its_link_under_the_service_lock() {
    let dir = crate::test_scratch_dir("en");
    let dotfiles = dir.path().join("dotfiles");
    let cfg = dir.path().join("cfg/aterm");
    std::fs::create_dir_all(&dotfiles).expect("dotfiles");
    std::fs::create_dir_all(&cfg).expect("cfg");
    let target = dotfiles.join("aterm.toml");
    std::fs::write(&target, "font_px = 12.0\n").expect("seed");
    let config = cfg.join("aterm.toml");
    std::os::unix::fs::symlink("../../dotfiles/aterm.toml", &config).expect("link");
    let cmd = &words("aterm link serve --fleet local --broker /b.sock");

    assert_eq!(
        config_step(Some(&config), None, cmd, &mut Out::new(false)),
        ConfigSet::Set
    );
    let linked = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_symlink());
    assert!(linked(&config), "the link was replaced by a file");
    let text = std::fs::read_to_string(&target).expect("target");
    assert_eq!(fabric::command_in_toml(&text), Ok(Some(cmd.clone())));
    assert_eq!(
        std::fs::read_to_string(dotfiles.join("aterm.toml.bak")).expect("bak"),
        "font_px = 12.0\n"
    );

    let lock = dotfiles.join(".aterm.toml.aterm-write.lock");
    let file = ConfigFile::open(&config).expect("open");
    let edit = file.lock().expect("lock");
    assert_eq!(
        aterm_uds::ownerlock::probe(&lock),
        aterm_uds::ownerlock::Owner::Live
    );
    std::fs::write(&target, "font_px = 18.0\n").expect("the other writer");
    edit.replace("font_px = 9.0\n")
        .expect_err("changed under it");
    assert_eq!(
        std::fs::read_to_string(&target).expect("target"),
        "font_px = 18.0\n"
    );
    assert_eq!(
        std::fs::read_to_string(dotfiles.join("aterm.toml.bak")).expect("bak"),
        "font_px = 12.0\n"
    );
    assert_eq!(
        aterm_uds::ownerlock::probe(&lock),
        aterm_uds::ownerlock::Owner::Dead
    );
    assert!(linked(&config));
    let entries = std::fs::read_dir(&dotfiles).expect("list").count();
    assert_eq!(
        entries, 3,
        "a temp was left behind: aterm.toml, its .bak, the lock"
    );
}

#[test]
fn a_no_op_through_a_link_into_a_read_only_dir_reads_already() {
    use std::os::unix::fs::PermissionsExt;
    let dir = crate::test_scratch_dir("en");
    let store = dir.path().join("store");
    std::fs::create_dir_all(&store).expect("store");
    let cmd = &words("aterm link serve --fleet local --broker /b.sock");
    let (configured, _) = set_fabric_command("font_px = 12.0\n", cmd).expect("ok");
    let link = |name: &str, text: &str| {
        let file = store.join(format!("{name}-aterm.toml"));
        std::fs::write(&file, text).expect("seed");
        let cfg = dir.path().join(name).join("aterm");
        std::fs::create_dir_all(&cfg).expect("cfg");
        std::os::unix::fs::symlink(&file, cfg.join("aterm.toml")).expect("link");
        cfg.join("aterm.toml")
    };
    let (on_config, off_config) = (link("on", &configured), link("off", "font_px = 12.0\n"));
    let chmod = |mode| {
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(mode)).expect("chmod");
    };
    chmod(0o555);
    let (mut on, mut off) = (Out::new(false), Out::new(false));
    let on_ran = config_step(Some(&on_config), None, cmd, &mut on) == ConfigSet::Set;
    off_config_step(Some(&off_config), &mut off);
    chmod(0o755);
    assert!(
        on_ran && !on.failed && on.changed == 0,
        "on did not read `already`"
    );
    assert!(
        !off.failed && off.changed == 0,
        "off did not read `already`"
    );
    assert_eq!(
        std::fs::read_dir(&store).expect("list").count(),
        2,
        "a lock or a temp was left in the store"
    );
}

#[test]
fn the_rendezvous_file_round_trips() {
    let r = Rendezvous {
        fleet: "local".to_string(),
        broker: "/tmp/x/bus.sock".to_string(),
        node: "n-0123456789abcdef".to_string(),
        cap_file: "/tmp/x/node.cap".to_string(),
        state: "/tmp/x/link-state".to_string(),
        command: words("/x/aterm link serve --fleet local --broker /tmp/x/bus.sock"),
    };
    let text = r.render();
    assert!(
        text.contains("command = \"/x/aterm link serve --fleet local --broker /tmp/x/bus.sock\"\n"),
        "a plain argv is written as the line it always was:\n{text}"
    );
    assert_eq!(Rendezvous::parse(&text), Ok(r.clone()));
    assert!(
        Rendezvous::parse(&text.replace("node = \"n-0123456789abcdef\"", "node = 1")).is_err(),
        "a field of the wrong type"
    );
    let odd = Rendezvous {
        command: words("a \"quoted\" \\ back"),
        ..r.clone()
    };
    assert_eq!(Rendezvous::parse(&odd.render()), Ok(odd));
    let text = r.render().replace(
        "command = \"/x/aterm link serve --fleet local --broker /tmp/x/bus.sock\"",
        "command = [\"/a b/aterm\", \"link\", \"serve\"]",
    );
    assert_eq!(
        Rendezvous::parse(&text).map(|r| r.command),
        Ok(vec![
            "/a b/aterm".to_string(),
            "link".to_string(),
            "serve".to_string()
        ])
    );
    assert!(Rendezvous::parse("fleet = 1\n").is_err());
    assert!(Rendezvous::parse("[x\n").is_err());
}

/// THE RINGS ATERM MINTS ARE ASTREAM'S CEILINGS' RULES. The historical node ring is frozen,
/// including its two retired drive faces and excluding later halt grants. A person's
/// ring is byte for byte the one `enable::person_grants` minted before it was deleted (its
/// output for a human and a service, spelled here), so a ring `mint-for` minted then is the
/// one it mints now. Every node id `on` makes is one the node ceiling takes.
#[test]
fn the_rings_are_the_ceilings_rules() {
    let ceiling = astream_cap::GrantCeiling::aterm_node_ring("local", "n-a").expect("a node");
    assert_eq!(
        ceiling.rules(),
        [
            "rw,p=n-a:/f/local/pub/n-a/>",
            "ro:/f/local/pub/>",
            "ro:/f/local/fleet/>",
            "rw,p=n-a:/f/local/in/*/*/n-a/*",
            "ro:/f/local/in/n-a/>",
            "rw,p=n-a:/f/local/cur/n-a/>",
            "ro:/f/local/halt/>",
            "rw,p=n-a:/f/local/halt/n-a",
        ]
    );
    assert_eq!(
        before_halt_grants(&ceiling, "local", "n-a"),
        ceiling.rules()[..6]
    );
    assert_eq!(
        earlier_node_grants("local", "n-a")[6..],
        [
            "ro:/f/local/term/n-a/>",
            "rw,p=n-a:/f/local/term/n-a/*/screen",
        ]
    );
    assert_eq!(
        node_rules("lab", "n-b"),
        ceiling
            .rules()
            .iter()
            .map(|x| x.replace("/local/", "/lab/").replace("n-a", "n-b"))
            .collect::<Vec<_>>()
    );
    // `person_grants`'s output, as it was deleted.
    let human = [
        "rw,p=h-andrew:/f/lab/fleet/h-andrew/>",
        "rw,p=h-andrew:/f/lab/in/*/*/h-andrew/*",
        "rw,p=h-andrew:/f/lab/pub/p/h-andrew/>",
        "ro:/f/lab/in/p/h-andrew/>",
        "rw,p=h-andrew:/f/lab/cur/p/h-andrew/>",
        "ro:/f/lab/pub/>",
    ];
    let service = [
        "rw,p=a-bot:/f/lab/in/*/*/a-bot/*",
        "rw,p=a-bot:/f/lab/pub/p/a-bot/>",
        "ro:/f/lab/in/p/a-bot/>",
        "rw,p=a-bot:/f/lab/cur/p/a-bot/>",
        "ro:/f/lab/pub/>",
    ];
    assert_eq!(person_rules("lab", "h-andrew"), human);
    assert_eq!(person_rules("lab", "a-bot"), service);
    let minted = aterm_uds::rand::hex_token::<8>().expect("entropy");
    for node in [
        format!("n-{minted}"),
        "n-0123456789abcdef".to_string(),
        "n-m7".to_string(),
        format!("n-{}", "z".repeat(crate::subject::PRINCIPAL_MAX - 2)),
    ] {
        assert!(is_node_id(&node), "{node}");
        for fleet in ["local", "lab", "a", &"f".repeat(32)] {
            assert!(
                crate::subject::is_fleet(fleet)
                    && astream_cap::GrantCeiling::aterm_node_ring(fleet, &node).is_ok(),
                "{fleet} {node}"
            );
        }
    }
}

/// A RING MINTED FROM A CEILING'S RULES IS WITHIN IT: a node's, a human's and a service's,
/// minted grant for grant and checked by astream's `check_ring` and by `on`'s own reading.
#[test]
fn a_ring_minted_from_its_rules_passes_the_ceiling_check() {
    let secret = [7u8; 32];
    let minted = |grants: &[String]| -> Vec<astream_cap::Capability> {
        grants
            .iter()
            .map(|g| astream_cap::mint(&secret, g).expect("mint"))
            .collect()
    };
    let node = "n-0123456789abcdef";
    let ceiling = astream_cap::GrantCeiling::aterm_node_ring("lab", node).expect("a node");
    let held = minted(&ceiling.rules());
    assert!(ceiling.check_ring(&held).is_ok());
    assert!(matches!(fit(&held, "lab", node), Ok(Fit::Within)));
    // A ring minted before the halt grants is within the ceiling too, and `on` mints over it.
    let before = minted(&before_halt_grants(&ceiling, "lab", node));
    assert!(ceiling.check_ring(&before).is_ok());
    assert!(matches!(fit(&before, "lab", node), Ok(Fit::BeforeHalts)));
    assert!(minted_by_on(&before, node));
    for person in ["h-andrew", "a-bot"] {
        let ceiling =
            astream_cap::GrantCeiling::aterm_person_ring("lab", person).expect("a person");
        assert!(
            ceiling
                .check_authenticated_ring(&secret, &minted(&ceiling.rules()), 0)
                .is_ok(),
            "{person}"
        );
    }
}

#[test]
fn a_word_no_supervisor_job_can_carry_is_refused() {
    for ok in [
        "/Users//u/.local/share/aterm-fabric",
        "/Users//u/Caf\u{e9}",
        "/x/aterm-link",
    ] {
        assert!(argv_safe(ok).is_ok(), "{ok}");
    }
    for (bad, why) in [
        ("/Users//u/Applications/aterm (dev).app/aterm", "whitespace"),
        ("/a\tb", "whitespace"),
        ("/nb\u{a0}sp", "whitespace"),
        ("/Users//Bob s", "whitespace"),
        ("/Users//Bob's", "a `'`"),
        ("/Users//u/fab\"ric", "a `\"`"),
        ("/Users//u/fab\\ric", "a `\\`"),
        ("/s/100%41", "a `%`"),
        ("/s/$HOME", "a `$`"),
        ("/a\u{7}b", "a control character"),
    ] {
        let err = argv_safe(bad).expect_err(bad);
        assert!(err.contains(why), "{bad:?}: {err}");
        let fixed: String = bad
            .chars()
            .filter(|&c| !(c.is_whitespace() || c.is_control() || "'\"\\%$".contains(c)))
            .collect();
        assert!(argv_safe(&fixed).is_ok(), "{fixed:?}");
    }
}

#[test]
fn an_aterm_path_that_is_not_plain_runs_through_a_link_in_the_root() {
    let p = |aterm: &str| Paths {
        home: PathBuf::from("/Users//u"),
        login_home: Some(PathBuf::from("/Users//u")),
        root: PathBuf::from("/Users//u/.local/share/aterm-fabric"),
        aterm: aterm.to_string(),
        fleet: "local".to_string(),
        config: None,
        rendezvous: None,
        durability: None,
    };
    let link = "/Users//u/.local/share/aterm-fabric/bin/aterm";
    for aterm in [
        "/Users//u/Applications/aterm (dev).app/Contents/MacOS/aterm",
        "/Users//Bob's apps/aterm",
        "/Users//u/100%/aterm",
    ] {
        let p = p(aterm);
        assert_eq!(p.program(), link, "{aterm}");
        assert_eq!(p.program_link(), PathBuf::from(link));
        let bridge = p.bridge_command("n-a");
        let broker = p.broker_argv();
        assert_eq!(bridge[0], link);
        assert_eq!(broker[0], link);
        for word in bridge.iter().chain(&broker) {
            assert_eq!(argv_safe(word), Ok(()), "{aterm}: {word}");
        }
        assert!(aterm_types::fabric_command::line_spells(&bridge));
        let unit = p.unit_text();
        assert_eq!(
            unit_argv(&unit).as_deref(),
            Some(broker.as_slice()),
            "{unit}"
        );
        assert!(!unit.contains(aterm) && !p.plist_text().contains(aterm));
    }
    let shim = p("/x y/target/debug/aterm-link");
    assert!(shim.is_link_shim());
    assert_eq!(
        shim.program(),
        "/Users//u/.local/share/aterm-fabric/bin/aterm-link"
    );
    assert_eq!(shim.bridge_command("n-a")[1], "serve");
    let plain = p("/Users//u/.local/bin/aterm");
    assert_eq!(plain.program(), "/Users//u/.local/bin/aterm");
    assert_eq!(plain.broker_argv()[0], "/Users//u/.local/bin/aterm");
}

/// `Paths::resolve` names an `aterm` other than this process for the broker where the seam is
/// not blank or `$PATH` holds an executable `aterm`: a blank seam, a file that is not
/// executable, a folder named `aterm`, a `$PATH` that does not hold it, and no `$PATH` name
/// none (`converge` then turns the fabric on only from the installed app).
/// NEGATIVE CONTROLS: a read that takes a blank seam for one names a program for any build; one
/// that takes any `aterm` entry for a program names one a `$PATH` cannot run.
#[test]
fn an_aterm_is_named_on_path_only_where_one_runs_there() {
    use std::ffi::OsStr;
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = crate::test_scratch_dir("en");
    let dir = scratch.path();
    let (runs, plain, folder, none) = (
        dir.join("runs"),
        dir.join("plain"),
        dir.join("folder"),
        dir.join("none"),
    );
    for d in [&runs, &plain, &folder, &none] {
        std::fs::create_dir(d).expect("scratch");
    }
    for (d, mode) in [(&runs, 0o755), (&plain, 0o644)] {
        std::fs::write(d.join("aterm"), "#!/bin/sh\n").expect("an aterm");
        std::fs::set_permissions(d.join("aterm"), std::fs::Permissions::from_mode(mode))
            .expect("chmod");
    }
    std::fs::create_dir(folder.join("aterm")).expect("a folder named aterm");
    let path = |dirs: &[&PathBuf]| std::env::join_paths(dirs).expect("a $PATH");

    assert!(names_aterm_on(None, Some(&path(&[&none, &runs]))));
    assert!(names_aterm_on(Some(OsStr::new("/x/aterm")), None));
    assert!(!names_aterm_on(
        None,
        Some(&path(&[&none, &plain, &folder]))
    ));
    assert!(!names_aterm_on(None, None));
    for blank in ["", "  "] {
        assert!(
            !names_aterm_on(Some(OsStr::new(blank)), Some(&path(&[&none]))),
            "{blank:?}"
        );
    }
}

#[test]
fn the_program_step_links_a_path_that_is_not_plain_and_replaces_no_file() {
    let scratch = crate::test_scratch_dir("en");
    let dir = scratch.path();
    let spaced = dir.join("a b");
    std::fs::create_dir_all(&spaced).expect("scratch");
    let aterm = spaced.join("aterm");
    std::fs::write(&aterm, "#!/bin/sh\necho linked \"$@\"\n").expect("a program");
    std::fs::set_permissions(&aterm, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");
    let p = Paths {
        home: dir.to_path_buf(),
        login_home: None,
        root: dir.join("root"),
        aterm: aterm.to_string_lossy().into_owned(),
        fleet: "local".to_string(),
        config: None,
        rendezvous: None,
        durability: None,
    };
    std::fs::create_dir(&p.root).expect("the root step precedes the program step");
    let link = p.program_link();
    assert!(program_step(&p, &mut Out::new(true)));
    assert!(std::fs::symlink_metadata(&link).is_err());
    let mut out = Out::new(false);
    assert!(program_step(&p, &mut out));
    assert_eq!(out.changed, 1);
    assert_eq!(std::fs::read_link(&link).expect("a link"), aterm);
    assert_eq!(
        std::fs::read_to_string(&link).expect("read the linked file"),
        "#!/bin/sh\necho linked \"$@\"\n"
    );
    let mut again = Out::new(false);
    assert!(program_step(&p, &mut again));
    assert_eq!(again.changed, 0, "already");
    std::fs::remove_file(&link).expect("unlink");
    std::os::unix::fs::symlink("/elsewhere", &link).expect("a stale link");
    assert!(program_step(&p, &mut Out::new(false)));
    assert_eq!(std::fs::read_link(&link).expect("a link"), aterm);
    std::fs::remove_file(&link).expect("unlink");
    std::fs::write(&link, "mine").expect("a file");
    let mut refused = Out::new(false);
    assert!(!program_step(&p, &mut refused));
    assert!(refused.failed);
    assert_eq!(std::fs::read_to_string(&link).expect("kept"), "mine");
    std::fs::remove_file(&link).expect("unlink");
    let bin = link.parent().expect("bin");
    std::fs::remove_dir(bin).expect("empty physical bin");
    let outside = dir.join("outside");
    std::fs::create_dir(&outside).expect("owned external directory");
    std::fs::set_permissions(
        &outside,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("external mode");
    let marker = outside.join("mine");
    std::fs::write(&marker, "external contents").expect("external contents");
    let external_link = outside.join("aterm");
    std::os::unix::fs::symlink(&aterm, &external_link).expect("matching external link");
    std::os::unix::fs::symlink(&outside, bin).expect("aliased bin");
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(&outside)
            .expect("external metadata")
            .permissions(),
    );
    let mut matching = Out::new(false);
    let matching_refused = !program_step(&p, &mut matching);
    let mut dry = Out::new(true);
    let dry_refused = !program_step(&p, &mut dry);
    std::fs::remove_file(&external_link).expect("replace owned external link");
    let other = std::path::Path::new("/not-the-selected-program");
    std::os::unix::fs::symlink(other, &external_link).expect("different external link");
    let mut modifying = Out::new(false);
    let modifying_refused = !program_step(&p, &mut modifying);
    assert!(matching_refused && matching.failed && dry_refused && dry.failed);
    assert!(modifying_refused && modifying.failed);
    assert_eq!(std::fs::read_link(bin).expect("bin kept"), outside);
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&outside)
                .expect("external metadata")
                .permissions(),
        ),
        mode,
        "the external directory's mode is untouched"
    );
    assert_eq!(
        std::fs::read_to_string(&marker).expect("external file kept"),
        "external contents"
    );
    assert_eq!(
        std::fs::read_link(&external_link).expect("external link kept"),
        other
    );
    assert_eq!(
        std::fs::read_dir(&outside)
            .expect("external listing")
            .count(),
        2
    );
    std::fs::remove_file(bin).expect("remove our bin alias");
    std::fs::create_dir(bin).expect("restore physical bin");
    let plain = Paths {
        aterm: "/usr/bin/true".to_string(),
        ..p
    };
    let mut nothing = Out::new(false);
    assert!(program_step(&plain, &mut nothing));
    assert_eq!(nothing.changed, 0);
    assert_eq!(
        std::fs::read_dir(link.parent().expect("bin"))
            .expect("bin")
            .count(),
        0,
        "nothing linked"
    );
}

#[test]
fn a_spaced_word_is_never_written_as_an_array() {
    let dev = "/Users//u/Applications/aterm (dev).app/Contents/MacOS/aterm";
    let mut argv = vec![dev.to_string()];
    argv.extend(words("link serve --fleet local --broker /b.sock"));
    let err = set_fabric_command("font_px = 12.0\n", &argv).expect_err("refused");
    assert!(err.contains("whitespace"), "{err}");
    let line = format!("[fabric]\ncommand = \"{}\"\n", argv.join(" "));
    let read = fabric::command_in_toml(&line).expect("ok").expect("some");
    assert_ne!(read, argv, "the line splits the spaced path");
    argv[0] = "/r/bin/aterm".to_string();
    let (after, changed) = set_fabric_command("font_px = 12.0\n", &argv).expect("ok");
    assert!(changed);
    assert!(
        after.contains("command = \"/r/bin/aterm link serve --fleet local --broker /b.sock\"\n"),
        "{after}"
    );
    assert_eq!(fabric::command_in_toml(&after), Ok(Some(argv.clone())));
    let plain = words("/usr/bin/aterm link serve --fleet local --broker /b.sock");
    let hand = "[fabric]\ncommand = [\n  \"/a b/aterm\", # the program\n  \"link\", 'serve]',\n]\nreceipts = false\n[keys]\nx = 1\n";
    let (back, changed) = set_fabric_command(hand, &plain).expect("ok");
    assert!(changed);
    assert_eq!(
        back,
        "[fabric]\ncommand = \"/usr/bin/aterm link serve --fleet local --broker /b.sock\"\nreceipts = false\n[keys]\nx = 1\n"
    );
    let same = format!(
        "[fabric]\ncommand = [{}]\nreceipts = true\n",
        plain
            .iter()
            .map(|w| format!("\"{w}\""))
            .collect::<Vec<_>>()
            .join(", ")
    );
    assert!(!set_fabric_command(&same, &plain).expect("ok").1);
    let open = "[fabric]\ncommand = [\"aterm\",\n";
    assert!(set_fabric_command(open, &plain).is_err());
}

/// One of every [`fabric::WarningKind`] shape a remedy branches on: each variant, each
/// `BridgeLinkDown` reason, and each broker line (`last_word`) [`quoted_broker_fix`] tells
/// apart. Every variant is named once in the exhaustive match below, without a wildcard, so
/// a new one does not compile until it gets an index there and a shape here.
fn every_warning_shape() -> Vec<fabric::WarningKind> {
    use fabric::WarningKind as W;
    use std::io::ErrorKind as K;
    let refusal = |kind| crate::cli::log_refusal_remedy(kind, "/r/bus.log").expect("a remedy");
    let poisoned = |restart| {
        crate::broker_health::poisoned_line(
            "/r/bus.log",
            "a flush failed (EIO) with records no full flush covers",
            restart,
        )
    };
    let lines: Vec<Option<String>> = [
        None,
        Some("open bus.log: refused".to_string()),
        Some(refusal(K::InvalidData)),
        Some(refusal(K::Unsupported)),
        Some(poisoned(None)),
        Some(poisoned(Some("aterm link broker /r/bus.sock /r/bus.log"))),
        Some(crate::at_rest::UNFINISHED_RETIRE_REMEDY.to_string()),
        Some(crate::at_rest::SEALED_LOG_REMEDY.to_string()),
    ]
    .into_iter()
    .map(|line| line.map(|l| format!("written 3m ago: aterm link broker: {l}")))
    .collect();
    let mut shapes: Vec<W> = lines
        .iter()
        .map(|last_word| W::BrokerUnreachable {
            last_word: last_word.clone(),
        })
        .collect();
    for reason in [
        "no-socket",
        "refused",
        "no-ack",
        "expired",
        "poisoned",
        "attach",
        "busy",
        "record",
        "denied",
        "cut-say",
        "protocol",
        "endpoint-grammar",
        "other",
    ] {
        let quoted = if matches!(reason, "no-socket" | "refused") {
            lines.clone()
        } else {
            vec![None]
        };
        shapes.extend(quoted.into_iter().map(|last_word| W::BridgeLinkDown {
            reason: reason.to_string(),
            last_word,
        }));
    }
    shapes.extend([
        W::ConnectedProbeMismatch,
        W::BrokerCapLapsed,
        W::BridgeStarting,
        W::BusUnreadable,
        W::BrokerProtocolSkew,
        W::BrokerReplacedBuild,
        W::BrokerUnguarded,
        W::NoFabricCommand,
        W::NoNodeId,
        W::CapUnreadable {
            owner_or_link_refused: false,
        },
        W::CapUnreadable {
            owner_or_link_refused: true,
        },
        W::CapWiderRing,
        W::InstanceNoBridge,
        W::BridgeDown,
        W::BridgeDifferentCommand,
        W::BridgeStateLocked,
        W::ConnectedPresenceMismatch,
        W::BridgeStale,
        W::OverdueWork { to_person: false },
        W::OverdueWork { to_person: true },
        W::FleetHaltStanding,
        W::SessionQueryFailed {
            pid: 42,
            sock: None,
        },
        W::SessionQueryFailed {
            pid: 42,
            sock: Some("/run/a b.sock".to_string()),
        },
        W::SessionHeld {
            sid: "ta000000000".to_string(),
            pid: 1,
            sock: None,
            origin: Some("local".to_string()),
            fabric_lost: false,
        },
        W::SessionHeld {
            sid: "ta000000000".to_string(),
            pid: 1,
            sock: None,
            origin: Some("person,remote,fleet,local".to_string()),
            fabric_lost: true,
        },
        W::SessionHeld {
            sid: "ta000000000".to_string(),
            pid: 1,
            sock: None,
            origin: None,
            fabric_lost: false,
        },
        W::SessionInboxEvicted,
        W::RetentionGaveUp,
        W::SessionUnhandledStaleWork,
        W::OverdueUnjudged,
        W::SessionQueuedPostsNoBridge,
        W::InstanceUnreachable,
        W::InstanceDiscoveryFailed,
        W::GhostSessionRow,
        W::RendezvousMissing,
        W::RendezvousUnreadable,
        W::BridgeBuildMismatch,
        W::BridgeLaneFaults,
        W::SubjectBudgetHigh,
        W::RosterTruncated,
        W::BrokerLogPoisoned,
        W::BrokerFlushFailed,
    ]);
    let variants = 39;
    let index = |w: &W| match w {
        W::NoFabricCommand => 0,
        W::NoNodeId => 1,
        W::CapUnreadable { .. } => 2,
        W::CapWiderRing => 3,
        W::BrokerProtocolSkew => 4,
        W::BrokerUnreachable { .. } => 5,
        W::BrokerCapLapsed => 6,
        W::BusUnreadable => 7,
        W::BrokerReplacedBuild => 8,
        W::BrokerUnguarded => 9,
        W::BrokerLogPoisoned => 10,
        W::BrokerFlushFailed => 11,
        W::InstanceDiscoveryFailed => 12,
        W::InstanceUnreachable => 13,
        W::InstanceNoBridge => 14,
        W::BridgeDown => 15,
        W::BridgeStateLocked => 16,
        W::BridgeStarting => 17,
        W::BridgeLinkDown { .. } => 18,
        W::BridgeStale => 19,
        W::ConnectedProbeMismatch => 20,
        W::BridgeBuildMismatch => 21,
        W::BridgeLaneFaults => 22,
        W::RetentionGaveUp => 23,
        W::BridgeDifferentCommand => 24,
        W::ConnectedPresenceMismatch => 25,
        W::SessionQueryFailed { .. } => 26,
        W::SessionInboxEvicted => 27,
        W::SessionHeld { .. } => 28,
        W::SessionUnhandledStaleWork => 29,
        W::SessionQueuedPostsNoBridge => 30,
        W::OverdueWork { .. } => 31,
        W::OverdueUnjudged => 32,
        W::GhostSessionRow => 33,
        W::FleetHaltStanding => 34,
        W::RosterTruncated => 35,
        W::SubjectBudgetHigh => 36,
        W::RendezvousMissing => 37,
        W::RendezvousUnreadable => 38,
    };
    let named: BTreeSet<usize> = shapes.iter().map(index).collect();
    assert_eq!(
        named,
        (0..variants).collect::<BTreeSet<_>>(),
        "every WarningKind variant has a shape"
    );
    shapes
}

/// Every [`fabric::WarningKind`] maps to an actionable remedy under every [`Role`] (each
/// fleet, each [`Keeper`]), and every `aterm fabric on` command in that remedy carries the
/// role's `--fleet`.
/// NEGATIVE CONTROL: `SessionQueryFailed` and `RendezvousUnreadable` (which prose substring
/// matching previously sent to the generic help fallback or misclassified as `BridgeLaneFaults`)
/// receive their own remedies.
#[test]
fn every_warning_shape_has_a_fix() {
    use fabric::WarningKind as W;
    let roles: Vec<Role> = [Keeper::Job, Keeper::Node]
        .into_iter()
        .flat_map(|keeper| {
            [None, Some("lab".to_string())]
                .into_iter()
                .map(move |fleet| Role { fleet, keeper })
        })
        .collect();
    for w in every_warning_shape() {
        for role in &roles {
            let fix = fix_for(&w, role);
            assert!(!fix.is_empty(), "empty fix for {w:?}");
            assert_ne!(fix, "see `aterm help fabric`", "no fix for: {w:?}");
            if role.fleet.is_some() {
                assert_eq!(
                    fix.matches("`aterm fabric on").count(),
                    fix.matches("`aterm fabric on --fleet lab`").count(),
                    "{w:?}: {fix}"
                );
            }
        }
    }
    let session_err = fix_for(
        &W::SessionQueryFailed {
            pid: 42,
            sock: None,
        },
        &Role::default(),
    );
    assert!(
        session_err.contains("`aterm ctl --pid 42 ls`"),
        "{session_err}"
    );
    assert_ne!(
        session_err,
        fix_for(&W::BridgeLaneFaults, &Role::default()),
        "NEGATIVE CONTROL: a failed session ctl query must not misclassify as BridgeLaneFaults"
    );
    let rv_unreadable = fix_for(&W::RendezvousUnreadable, &Role::default());
    assert!(
        rv_unreadable.contains("rewrites the rendezvous file"),
        "{rv_unreadable}"
    );
}

/// Under the node service ([`Keeper::Node`]) no remedy, for any warning shape, any fleet and
/// any quoted broker line, names a broker job or its supervisor, launchd or systemd
/// restarting a broker, `broker.err`, a broker's `--secret-file`, a rewrite, start or restart
/// of a broker, or a `kill` of anything but a bridge: the process serving bus.log is the node
/// service, whose kill ends everything it hosts. What it does name is what the person can do
/// now. NEGATIVE CONTROL: under [`Keeper::Job`] each of those phrases, and a `kill` of a
/// broker, is said by some remedy, so the sweep reads remedies that can say them.
#[test]
fn no_remedy_names_a_broker_job_where_the_node_service_hosts_bus_log() {
    use fabric::WarningKind as W;
    const JOB_ONLY: [&str; 9] = [
        "job",
        "supervisor",
        "launchd",
        "systemd",
        "broker.err",
        "--secret-file",
        "rewrites the broker",
        "restarts the broker",
        "start the broker",
    ];
    fn kills_more_than_a_bridge(fix: &str) -> bool {
        fix.match_indices("kill").any(|(at, _)| {
            !fix[at..]
                .chars()
                .take(40)
                .collect::<String>()
                .contains("bridge")
        })
    }
    let shapes = every_warning_shape();
    let fixes = |keeper: Keeper| -> Vec<(String, String)> {
        shapes
            .iter()
            .flat_map(|w| {
                [None, Some("lab")].map(|fleet| {
                    let role = Role {
                        fleet: fleet.map(str::to_string),
                        keeper,
                    };
                    (format!("{w:?}"), fix_for(w, &role))
                })
            })
            .collect()
    };
    for (w, fix) in fixes(Keeper::Node) {
        for phrase in JOB_ONLY {
            assert!(
                !fix.contains(phrase),
                "{w}: the node service's remedy names `{phrase}`: {fix}"
            );
        }
        assert!(!kills_more_than_a_bridge(&fix), "{w}: {fix}");
    }
    let job = fixes(Keeper::Job);
    for phrase in JOB_ONLY {
        assert!(
            job.iter().any(|(_, fix)| fix.contains(phrase)),
            "NEGATIVE CONTROL: no broker's remedy says `{phrase}`"
        );
    }
    assert!(
        job.iter().any(|(_, fix)| kills_more_than_a_bridge(fix)),
        "NEGATIVE CONTROL: a broker's remedy kills the broker"
    );

    let node = Role {
        fleet: None,
        keeper: Keeper::Node,
    };
    let poisoned = fix_for(&W::BrokerLogPoisoned, &node);
    assert!(
        poisoned.contains("which the node service does itself at its next pass"),
        "{poisoned}"
    );
    let down = fix_for(&W::BrokerUnreachable { last_word: None }, &node);
    assert!(
        down.contains("`aterm node status`")
            && down.contains("`aterm node on` starts it")
            && down.contains("`aterm fabric off`, then `aterm link broker --repair")
            && down.contains("then `aterm fabric on`"),
        "{down}"
    );
    assert_eq!(
        fix_for(
            &W::BridgeLinkDown {
                reason: "refused".to_string(),
                last_word: Some("written 1s ago: aterm link broker: refused".to_string()),
            },
            &node
        ),
        format!("{down}; the bridge then reconnects on its own"),
        "a retired job's line is no reason of the node service's"
    );
}

#[test]
fn a_refused_log_gets_its_own_remedy_and_a_newer_one_never_a_repair() {
    use std::io::ErrorKind as K;
    let warning = |last: &str| fabric::WarningKind::BrokerUnreachable {
        last_word: Some(format!("written 1s ago: aterm link broker: {last}")),
    };
    let bridge = |reason: &str, last: Option<&str>| fabric::WarningKind::BridgeLinkDown {
        reason: reason.to_string(),
        last_word: last.map(|l| format!("written 1s ago: aterm link broker: {l}")),
    };
    let reconnects = "; the bridge then reconnects on its own";
    let line = |kind| crate::cli::log_refusal_remedy(kind, "/r/bus.log").expect("a remedy");
    let role = Role::default();
    let corrupt = fix_for(&warning(&line(K::InvalidData)), &role);
    assert!(corrupt.contains("`aterm link broker --repair"), "{corrupt}");
    // Its real length is `fabric::tests::a_sealed_logs_refusal_gets_its_remedy_at_real_lengths`.
    let sealed = fix_for(&warning(crate::at_rest::SEALED_LOG_REMEDY), &role);
    assert!(sealed.contains("--retire-log"), "{sealed}");
    assert_ne!(sealed, corrupt);
    let newer = fix_for(&warning(&line(K::Unsupported)), &role);
    assert!(!newer.contains("--repair"), "{newer}");
    assert_ne!(newer, corrupt);
    let other = fix_for(&warning("open /r/bus.log: address in use"), &role);
    assert!(other.contains("fix that first"), "{other}");
    for reason in ["no-socket", "refused"] {
        let from = |last: Option<&str>| fix_for(&bridge(reason, last), &role);
        assert_eq!(
            from(Some(&line(K::InvalidData))),
            format!("{corrupt}{reconnects}")
        );
        assert_eq!(
            from(Some(&line(K::Unsupported))),
            format!("{newer}{reconnects}")
        );
        assert_eq!(
            from(Some("open /r/bus.log: address in use")),
            format!("{other}{reconnects}")
        );
        let bare = from(None);
        assert!(
            bare.starts_with("nothing listens at the broker's socket")
                && bare.contains("starts it")
                && !bare.contains("--repair"),
            "{bare}"
        );
    }
    assert_eq!(
        crate::cli::log_refusal_remedy(K::AlreadyExists, "/r/bus.log"),
        None
    );
}

#[test]
fn a_poisoned_log_is_answered_by_the_brokers_restart() {
    let role = Role::default();
    let quoted = |last: &str| {
        fix_for(
            &fabric::WarningKind::BrokerUnreachable {
                last_word: Some(format!("written 1s ago: {last}")),
            },
            &role,
        )
    };
    let reason = "a flush failed (EIO) with records no full flush covers";
    for restart in [None, Some("aterm link broker /r/bus.sock /r/bus.log")] {
        let exited = quoted(&crate::broker_health::poisoned_line(
            "/r/bus.log",
            reason,
            restart,
        ));
        assert!(
            exited.starts_with("the broker exited over a poisoned log")
                && !exited.contains("--repair")
                && !exited.contains("fix that first"),
            "{exited}"
        );
        if restart.is_some() {
            assert!(
                exited.contains("the command its line names")
                    && !exited.contains("`aterm fabric on` starts"),
                "{exited}"
            );
        } else {
            assert!(
                exited.contains("`aterm fabric on` starts it again")
                    && !exited.contains("the command its line names"),
                "{exited}"
            );
        }
    }
    assert!(quoted("aterm link broker: open /r/bus.log: address in use").contains("fix that first"));
    let down = |reason: &str| {
        fix_for(
            &fabric::WarningKind::BridgeLinkDown {
                reason: reason.to_string(),
                last_word: None,
            },
            &role,
        )
    };
    let poisoned = down("poisoned");
    assert!(
        poisoned.starts_with("the broker's log is poisoned")
            && !poisoned.contains("node.cap")
            && !poisoned.contains("`aterm fabric on`"),
        "{poisoned}"
    );
    assert!(
        down("attach").contains("node.cap"),
        "the refused cap keeps its fix"
    );
}

#[test]
fn a_fix_picks_the_remedy_the_warning_names() {
    fn fix_for(warning: &fabric::WarningKind) -> String {
        super::fix_for(warning, &Role::default())
    }
    let held = |reason: &str, origin: &str| {
        fix_for(&fabric::WarningKind::SessionHeld {
            sid: "ta000000000".to_string(),
            pid: 1,
            sock: None,
            origin: Some(origin.to_string()),
            fabric_lost: reason == "fabric-lost",
        })
    };
    assert_eq!(
        held("x", "local"),
        "`aterm ctl --pid 1 hold ta000000000 off`"
    );
    assert_eq!(
        fix_for(&fabric::WarningKind::SessionHeld {
            sid: String::new(),
            pid: 0,
            sock: None,
            origin: Some("local".to_string()),
            fabric_lost: false,
        }),
        "`aterm ctl --pid <pid> hold <sid> off`"
    );
    assert_eq!(
        fix_for(&fabric::WarningKind::SessionHeld {
            sid: "ta000000000".to_string(),
            pid: 1,
            sock: Some("/tmp/at.x/a.sock".to_string()),
            origin: Some("local".to_string()),
            fabric_lost: false,
        }),
        "`aterm ctl --sock /tmp/at.x/a.sock hold ta000000000 off`"
    );
    assert_eq!(
        fix_for(&fabric::WarningKind::SessionHeld {
            sid: "ta000000000".to_string(),
            pid: 0,
            sock: Some("/tmp/a b.sock".to_string()),
            origin: Some("local".to_string()),
            fabric_lost: false,
        }),
        "`aterm ctl --sock '/tmp/a b.sock' hold ta000000000 off`"
    );
    assert_eq!(
        fix_for(&fabric::WarningKind::SessionHeld {
            sid: "ta000000000".to_string(),
            pid: 0,
            sock: Some("/tmp/a'b.sock".to_string()),
            origin: Some("local".to_string()),
            fabric_lost: false,
        }),
        "`aterm ctl --sock <sock> hold ta000000000 off`"
    );
    assert_eq!(
        arm_command(
            4242,
            "/tmp/at.x/a.sock",
            &["--node".to_string(), "n1".to_string()]
        ),
        "arm 4242 (`aterm ctl --sock /tmp/at.x/a.sock fabric attach --node n1`)"
    );
    assert_eq!(
        fix_for(&fabric::WarningKind::SessionHeld {
            sid: "ta000000000".to_string(),
            pid: 0,
            sock: None,
            origin: Some("local".to_string()),
            fabric_lost: false,
        }),
        "`aterm ctl --pid <pid> hold ta000000000 off`"
    );
    let fleet = held("stop", "fleet");
    assert!(
        fleet.contains("aterm fabric halt") && !fleet.contains("hold ta000000000 off"),
        "{fleet}"
    );
    let both = held("stop", "fleet,local");
    assert!(
        both.contains("aterm fabric halt")
            && both.contains("`aterm ctl --pid 1 hold ta000000000 off`"),
        "{both}"
    );
    let lost = held("fabric-lost", "fleet,local");
    assert!(
        lost.contains("bridge reconnects") && lost.contains("hold ta000000000 off"),
        "{lost}"
    );
    let person = held("taken-over", "person");
    assert_eq!(person, PERSON_HOLD_LIFT);
    let beside = held("taken-over", "person,local");
    assert!(
        beside.starts_with(PERSON_HOLD_LIFT)
            && beside.contains("`aterm ctl --pid 1 hold ta000000000 off`"),
        "{beside}"
    );
    let remote = held("-", "remote");
    assert!(
        remote.starts_with(REMOTE_HOLD_LIFT)
            && remote.ends_with("so does `aterm ctl --pid 1 hold ta000000000 off`"),
        "{remote}"
    );
    let beside = held("-", "remote,fleet");
    assert!(
        beside.starts_with(REMOTE_HOLD_LIFT) && beside.contains("aterm fabric halt"),
        "{beside}"
    );
    let all = held("taken-over", "person,fleet,local");
    assert!(
        all.starts_with(PERSON_HOLD_LIFT)
            && all.contains("aterm fabric halt")
            && all.contains("hold ta000000000 off"),
        "{all}"
    );

    let down = |reason: &str| {
        fix_for(&fabric::WarningKind::BridgeLinkDown {
            reason: reason.to_string(),
            last_word: None,
        })
    };
    assert!(
        down("expired").contains("has lapsed") && down("expired") != down("attach"),
        "{}",
        down("expired")
    );
    assert!(
        fix_for(&fabric::WarningKind::BrokerCapLapsed)
            .starts_with(&renew_remedy("`aterm fabric on`")),
        "{}",
        fix_for(&fabric::WarningKind::BrokerCapLapsed)
    );
    let here = protocol_remedy("`aterm fabric on`", Keeper::Job);
    assert!(down("protocol").starts_with(&here), "{}", down("protocol"));
    assert_eq!(fix_for(&fabric::WarningKind::BrokerProtocolSkew), here);
    for reason in ["busy", "record", "protocol"] {
        assert!(
            !down(reason).contains("node.cap aside") && down(reason) != down("other"),
            "{reason}: {}",
            down(reason)
        );
    }
    assert!(
        down("cut-say") != down("closed") && down("cut-inbox") == down("cut-say"),
        "{}",
        down("cut-say")
    );
    let unread = fix_for(&fabric::WarningKind::BusUnreadable);
    assert!(
        unread.contains("move <root>/node.cap aside"),
        "a ring for this fleet that lacks a grant passes `on` as already: {unread}"
    );
}

#[test]
fn rendezvous_defaults_fill_only_absent_flags() {
    let r = Rendezvous {
        fleet: "f".to_string(),
        broker: "/b".to_string(),
        node: "n-a".to_string(),
        cap_file: "/c".to_string(),
        state: "/s".to_string(),
        command: words("x"),
    };
    let fill = |args: &[&str]| -> Vec<String> {
        let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        fill_defaults(&args, &r)
    };
    assert_eq!(
        fill(&["--attention"]).join(" "),
        "--fleet f --broker /b --cap-file /c --state /s --attention"
    );
    assert_eq!(
        fill(&["--fleet", "g", "--state", "/t"]).join(" "),
        "--broker /b --cap-file /c --fleet g --state /t"
    );
    assert_eq!(root_of("/r/bus.sock"), Some(PathBuf::from("/r")));
    assert_eq!(root_of("bus.sock"), None, "a relative socket");
}

#[test]
fn an_instance_replaced_after_the_listing_is_asked_with_its_own_token() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    let scratch = crate::test_scratch_dir("repl");
    let dir = scratch.path();
    let sock = dir.join("a.sock");
    let token_file = dir.join("a.sock.token");
    std::fs::write(&token_file, "deadbeef\n").expect("token");
    let serve = |listener: UnixListener, reply: &'static str| {
        std::thread::spawn(move || {
            let (conn, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Until(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
                aterm_uds::AcceptedMode::Blocking,
            )
            .expect("accept within its one-minute hang detector");
            conn.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                .expect("bound the required requests");
            conn.set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                .expect("bound the scripted reply");
            let mut lines = BufReader::new(conn.try_clone().expect("clone"));
            let mut seen = Vec::new();
            for _ in 0..2 {
                let mut line = String::new();
                assert_ne!(
                    lines.read_line(&mut line).expect("a request line"),
                    0,
                    "the instance connection ended before both requests"
                );
                seen.push(line);
            }
            (&conn).write_all(reply.as_bytes()).expect("reply");
            seen
        })
    };
    let listing = serve(
        UnixListener::bind(&sock).expect("bind"),
        "OK supervised=0 state=- command=-\n",
    );
    let path = sock.to_str().expect("utf8").to_string();
    let inst = ask_instance(std::process::id(), path).expect("listed");
    assert_eq!(
        listing.join().expect("server"),
        ["AUTH deadbeef\n", "fabric status\n"]
    );
    std::fs::remove_file(&token_file).expect("unlink the old token");
    std::fs::write(&token_file, "cafef00d\n").expect("the new token");
    std::fs::remove_file(&sock).expect("unlink the old socket");
    let asked = serve(
        UnixListener::bind(&sock).expect("rebind"),
        "OK 1\n0 t0000000001 - alive zsh\n",
    );
    assert_eq!(sessions_of(&inst), ["t0000000001"]);
    assert_eq!(
        asked.join().expect("server"),
        ["AUTH cafef00d\n", "sessions\n"]
    );
}

/// The proof against a scripted instance: its `sessions` listing names `t0000000001`, then the proof's
/// own connection gets `replies` in order. Answers the proof's result and that connection's
/// requests.
fn prove_against(name: &str, replies: &[&str]) -> (Result<u64, String>, Vec<String>) {
    use std::io::{BufRead, BufReader, Write};
    assert!(crate::subject::is_session_id("t0000000001"));
    let scratch = crate::test_scratch_dir(name);
    let dir = scratch.path();
    let sock = dir.join("a.sock");
    std::fs::write(dir.join("a.sock.token"), "deadbeef\n").expect("token");
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
    let replies: Vec<String> = replies.iter().map(|r| (*r).to_string()).collect();
    let server = std::thread::spawn(move || {
        let (listing, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Until(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
            aterm_uds::AcceptedMode::Blocking,
        )
        .expect("accept the listing within its one-minute hang detector");
        listing
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .expect("bound the required listing requests");
        listing
            .set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .expect("bound the listing reply");
        let mut lines = BufReader::new(listing.try_clone().expect("clone"));
        for _ in 0..2 {
            assert_ne!(
                lines
                    .read_line(&mut String::new())
                    .expect("AUTH, then sessions"),
                0,
                "the listing connection ended before AUTH and sessions"
            );
        }
        (&listing)
            .write_all(b"OK 1\n0 t0000000001 - alive zsh\n")
            .expect("list");
        let (conn, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Until(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
            aterm_uds::AcceptedMode::Blocking,
        )
        .expect("accept the proof within its one-minute hang detector");
        conn.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .expect("bound the required proof requests");
        conn.set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .expect("bound the proof replies");
        let mut lines = BufReader::new(conn.try_clone().expect("clone"));
        assert_ne!(
            lines.read_line(&mut String::new()).expect("AUTH"),
            0,
            "the proof connection ended before AUTH"
        );
        let mut seen = Vec::new();
        for reply in replies {
            let mut line = String::new();
            if lines.read_line(&mut line).expect("a request") == 0 {
                break;
            }
            seen.push(line.trim_end().to_string());
            (&conn).write_all(reply.as_bytes()).expect("reply");
        }
        seen
    });
    let inst = Instance {
        pid: std::process::id(),
        sock: sock.to_str().expect("utf8").to_string(),
        supervised: true,
        state: "connected".to_string(),
        reason: String::new(),
        command: None,
    };
    let proved = prove(&[inst], &mut Out::kept());
    let seen = server.join().expect("server");
    (proved, seen)
}

#[test]
fn the_proof_waits_past_another_senders_note_for_its_own() {
    let (proved, seen) = prove_against(
        "proof-past",
        &[
            "OK 0 hold=0\n",
            "OK 1 off=6\n",
            "OK inbox 1\n",
            "OK 1 hold=0\nmsg 1 off=5 t=1 from=t0000000009@n-b kind=note trust=agent\n",
            "OK inbox 2\n",
            "OK 1 hold=0\nmsg 2 off=6 t=2 from=t0000000001@n-a kind=note trust=agent\n",
        ],
    );
    assert!(proved.is_ok(), "{proved:?}: {seen:#?}");
    assert!(
        seen[4].starts_with("@t0000000001 await inbox since=1 kinds=note "),
        "the second wait starts past the other sender's note: {seen:#?}"
    );
}

#[test]
fn the_proof_accepts_a_combined_bus_and_direct_mail_wake() {
    let (proved, seen) = prove_against(
        "proof-direct",
        &[
            "OK 0 hold=0\n",
            "OK 1 off=6\n",
            "OK inbox 1 fleet=f1\n",
            "OK 1 hold=0\nmsg 1 off=5 t=1 from=t0000000009@n-b kind=note trust=agent\n",
            "OK inbox 2\n",
            "OK 1 hold=0\nmsg 2 off=6 t=2 from=t0000000001@n-a kind=note trust=agent\n",
        ],
    );
    assert!(proved.is_ok(), "{proved:?}: {seen:#?}");
    assert!(
        seen[4].starts_with("@t0000000001 await inbox since=1 kinds=note "),
        "the direct metadata must not replace the bus cursor: {seen:#?}"
    );
}

#[test]
fn inbox_wait_cursor_rejects_malformed_metadata() {
    assert_eq!(inbox_wait_cursor("OK inbox 0"), Some(0));
    for header in [
        "OK timeout",
        "OK inbox",
        "OK inbox nope fleet=f1",
        "OK inbox 18446744073709551616",
        "OK inbox 1 fleet=1",
        "OK inbox 1 fleet=f",
        "OK inbox 1 fleet=f0",
        "OK inbox 1 fleet=f+1",
        "OK inbox 1 fleet=f18446744073709551616",
        "OK inbox 1 unknown=f1",
        "OK inbox 1 fleet=f1 fleet=f2",
        "OK inbox 1 fleet=f1 trailing",
    ] {
        assert_eq!(inbox_wait_cursor(header), None, "{header}");
    }
}

#[test]
fn a_proof_wait_that_times_out_says_the_note_did_not_come_back() {
    let (proved, seen) = prove_against(
        "proof-timeout",
        &["OK 0 hold=0\n", "OK 1 off=6\n", "OK timeout\n"],
    );
    let why = proved.expect_err("nothing came back");
    assert!(
        why.starts_with("landed at @6 but was not delivered back within 60s: ")
            && why.contains("OK timeout"),
        "{why}: {seen:#?}"
    );
}

#[test]
fn an_explicit_broker_gets_no_rendezvous_cap_or_state() {
    for broker in ["/r/bus.sock", "127.0.0.1:7000"] {
        let r = Rendezvous {
            fleet: "f".to_string(),
            broker: broker.to_string(),
            node: "n-a".to_string(),
            cap_file: "/r/node.cap".to_string(),
            state: "/r/link-state".to_string(),
            command: words("x"),
        };
        let fill = |args: &[&str]| -> String {
            let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
            fill_defaults(&args, &r).join(" ")
        };
        let got = fill(&["--broker", "/elsewhere/b.sock"]);
        assert_eq!(got, "--fleet f --broker /elsewhere/b.sock", "{broker}");
        assert!(
            !got.contains("/r/node.cap") && !got.contains("/r/link-state"),
            "{broker}: the node's cap/state followed a named broker: {got}"
        );
        assert_eq!(
            fill(&[
                "--broker",
                "/e.sock",
                "--cap-file",
                "/mine.cap",
                "--state",
                "/s"
            ]),
            "--fleet f --broker /e.sock --cap-file /mine.cap --state /s",
            "{broker}: the caller's own cap and state are kept"
        );
    }
}

#[test]
fn a_cap_minted_under_the_roots_secret_is_recognised() {
    let cap_is_for = |path: &Path, fleet: &str, node: &str, secret: &[u8]| {
        let ring = crate::bridge::read_cap_file(&path.to_string_lossy());
        ring_is_for(ring, fleet, node, secret)
    };
    let scratch = crate::test_scratch_dir("minted");
    let dir = scratch.path();
    let node = "n-0123456789abcdef";
    let ring = node_rules("lab", node);
    let cap_under = |key: &[u8], name: &str, grants: &[String]| {
        let lines: String = grants
            .iter()
            .map(|g| {
                let c = astream_cap::mint(key, g).expect("mint");
                astream_cap::capfile::format_line(&c.filter, &c.tag) + "\n"
            })
            .collect();
        let path = dir.join(name);
        crate::bridge::write_owner_only(&path, lines);
        path
    };
    let own = cap_under(&[4u8; 32], "own.cap", &ring);
    assert_eq!(cap_is_for(&own, "lab", node, &[4u8; 32]), Ok(ring.len()));
    {
        use std::os::unix::fs::PermissionsExt as _;
        let at = |mode| std::fs::set_permissions(&own, std::fs::Permissions::from_mode(mode));
        at(0o644).expect("widen");
        let miss = cap_is_for(&own, "lab", node, &[4u8; 32]).expect_err("others can read it");
        assert!(miss.refused && !miss.keep, "{miss:?}");
        assert!(miss.why.contains("chmod 600"), "{miss:?}");
        at(0o000).expect("close");
        let miss = cap_is_for(&own, "lab", node, &[4u8; 32]).expect_err("the OS refuses it");
        assert!(
            miss.refused && !miss.keep,
            "OS permissions are a refusal: {miss:?}"
        );
        at(0o600).expect("narrow");
    }
    assert_eq!(
        cap_is_for(&own, "lab", "n-other", &[4u8; 32]),
        Err(CapMiss {
            why: "minted for a different node id, not n-other".to_string(),
            keep: true,
            refused: false,
        }),
        "another node's ring, the last record of its id once the state dir names another"
    );
    let foreign = cap_under(&[5u8; 32], "foreign.cap", &ring);
    assert_eq!(
        cap_is_for(&foreign, "lab", node, &[4u8; 32]),
        Err(CapMiss::from(
            "not minted under the current mint secret".to_string()
        )),
        "another secret's ring of this node, which `on` mints again"
    );
    assert_eq!(
        cap_is_for(&foreign, "lab", "n-other", &[4u8; 32]),
        Err(CapMiss {
            why: "not minted under the current mint secret".to_string(),
            keep: true,
            refused: false,
        }),
        "another secret's ring is judged by its secret first, and kept: no `on` for n-other \
         mints it"
    );
    assert!(cap_is_for(&dir.join("absent.cap"), "lab", node, &[4u8; 32]).is_err());
    assert_eq!(
        cap_is_for(&own, "work", node, &[4u8; 32]),
        Err(CapMiss {
            why: "minted for fleet lab, not work".to_string(),
            keep: false,
            refused: false,
        }),
        "another fleet's ring, which `on --fleet lab` mints again grant for grant"
    );
    let by_hand = cap_under(&[4u8; 32], "hand.cap", &ring[..5]);
    assert_eq!(cap_is_for(&by_hand, "lab", node, &[4u8; 32]), Ok(5));
    assert!(
        cap_is_for(&by_hand, "work", node, &[4u8; 32]).is_err_and(|m| m.keep),
        "a ring no `on` mints is kept aside"
    );
}

/// A RING WIDER THAN THE NODE'S CEILING IS KEPT AND NAMED; THE ONE AN EARLIER ATERM MINTED IS
/// MINTED OVER (DESIGN-aterm-fabric §14 Q5): the node's ring and one extra unbound
/// `rw:/f/lab/>`, minted under the root's own secret, is refused with the ceiling check's
/// first violation and kept aside; so is the node's ring with one of the cut `term/` grants.
/// The eight grants an aterm before D08 (`49b5230d88`) minted, on this fleet or another, are
/// minted over without being kept. `doctor` names the wide ring alone, with the fix `on`.
/// NEGATIVE CONTROL: the wide ring passes every test `on` made of a ring before the ceiling
/// check (each tag verifies under the secret, a grant binds the node, every grant lies in
/// the fleet), so without `check_ring` it is accepted as this node's, as it was (run: a
/// `ring_is_for` that takes `Fit::Wider` as `Within` answers `Ok(7)` and fails here).
#[test]
fn a_ring_wider_than_the_ceiling_is_kept_and_the_earlier_ring_is_minted_over() {
    let secret = [4u8; 32];
    let dir = crate::test_scratch_dir("en");
    let node = "n-0123456789abcdef";
    let ceiling = astream_cap::GrantCeiling::aterm_node_ring("lab", node).expect("a node");
    let cap_under = |name: &str, grants: &[String]| -> Vec<crate::bridge::Cap> {
        let lines: String = grants
            .iter()
            .map(|g| {
                let c = astream_cap::mint(&secret, g).expect("mint");
                astream_cap::capfile::format_line(&c.filter, &c.tag) + "\n"
            })
            .collect();
        let path = dir.path().join(name);
        crate::bridge::write_owner_only(&path, lines);
        crate::bridge::read_cap_file(&path.to_string_lossy()).expect("a ring")
    };
    let held = |caps: &[crate::bridge::Cap]| capabilities(caps).expect("parses");
    let named = |caps: &[crate::bridge::Cap]| {
        wider_than_ceiling(&[("/r/node.cap", caps)], "lab", node)
            .into_iter()
            .map(|(_, why)| why)
            .next()
    };

    let mut wide = ceiling.rules();
    wide.push("rw:/f/lab/>".to_string());
    let wide = cap_under("wide.cap", &wide);
    let control = held(&wide);
    assert!(
        control.iter().all(|c| astream_cap::verify(&secret, c))
            && binds(&control, node)
            && control
                .iter()
                .all(|c| fleet_of(&c.filter).as_deref() == Some("lab")),
        "NEGATIVE CONTROL: the ring passes every test before the ceiling check"
    );
    let miss = ring_is_for(Ok(wide.clone()), "lab", node, &secret).expect_err("wide");
    assert!(miss.keep && !miss.refused, "{miss:?}");
    assert!(
        miss.why.starts_with(WIDER_RING)
            && miss
                .why
                .contains(&format!("ring[{}]", ceiling.rules().len()))
            && miss.why.contains("rw:/f/lab/>")
            && miss.why.contains("god capability"),
        "{miss:?}"
    );
    let named_wide = named(&wide).expect("doctor names it");
    assert_eq!(named_wide, miss.why, "doctor and `on` say the same");
    let warning = format!("cap file /r/node.cap: {named_wide}");
    let fix = fix_for(&fabric::WarningKind::CapWiderRing, &Role::default());
    assert!(
        fix.contains("<root>/node.cap") && fix.contains("keeps aside"),
        "{warning}: {fix}"
    );

    let earlier = earlier_node_grants("lab", node);
    let mut one_term = ceiling.rules();
    one_term.push(earlier[7].clone());
    let one_term = cap_under("one-term.cap", &one_term);
    let miss = ring_is_for(Ok(one_term.clone()), "lab", node, &secret).expect_err("wide");
    assert!(miss.keep && miss.why.starts_with(WIDER_RING), "{miss:?}");
    assert!(named(&one_term).is_some());

    let mut shuffled = earlier.clone();
    shuffled.reverse();
    let earlier = cap_under("earlier.cap", &shuffled);
    let miss = ring_is_for(Ok(earlier.clone()), "lab", node, &secret).expect_err("earlier");
    assert!(!miss.keep && !miss.refused, "{miss:?}");
    assert!(miss.why.contains("earlier aterm"), "{miss:?}");
    assert_eq!(named(&earlier), None);
    let elsewhere = cap_under("elsewhere.cap", &earlier_node_grants("work", node));
    let miss = ring_is_for(Ok(elsewhere.clone()), "lab", node, &secret).expect_err("other");
    assert!(
        !miss.keep && miss.why == "minted for fleet work, not lab",
        "{miss:?}"
    );
    assert_eq!(named(&elsewhere), None);

    let own = cap_under("own.cap", &ceiling.rules());
    assert_eq!(
        ring_is_for(Ok(own.clone()), "lab", node, &secret),
        Ok(ceiling.rules().len())
    );
    assert_eq!(named(&own), None);
}

/// Each of `grants`, minted under `key`, as a cap file at `dir/name` reads back.
fn ring_in(
    dir: &Path,
    key: &[u8],
    name: &str,
    grants: &[String],
) -> io::Result<Vec<crate::bridge::Cap>> {
    let lines: String = grants
        .iter()
        .map(|g| {
            let c = astream_cap::mint(key, g).expect("mint");
            astream_cap::capfile::format_line(&c.filter, &c.tag) + "\n"
        })
        .collect();
    let path = dir.join(name);
    crate::bridge::write_owner_only(&path, lines);
    crate::bridge::read_cap_file(&path.to_string_lossy())
}

/// A GOD CAP ALONE IN A SECOND CAP FILE IS NAMED (DESIGN-aterm-fabric §14 Q5): the bridge
/// attaches every `--cap-file` of its command as one ring, so the node's own ring in one
/// file and an unbound `rw:/f/lab/>` alone in another is a ring wider than the node's, and
/// `doctor` names the second file, in either order, with the violation indexed in that file;
/// so it does an unbound `rw:/f/work/>` there, and a read-write grant behind a read one, at
/// its own index. A reader's ring beside the node's (`ro:/f/lab/>`, which publishes as no
/// one) is not named. NEGATIVE CONTROL: judged alone, as each file was before, the second
/// file binds no grant to the node and is not named.
#[test]
fn a_god_cap_alone_in_a_second_cap_file_is_named() {
    let secret = [4u8; 32];
    let dir = crate::test_scratch_dir("en");
    let node = "n-0123456789abcdef";
    let rules = node_rules("lab", node);
    let own = ring_in(dir.path(), &secret, "node.cap", &rules).expect("a ring");
    let god =
        ring_in(dir.path(), &secret, "god.cap", &["rw:/f/lab/>".to_string()]).expect("a ring");
    assert_eq!(
        wider_than_ceiling(&[("/r/god.cap", &god[..])], "lab", node),
        Vec::new(),
        "NEGATIVE CONTROL: alone, the file binds no grant to the node"
    );
    for files in [
        [("/r/node.cap", &own[..]), ("/r/god.cap", &god[..])],
        [("/r/god.cap", &god[..]), ("/r/node.cap", &own[..])],
    ] {
        let named = wider_than_ceiling(&files, "lab", node);
        assert!(
            named.len() == 1
                && named[0].0 == "/r/god.cap"
                && named[0].1.starts_with(WIDER_RING)
                && named[0].1.contains("ring[0]")
                && named[0].1.contains("rw:/f/lab/>"),
            "{named:?}"
        );
    }
    let beside = |name: &str, grants: &[&str]| {
        let grants: Vec<String> = grants.iter().map(|g| (*g).to_string()).collect();
        let ring = ring_in(dir.path(), &secret, name, &grants).expect("a ring");
        wider_than_ceiling(&[("/r/node.cap", &own[..]), (name, &ring[..])], "lab", node)
            .into_iter()
            .map(|(path, why)| format!("{path}: {why}"))
            .collect::<Vec<_>>()
    };
    let work = beside("work.cap", &["rw:/f/work/>"]);
    assert!(
        work.len() == 1 && work[0].starts_with("work.cap: ") && work[0].contains("ring[0]"),
        "another fleet's god cap: {work:?}"
    );
    let behind = beside("behind.cap", &["ro:/f/lab/>", "rw:/f/lab/>"]);
    assert!(
        behind.len() == 1 && behind[0].contains("ring[1]") && behind[0].contains("rw:/f/lab/>"),
        "a write behind a read: {behind:?}"
    );
    assert_eq!(
        beside("reader.cap", &["ro:/f/lab/>"]),
        Vec::<String>::new(),
        "a reader's ring"
    );
}

/// WHAT `doctor` NAMES OF `<root>/node.cap`, `on` KEEPS ASIDE, so the fix `doctor` offers
/// holds: over rings of each shape `on` judges (the node's own, wide, with one cut `term/`
/// grant, the earlier eight, another fleet's, another node's, another node's made wide, a
/// god cap alone, a partial ring), each under the root's secret and under another, and each
/// alone or beside a second cap file holding the node's own ring, every ring `doctor` names
/// is one `ring_is_for` refuses with `keep`. CONTROL: the matrix names rings under each
/// secret, the second file's among them, so the implication is not vacuous.
#[test]
fn every_ring_doctor_names_on_keeps_aside() {
    let (root, other) = ([4u8; 32], [5u8; 32]);
    let dir = crate::test_scratch_dir("en");
    let node = "n-0123456789abcdef";
    let ceiling = astream_cap::GrantCeiling::aterm_node_ring("lab", node).expect("a node");
    let rules = ceiling.rules();
    let earlier = earlier_node_grants("lab", node);
    let god = "rw:/f/lab/>".to_string();
    let with = |ring: &[String], extra: &str| {
        let mut ring = ring.to_vec();
        ring.push(extra.to_string());
        ring
    };
    let another = node_rules("lab", "n-other");
    let shapes: [(&str, Vec<String>); 9] = [
        ("own", rules.clone()),
        ("wide", with(&rules, &god)),
        ("one-term", with(&rules, &earlier[7])),
        ("earlier", earlier.clone()),
        ("work", earlier_node_grants("work", node)),
        ("another", another.clone()),
        ("another-wide", with(&another, &god)),
        ("god", vec![god.clone()]),
        ("partial", rules[..5].to_vec()),
    ];
    let beside = ring_in(dir.path(), &root, "beside.cap", &rules).expect("a ring");
    let mut named = Vec::new();
    for (secret, under) in [(&root, "root"), (&other, "other")] {
        for (shape, grants) in &shapes {
            let caps = ring_in(dir.path(), secret, &format!("{shape}-{under}.cap"), grants)
                .expect("a ring");
            for second in [false, true] {
                let mut files = vec![("/r/node.cap", &caps[..])];
                if second {
                    files.push(("/r/beside.cap", &beside[..]));
                }
                if !wider_than_ceiling(&files, "lab", node)
                    .iter()
                    .any(|(path, _)| *path == "/r/node.cap")
                {
                    continue;
                }
                named.push(format!("{shape}/{under}/{second}"));
                let miss = ring_is_for(Ok(caps.clone()), "lab", node, &root)
                    .expect_err("a ring doctor names is not the node's");
                assert!(
                    miss.keep && !miss.refused,
                    "{shape} under the {under} secret (second file: {second}): {miss:?}"
                );
            }
        }
    }
    for want in ["wide/root/false", "wide/other/false", "god/other/true"] {
        assert!(
            named.iter().any(|n| n == want),
            "CONTROL: {want} named: {named:?}"
        );
    }
}

/// A RING NO `on` MINTS IS KEPT ASIDE WHATEVER SECRET MINTED IT (DESIGN-aterm-fabric §14 Q5):
/// the node's ring and an unbound `rw:/f/lab/>`, minted under a secret other than the
/// root's, is a ring `doctor` names wider than the node's, and `on` keeps it aside before it
/// mints the node's own in its place, as it keeps one minted under the root's secret. A ring
/// `on` mints grant for grant (the node's own, or the eight grants an aterm before D08
/// minted) under another secret is minted over without being kept: `on` mints it again.
/// NEGATIVE CONTROL: that ring under the root's secret is kept for being wide, so it is the
/// secret alone that the check must look past.
#[test]
fn a_wide_ring_under_another_secret_is_kept_aside() {
    let (root, other) = ([4u8; 32], [5u8; 32]);
    let dir = crate::test_scratch_dir("en");
    let node = "n-0123456789abcdef";
    let ceiling = astream_cap::GrantCeiling::aterm_node_ring("lab", node).expect("a node");
    let ring_under = |key: &[u8], name: &str, grants: &[String]| {
        let lines: String = grants
            .iter()
            .map(|g| {
                let c = astream_cap::mint(key, g).expect("mint");
                astream_cap::capfile::format_line(&c.filter, &c.tag) + "\n"
            })
            .collect();
        let path = dir.path().join(name);
        crate::bridge::write_owner_only(&path, lines);
        crate::bridge::read_cap_file(&path.to_string_lossy())
    };
    let mut wide = ceiling.rules();
    wide.push("rw:/f/lab/>".to_string());
    let control = ring_is_for(ring_under(&root, "root.cap", &wide), "lab", node, &root)
        .expect_err("NEGATIVE CONTROL: wide");
    assert!(control.keep, "NEGATIVE CONTROL: {control:?}");

    let miss = ring_is_for(ring_under(&other, "other.cap", &wide), "lab", node, &root)
        .expect_err("another secret");
    assert!(
        miss.keep && !miss.refused && miss.why.contains("not minted under"),
        "a wide ring under another secret is kept aside: {miss:?}"
    );
    for (name, grants) in [
        ("own.cap", ceiling.rules()),
        ("earlier.cap", earlier_node_grants("lab", node)),
    ] {
        let miss = ring_is_for(ring_under(&other, name, &grants), "lab", node, &root)
            .expect_err("another secret");
        assert!(!miss.keep, "{name}: `on` mints it again: {miss:?}");
    }
}

#[test]
fn a_node_id_is_n_dash_and_a_principal() {
    assert!(is_node_id("n-0123456789abcdef"));
    assert!(is_node_id("n-m7"));
    for bad in [
        "new",
        "n-",
        "t0000000123",
        "h-andrew",
        "n-UPPER",
        "n-a b",
        "n-a/b",
        "",
    ] {
        assert!(!is_node_id(bad), "{bad}");
    }
}

#[test]
fn the_deleted_scripts_shell_job_is_read_for_its_broker_and_nothing_else_is() {
    let words = |w: &[&str]| Some(w.iter().map(ToString::to_string).collect::<Vec<_>>());
    let legacy = "rm -f '/r/bus.sock'; exec '/u/aterm' link broker '/r/bus.sock' '/r/bus.log'";
    let want = words(&["/u/aterm", "link", "broker", "/r/bus.sock", "/r/bus.log"]);
    assert_eq!(sh_c_words(legacy), want);
    assert_eq!(
        sh_c_words("exec '/u/aterm' 'link' 'broker' '/r/bus.sock' '/r/bus.log'"),
        want,
        "the unit's every-word-quoted form"
    );
    assert_eq!(
        sh_c_words(" \trm -f /r/bus.sock;exec\t/u/aterm link broker /r/bus.sock /r/bus.log \n"),
        want,
        "bare words, tabs, no space after `;`, whitespace around the script"
    );
    assert_eq!(sh_c_words("exec /u/aterm ''"), words(&["/u/aterm", ""]));
    for bad in [
        "curl x; exec /u/aterm link broker a b",
        "rm -f a; exec /u/aterm link broker a b && true",
        "rm -f a; exec /u/aterm link broker a b; true",
        "rm -f a b; exec /u/aterm link broker a b",
        "rm -f a; rm -f b; exec /u/aterm link broker a b",
        "exec \"/u/aterm\" link broker a b",
        "exec $X broker a b",
        "exec /u/aterm link broker a\\ b",
        "exec /u/aterm link broker /r/*.sock",
        "exec /u/aterm link broker 'a'b",
        "exec /u/aterm link broker 'a",
        "exec /u/aterm link broker a\nb",
        "rm -f a; /u/aterm link broker a b",
        "/u/aterm link broker a b",
        "exec",
        "rm -f a; exec",
        "",
    ] {
        assert_eq!(sh_c_words(bad), None, "{bad:?}");
    }

    let sh = |script: &str| vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
    assert_eq!(supervised_argv(sh(legacy)), want, "the shell job unwraps");
    assert_eq!(supervised_argv(sh("curl x; exec a")), None);
    let direct = want.clone().expect("words");
    assert_eq!(
        supervised_argv(direct.clone()),
        Some(direct),
        "a direct argv is the command"
    );
    let zsh = vec!["/bin/zsh".to_string(), "-c".to_string(), legacy.to_string()];
    assert_eq!(
        supervised_argv(zsh.clone()),
        Some(zsh),
        "only `sh` is unwrapped: another shell is an argv `Role::of` refuses"
    );
}

#[test]
fn role_of_a_root_with_the_scripts_plist_is_known_and_not_current() {
    let dir = crate::test_scratch_dir("en");
    let root = dir.path().join("fabric");
    std::fs::create_dir_all(&root).expect("root");
    let p = Paths {
        home: dir.path().join("home"),
        login_home: Some(dir.path().join("login")),
        root: root.clone(),
        aterm: "/u/aterm".to_string(),
        fleet: DEFAULT_FLEET.to_string(),
        config: None,
        rendezvous: None,
        durability: None,
    };
    assert!(
        p.plist().starts_with(&root),
        "a non-default root keeps its plist"
    );
    let r = root.to_string_lossy().into_owned();
    let plist = |script: &str| {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
             \t<key>Label</key>\n\t<string>{label}</string>\n\
             \t<key>ProgramArguments</key>\n\t<array>\n\
             \t\t<string>/bin/sh</string>\n\t\t<string>-c</string>\n\
             \t\t<string>{script}</string>\n\t</array>\n\
             \t<key>KeepAlive</key>\n\t<true/>\n</dict>\n</plist>\n",
            label = p.label(),
            script = aterm_codec::xml::escape(script),
        )
    };
    let install = |text: &str| std::fs::write(p.plist(), text).expect("plist");

    let legacy = plist(&format!(
        "rm -f '{r}/bus.sock'; exec '/u/aterm' link broker '{r}/bus.sock' '{r}/bus.log'"
    ));
    install(&legacy);
    assert_eq!(
        Role::of(&p),
        Ok(Role {
            fleet: None,
            keeper: Keeper::Job
        })
    );
    assert!(!p.plist_is_current(&legacy), "`on` still rewrites it");

    install(&plist("exec /usr/bin/true"));
    assert_eq!(Role::of(&p), Err(RoleError::NotABroker));
    install(&plist(&format!(
        "curl x; exec '/u/aterm' link broker '{r}/bus.sock' '{r}/bus.log'"
    )));
    assert_eq!(
        Role::of(&p),
        Err(RoleError::UnreadableJob {
            path: p.plist(),
            field: "ProgramArguments",
        })
    );
}

/// A root `p` under `dir` made private (0700), as `on` leaves it.
fn private_root(dir: &Path) -> Paths {
    let p = scratch_paths(dir);
    std::fs::create_dir_all(dir).expect("its folder");
    aterm_uds::private_file::ensure_private_dir(&p.root).expect("a private root");
    p
}

/// Set `path`'s modification time `ago` before now.
fn written_ago(path: &Path, ago: Duration) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(std::time::SystemTime::now() - ago))
        .expect("set its mtime");
}

/// The mesh switch picks who keeps the broker, the word `--service node` names it, and the
/// default without the mesh is the platform's supervisor, never the node service.
/// NEGATIVE CONTROL: a `default_for` that ignores `mesh` answers the platform's supervisor
/// for both.
#[test]
fn the_mesh_hands_the_broker_to_the_node_service() {
    assert_eq!(Service::default_for(true), Service::Node);
    assert_ne!(Service::default_for(false), Service::Node);
    assert_eq!(
        Service::default_here(),
        Service::default_for(aterm_types::mesh_switch::MESH)
    );
    assert_eq!(Service::parse("node"), Ok(Service::Node));
    assert_eq!(Service::Node.name(), "node");
    let refused = Service::parse("cron").expect_err("not a service");
    assert!(refused.contains("node"), "{refused}");
}

/// `on`'s broker step under the node service installs no job: with a broker job an earlier
/// build left loaded, launchd is only asked about it (no plist written, nothing booted out,
/// bootstrapped or kickstarted), the step says the node service retires it, records the tier
/// `--durability` named (0600) and waits for the node service to answer; one that does not
/// answer fails the step naming the node service, not a broker job. A step with no named tier
/// records none, and a dry run waits for nothing and writes nothing.
/// NEGATIVE CONTROLS: a Node arm that falls through to the launchd one writes and bootstraps
/// the plist (the calls); one that boots the old job out here leaves the Mac with no bus
/// until the node service's next pass (a `bootout` call); one that records the tier only
/// where it is not named reads `ordered` for a `--durability strict`.
#[test]
fn under_the_node_service_on_installs_no_job_and_records_a_named_tier() {
    use crate::durability::Durability;
    let dir = crate::test_scratch_dir("en");
    let p = Paths {
        durability: Some(Durability::Strict),
        ..private_root(dir.path())
    };
    let answering = || {
        Ok(fabric::BrokerView {
            endpoint: p.sock(),
            reachable: true,
            pid: Some(4242),
            durability: Some(Durability::Strict),
            ..fabric::BrokerView::default()
        })
    };
    let legacy = FakeLaunchd::new(true, 0);
    legacy.pid.set(Some(4242));
    let mut out = Out::kept();
    assert!(
        node_broker_step(&p, true, Service::Launchd, &legacy, answering, &mut out),
        "{}",
        out.said()
    );
    assert_eq!(legacy.calls(), ["print"], "launchd is only asked");
    assert!(!p.plist().exists(), "no plist is written");
    assert_eq!(recorded_durability(&p.root), Some(Durability::Strict));
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(p.root.join(DURABILITY_RECORD))
            .expect("the record")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the record is private");
    }
    let said = out.said();
    assert!(
        said.contains(&format!("launchd {} (pid 4242)", p.label()))
            && said.contains("retires it at its next pass")
            && said.contains(NODE_JOB)
            && !said.contains("WARNING"),
        "{said}"
    );

    let mut out = Out::kept();
    assert!(node_broker_step(
        &p,
        true,
        Service::Launchd,
        &FakeLaunchd::new(false, 0),
        answering,
        &mut out
    ));
    assert!(out.said().contains("already"), "{}", out.said());

    let mut out = Out::kept();
    let silent = || {
        Err(Box::new(fabric::BrokerView {
            endpoint: p.sock(),
            error: Some("connection refused".to_string()),
            ..fabric::BrokerView::default()
        }))
    };
    assert!(!node_broker_step(
        &p,
        true,
        Service::Launchd,
        &FakeLaunchd::new(false, 0),
        silent,
        &mut out
    ));
    let said = out.said();
    assert!(
        said.contains("FAILED")
            && said.contains(NODE_JOB)
            && said.contains("aterm node status")
            && !said.contains("launchd"),
        "{said}"
    );

    let unnamed = private_root(&dir.path().join("unnamed"));
    let mut out = Out::kept();
    let quiet = FakeLaunchd::new(false, 0);
    assert!(node_broker_step(
        &unnamed,
        false,
        Service::Launchd,
        &quiet,
        || {
            Ok(fabric::BrokerView {
                endpoint: unnamed.sock(),
                ..fabric::BrokerView::default()
            })
        },
        &mut out
    ));
    assert!(
        !unnamed.root.join(DURABILITY_RECORD).exists(),
        "no tier named, none recorded: {}",
        out.said()
    );
    assert!(
        !out.said().contains("earlier build"),
        "no job, none said: {}",
        out.said()
    );

    let dry = Paths {
        durability: Some(Durability::Relaxed),
        ..private_root(&dir.path().join("dry"))
    };
    let mut out = Out::new(true);
    out.kept = Some(std::cell::RefCell::new(Vec::new()));
    assert!(node_broker_step(
        &dry,
        true,
        Service::Launchd,
        &FakeLaunchd::new(false, 0),
        || panic!("a dry run waits for nothing"),
        &mut out
    ));
    assert!(
        !dry.root.join(DURABILITY_RECORD).exists(),
        "a dry run writes nothing: {}",
        out.said()
    );
}

/// Under the node service the broker is not judged and nothing of a job is stepped, a look and
/// an apply alike, so the converge pass's `sync.fabric` installs and restarts no broker job: a
/// configured fabric reads left to the node service even where the broker's build was
/// replaced, and with no command it still reads off, nothing looked at. A `[fabric] command`
/// aterm wrote whose program moves is still due there, and its apply runs the step for the
/// command alone.
/// NEGATIVE CONTROLS: the same replaced broker under launchd is renewed (the step runs), so a
/// renewal that ignores the service renews it under the node service too; one that leaves
/// before the look under the node service never rewrites the command whose program is gone.
#[test]
fn under_the_node_service_a_renewal_is_kept_and_steps_nothing() {
    let dir = crate::test_scratch_dir("en");
    let root = dir.path().join("root");
    aterm_uds::private_file::ensure_private_dir(&root).expect("the root");
    let command = || Some(words("aterm link serve"));
    // What `look_at` reads under the node service: no job, no supervisor, no holder.
    let hosted = |replaced: Option<&str>| {
        let mut at = rooted(found(Service::Node, replaced), &root);
        at.job = None;
        at.loaded = Ok(false);
        at.view.pid = None;
        at
    };
    for renew in [Renew::Look, Renew::Apply] {
        let kept = renew_with(
            command(),
            renew,
            |_: &[String]| Ok(hosted(Some("/old/aterm"))),
            |_: &Found, _: Steps| panic!("nothing is stepped"),
        );
        assert_eq!(kept, Ok(BrokerRenewal::Kept(NODE_HOSTS.to_owned())));
        let off = renew_with(
            None,
            renew,
            |_: &[String]| panic!("nothing is looked at"),
            |_: &Found, _: Steps| panic!("nothing is stepped"),
        );
        assert_eq!(off, Ok(BrokerRenewal::Off));
    }
    let app = "/Applications/aterm.app/Contents/MacOS/aterm";
    let moved = || {
        let mut at = hosted(None);
        at.command_program = Lasting::Moves(Moved {
            from: "/gone/aterm".to_string(),
            to: app.to_string(),
        });
        at
    };
    let due = renew_with(
        command(),
        Renew::Look,
        |_: &[String]| Ok(moved()),
        |_: &Found, _: Steps| panic!("a look runs no step"),
    );
    assert!(
        matches!(&due, Ok(BrokerRenewal::Due { from, to })
            if from == &format!(
                "`[fabric] command` runs /gone/aterm, which names no file now (the installed app \
                 is {app})"
            ) && to == &format!("`[fabric] command` runs {app}, the installed app")),
        "{due:?}"
    );
    let rewritten = renew_with(
        command(),
        Renew::Apply,
        |_: &[String]| Ok(moved()),
        |at: &Found, steps: Steps| {
            assert_eq!(at.broker_due(), None);
            assert_eq!(
                steps,
                Steps {
                    broker: false,
                    command: true,
                },
                "no broker is stepped"
            );
            Ok("config done updated [fabric] command".to_string())
        },
    );
    assert_eq!(
        rewritten,
        Ok(BrokerRenewal::Renewed(
            "config done updated [fabric] command".to_string()
        ))
    );
    let renewed = renew_with(
        command(),
        Renew::Apply,
        |_: &[String]| Ok(rooted(found(Service::Launchd, Some("/old/aterm")), &root)),
        |_: &Found, _: Steps| Ok("broker done (re)started launchd".to_string()),
    );
    assert!(
        matches!(renewed, Ok(BrokerRenewal::Renewed(_))),
        "{renewed:?}"
    );
}

/// Retiring a broker job records the tier its argv named before anything stops, boots the
/// job out by domain, waits for launchd to let it go and for the pid it ran to exit (it holds
/// bus.log's lock until then), and removes its plist; a second retire answers absent having
/// only asked launchd. A record newer than the job's plist (a person's `on --durability`
/// under the node service) stands; an older one is rewritten, and one under a job that named
/// no tier taken out. A pid that never exits fails the retire with what to do, its plist kept
/// for the next pass.
/// NEGATIVE CONTROLS: a retire that returns after the bootout without waiting leaves the
/// killed process running (`exiting` above zero) while the node service opens the log; one
/// that removes the plist before reading it records no tier; one that writes the record over
/// a newer one loses the person's later choice.
#[test]
fn retiring_a_broker_job_records_its_tier_waits_for_it_and_removes_its_plist() {
    use crate::durability::Durability;
    let dir = crate::test_scratch_dir("en");
    let p = private_root(dir.path());
    let install = |p: &Paths, tier: Option<Durability>| {
        let job = Paths {
            durability: tier,
            ..p.clone()
        };
        std::fs::write(p.plist(), job.plist_text()).expect("the job's plist");
    };
    let no_systemd = |_: &[&str]| -> Result<(), String> { panic!("no systemctl on launchd") };
    let no_unit = |_: &str| -> Option<u32> { panic!("no unit on launchd") };
    install(&p, Some(Durability::Relaxed));
    let lc = FakeLaunchd::new(true, 3);
    lc.pid.set(Some(4242));
    lc.exiting.set(2);
    let retired = retire_with(&p, Service::Launchd, &lc, &no_systemd, &no_unit);
    assert_eq!(
        retired,
        Ok(RetiredJob::Retired {
            label: p.label(),
            tier: Some(Durability::Relaxed),
        })
    );
    assert_eq!(lc.count("bootout"), 1, "{:?}", lc.calls());
    assert!(
        ["bootstrap", "kickstart", "write"]
            .iter()
            .all(|verb| lc.count(verb) == 0),
        "{:?}",
        lc.calls()
    );
    assert_eq!(lc.exiting.get(), 0, "the killed broker was waited for");
    assert!(!lc.held(), "launchd let it go");
    assert!(!p.plist().exists(), "its plist is gone");
    assert_eq!(recorded_durability(&p.root), Some(Durability::Relaxed));

    let before = lc.calls().len();
    assert_eq!(
        retire_with(&p, Service::Launchd, &lc, &no_systemd, &no_unit),
        Ok(RetiredJob::Absent)
    );
    assert_eq!(lc.calls()[before..], ["print"], "only launchd is asked");

    // A person's `on --durability strict` after the job was written stands.
    install(&p, Some(Durability::Relaxed));
    written_ago(&p.plist(), Duration::from_secs(60));
    write_durability_record(&p.root, Durability::Strict).expect("the person's record");
    let lc = FakeLaunchd::new(true, 0);
    assert_eq!(
        retire_with(&p, Service::Launchd, &lc, &no_systemd, &no_unit),
        Ok(RetiredJob::Retired {
            label: p.label(),
            tier: Some(Durability::Relaxed),
        })
    );
    assert_eq!(recorded_durability(&p.root), Some(Durability::Strict));

    // A job an older build wrote after the record names the tier; one that names none
    // takes the record out.
    write_durability_record(&p.root, Durability::Strict).expect("an older record");
    written_ago(&p.root.join(DURABILITY_RECORD), Duration::from_secs(60));
    install(&p, Some(Durability::Relaxed));
    assert_eq!(
        kept_durability(&p).map(|(tier, _)| tier),
        Ok(Some(Durability::Relaxed)),
        "`on` keeps the newer job's tier too"
    );
    let lc = FakeLaunchd::new(false, 0);
    assert!(retire_with(&p, Service::Launchd, &lc, &no_systemd, &no_unit).is_ok());
    assert_eq!(recorded_durability(&p.root), Some(Durability::Relaxed));
    assert_eq!(lc.count("bootout"), 0, "a job not loaded is not booted out");
    written_ago(&p.root.join(DURABILITY_RECORD), Duration::from_secs(60));
    install(&p, None);
    assert_eq!(
        retire_with(&p, Service::Launchd, &lc, &no_systemd, &no_unit),
        Ok(RetiredJob::Retired {
            label: p.label(),
            tier: None,
        })
    );
    assert!(
        !p.root.join(DURABILITY_RECORD).exists(),
        "the default: no record"
    );

    install(&p, None);
    let stuck = FakeLaunchd::new(true, 0);
    stuck.pid.set(Some(4242));
    stuck.exiting.set(FAKE_GONE_POLLS + 5);
    let failed = retire_with(&p, Service::Launchd, &stuck, &no_systemd, &no_unit);
    assert!(
        matches!(&failed, Err(why) if why.contains("kill 4242")),
        "{failed:?}"
    );
    assert!(p.plist().exists(), "kept for the next pass");
}

/// A systemd unit with no file left is stopped (not disabled, which needs its file), the pid
/// it ran waited for, and systemd reloaded; with no file and no pid nothing runs.
/// NEGATIVE CONTROL: a retire that disables a unit with no file fails where systemd refuses.
#[test]
fn retiring_a_systemd_broker_stops_it_and_waits_for_its_pid() {
    let dir = crate::test_scratch_dir("en");
    let p = private_root(dir.path());
    let lc = FakeLaunchd::new(false, 0);
    lc.exiting.set(2);
    let ran = std::cell::RefCell::new(Vec::new());
    let systemctl = |args: &[&str]| {
        ran.borrow_mut().push(args.join(" "));
        Ok(())
    };
    let retired = retire_with(&p, Service::Systemd, &lc, &systemctl, &|_| Some(4242));
    assert_eq!(
        retired,
        Ok(RetiredJob::Retired {
            label: p.label(),
            tier: None,
        })
    );
    assert_eq!(
        *ran.borrow(),
        [format!("stop {}", p.label()), "daemon-reload".to_string()]
    );
    assert_eq!(lc.exiting.get(), 0, "its pid was waited for");
    ran.borrow_mut().clear();
    assert_eq!(
        retire_with(&p, Service::Systemd, &lc, &systemctl, &|_| None),
        Ok(RetiredJob::Absent)
    );
    assert!(ran.borrow().is_empty(), "{:?}", ran.borrow());
}

/// `aterm fabric off` under the node service retires the job an earlier build left and says
/// the node service stops hosting bus.log; with none it says so and stops nothing.
/// NEGATIVE CONTROL: an off that leaves the legacy job loaded keeps a broker on bus.log the
/// person turned off (no `bootout` call).
#[test]
fn an_off_under_the_node_service_retires_a_legacy_job() {
    let dir = crate::test_scratch_dir("en");
    let p = private_root(dir.path());
    std::fs::write(p.plist(), p.plist_text()).expect("the job's plist");
    let lc = FakeLaunchd::new(true, 1);
    let mut out = Out::kept();
    off_node(
        &p,
        Service::Launchd,
        &lc,
        &|_| panic!("no systemctl"),
        &|_| panic!("no unit"),
        &mut out,
    );
    assert!(!out.failed, "{}", out.said());
    assert_eq!(lc.count("bootout"), 1);
    assert!(!p.plist().exists());
    assert!(
        out.said().contains("retired") && out.said().contains("stops hosting bus.log"),
        "{}",
        out.said()
    );
    let mut out = Out::kept();
    off_node(
        &p,
        Service::Launchd,
        &lc,
        &|_| panic!("no systemctl"),
        &|_| panic!("no unit"),
        &mut out,
    );
    assert!(
        out.said().contains("already") && !out.failed,
        "{}",
        out.said()
    );
}

/// What the node service hosts at a root: nothing where no mint secret is; off where an off
/// record stands (in the root or only beside aterm.toml), ahead of the person's hand record;
/// the person's broker where `on --service none` left its record, until an `on` under the
/// node service takes it out; else on, for the mesh's one fleet whatever fleet a bridge
/// command names, at the recorded tier (else ordered).
/// NEGATIVE CONTROL: a `bus_hosting` that skips the kept off record hosts the log of a
/// fabric whose root the person tidied after an off.
#[test]
fn the_node_service_hosts_bus_log_only_for_a_provisioned_fabric_that_is_on() {
    use crate::durability::Durability;
    let dir = crate::test_scratch_dir("en");
    let config_dir = dir.path().join("cfg/aterm");
    std::fs::create_dir_all(&config_dir).expect("aterm.toml's folder");
    let p = Paths {
        config: Some(config_dir.join("aterm.toml")),
        rendezvous: Some(dir.path().join("run/fabric.toml")),
        ..scratch_paths(dir.path())
    };
    assert_eq!(bus_hosting(&p), BusHosting::Unprovisioned);
    aterm_uds::private_file::ensure_private_dir(&p.root).expect("the root");
    assert_eq!(bus_hosting(&p), BusHosting::Unprovisioned);
    aterm_uds::private_file::write_private(&p.root, MINT_SECRET, &[7; SECRET_LEN])
        .expect("the secret");
    let on = |fleet: &str, tier| BusHosting::On {
        fleet: fleet.to_owned(),
        tier,
    };
    let fleet = crate::federate::FLEET;
    assert_eq!(bus_hosting(&p), on(fleet, Durability::Ordered));
    write_durability_record(&p.root, Durability::Strict).expect("a tier");
    assert_eq!(bus_hosting(&p), on(fleet, Durability::Strict));

    let lab = Paths {
        fleet: "lab".to_owned(),
        ..p.clone()
    };
    let (toml, _) =
        set_fabric_command("", &lab.bridge_command("n-0123456789abcdef")).expect("a command");
    std::fs::write(config_dir.join("aterm.toml"), toml).expect("aterm.toml");
    assert_eq!(
        bus_hosting(&p),
        on(fleet, Durability::Strict),
        "a bridge command's fleet is not the mesh's"
    );

    aterm_uds::private_file::write_private(&p.root, HAND_RECORD, b"").expect("by hand");
    assert_eq!(bus_hosting(&p), BusHosting::Hand);
    let mut out = Out::kept();
    assert!(record_step(&p, Service::Node, &mut out), "{}", out.said());
    assert_eq!(bus_hosting(&p), on(fleet, Durability::Strict));

    aterm_uds::private_file::write_private(&p.root, HAND_RECORD, b"").expect("by hand");
    let mut out = Out::kept();
    off_record_step(&p, &mut out);
    assert!(!out.failed, "{}", out.said());
    assert_eq!(bus_hosting(&p), BusHosting::Off, "off ahead of by hand");
    std::fs::remove_file(p.root.join(OFF_RECORD)).expect("the root's off record");
    assert_eq!(
        bus_hosting(&p),
        BusHosting::Off,
        "the off record beside aterm.toml"
    );
}

/// `on`'S NODE STEP RECORDS UNDER THE BRIDGE LOCK (§11 C3b): while a bridge holds the state
/// dir, as a bridge of the old node does after a key rotation until aterm relaunches it, the
/// step leaves `node` naming that node, so the move of its unread mail stays shut and that
/// bridge goes on owning its group alone; once the lock is free, the same step records the
/// node the key names (the control).
#[test]
fn the_mesh_node_step_names_no_node_under_a_running_bridge() {
    let dir = crate::test_scratch_dir("en");
    let keys = dir.path().join("keys");
    aterm_uds::private_file::ensure_private_dir(&keys).expect("a private dir");
    let text = format!("{}\n", aterm_codec::hex::encode_lower(&[7u8; 32]));
    aterm_uds::private_file::write_private(&keys, crate::federate::NODE_KEY_FILE, text.as_bytes())
        .expect("the key");
    let node = crate::federate::node_of_key(&keys)
        .expect("a key")
        .expect("a node");
    let state = dir.path().join(crate::federate::BRIDGE_STATE_DIR);
    let old = "n-0123456789abcdef";
    let dir_of = crate::state::StateDir::open(&state).expect("the state dir");
    dir_of.name_node(old).expect("the old node");

    let bridge = dir_of.lock_bridge().expect("a bridge holds the dir");
    let mut out = Out::kept();
    let named = mesh_node_step(&keys, &state, read_node(&state), false, &mut out);
    assert_eq!(named.ok().as_deref(), Some(node.as_str()), "{}", out.said());
    assert!(!out.failed, "{}", out.said());
    assert!(
        out.said().contains("held by another bridge"),
        "{}",
        out.said()
    );
    assert_eq!(
        read_node(&state).as_deref(),
        Some(old),
        "the running bridge's node stays: {}",
        out.said()
    );

    drop(bridge);
    let mut out = Out::kept();
    let named = mesh_node_step(&keys, &state, read_node(&state), false, &mut out);
    assert_eq!(named.ok().as_deref(), Some(node.as_str()), "{}", out.said());
    assert_eq!(
        read_node(&state).as_deref(),
        Some(node.as_str()),
        "with no bridge running the key's node is recorded: {}",
        out.said()
    );
}

thread_local! {
    /// What [`between_read_and_write`] runs next on this thread, once.
    static BETWEEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// The seam `config_edit` calls between its read of aterm.toml and its locked write: it runs
/// what [`between`] set on this thread, once.
pub(super) fn between_read_and_write() {
    if let Some(run) = BETWEEN.with(|between| between.borrow_mut().take()) {
        run();
    }
}

/// Run `writer` the next time `config_edit` on this thread has read aterm.toml and not yet
/// written it: another writer, a Settings save say, in that window.
fn between(writer: impl FnOnce() + 'static) {
    BETWEEN.with(|between| *between.borrow_mut() = Some(Box::new(writer)));
}
