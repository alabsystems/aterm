// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Actual native startup/custody. These echo fixtures still mint no SDK member.
use super::*;
use aterm_uds::spawnfd::{BirthAttempt, SpawnFailure};

fn starter(fixture: &Fixture) -> worker::Starter {
    fixture.starter.clone()
}

#[test]
fn required_backend_control_refuses_without_a_native_watcher_before_birth() {
    assert!(aterm_uds::tether::executable().is_none());
    let mut fixture = Fixture::new(true);
    let admitted = worker::Admitted::assumed(starter(&fixture));
    let attempt = BirthAttempt::new().unwrap();
    let key = attempt.key().clone();
    let failure = fixture
        .reservation
        .take()
        .unwrap()
        .spawn_with_required_control(admitted, fixture.home.path(), attempt)
        .unwrap_err();
    match failure.outcome {
        SpawnFailure::NotSpawned(receipt) => {
            assert_eq!(receipt.key(), &key);
            assert_eq!(receipt.error().kind(), std::io::ErrorKind::Unsupported);
        }
        SpawnFailure::Unbound { .. } => panic!("missing watcher started a backend"),
        SpawnFailure::Refused(_) => panic!("current birth attempt was refused"),
    }
    assert!(fixture.constructor_state().is_err());
    assert!(fixture.child.child.try_wait().unwrap().is_none());
}

#[test]
fn owned_start_binds_real_birth_and_drop_reaps_its_cohort() {
    let mut fixture = Fixture::new(true);
    let admitted = worker::Admitted::assumed(starter(&fixture));
    let attempt = BirthAttempt::new().unwrap();
    let owner = fixture.reservation.take().unwrap();
    let mut running = owner.spawn(admitted, fixture.home.path(), attempt).unwrap();
    assert_eq!(running.check_control_binding(), Err(Error::Unsupported));
    let process = running.process().unwrap();
    assert_ne!(process.pid, fixture.child.child.id());
    assert_eq!(process.group, process.pid);
    assert_eq!(process.parent, std::process::id());
    assert!(fixture.constructor_state().is_ok());
    let args = atpkg::caller_shell::process_args(process.pid).unwrap();
    assert_eq!(
        args.env_var("ALAB_OWNED_SOURCE_ONLY"),
        Some("source-tab-setting")
    );
    assert!(
        std::env::var_os("PATH").is_some(),
        "the launcher has its own PATH"
    );
    assert!(
        args.env_var("PATH").is_none(),
        "the GUI's ambient PATH must not leak into this source environment"
    );
    #[cfg(target_os = "macos")]
    {
        let bytes = aterm_uds::procargs::read(process.pid).unwrap();
        let parsed = aterm_uds::procargs::parse(&bytes).unwrap();
        assert!(parsed.env().any(|word| word == b"ALAB_OWNED_RAW=raw-\xff"));
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let bytes = aterm_uds::process::read_environ(process.pid).unwrap();
        assert!(
            bytes
                .split(|byte| *byte == 0)
                .any(|word| word == b"ALAB_OWNED_RAW=raw-\xff")
        );
    }
    assert_eq!(
        args.env_var("ATERM_SESSION_ID"),
        Some("s-owned-constructor-fixture")
    );
    assert_eq!(
        args.env_var("ATERM_PARENT_SESSION_ID"),
        Some("s-owned-constructor-fixture")
    );
    assert!(fixture.registry.state.lock().unwrap().members.is_empty());
    assert_eq!(running.check_sdk_construction(), Err(Error::Busy));
    // Even actual output from the owned child cannot substitute for SDK
    // construction through the authenticated native control lane.
    let banner = b"codex app-server (WebSockets)\n  listening on: ws://127.0.0.1:43210\n";
    let mut input = running.take_stdin().unwrap();
    std::io::Write::write_all(&mut input, banner).unwrap();
    let mut output = vec![0; banner.len()];
    std::io::Read::read_exact(&mut running.take_stdout().unwrap(), &mut output).unwrap();
    assert_eq!(output, banner);
    assert_eq!(running.check_sdk_construction(), Err(Error::Busy));
    drop(running);
    assert!(Process::observe(process.pid).is_none_or(|now| now.birth != process.birth));
    fixture.registry.reap_gone();
    assert!(
        !fixture
            .registry
            .state
            .lock()
            .unwrap()
            .launches
            .contains_key(&fixture.token)
    );
}

#[test]
fn failed_service_transfer_keeps_the_actual_child_for_settlement() {
    let mut fixture = Fixture::new(true);
    let admitted = worker::Admitted::assumed(starter(&fixture));
    let mut running = fixture
        .reservation
        .take()
        .unwrap()
        .spawn(admitted, fixture.home.path(), BirthAttempt::new().unwrap())
        .unwrap();
    let original = running.process().unwrap();
    let mut failure = match running.into_service() {
        Ok(_) => panic!("a child without a control socket cannot become a retained service"),
        Err(failure) => failure,
    };
    assert_eq!(failure.reason, Error::Unsupported);
    assert_eq!(failure.worker.process().unwrap(), original);
    assert!(fixture.child.child.owns_unreaped());
    failure.worker.stop().unwrap();
    assert!(Process::observe(original.pid).is_none_or(|now| now.birth != original.birth));
}

#[test]
fn cancelled_start_retains_genuine_no_child_receipt_for_its_exact_attempt() {
    let mut fixture = Fixture::new(true);
    let admitted = worker::Admitted::assumed(starter(&fixture));
    let attempt = BirthAttempt::new().unwrap();
    let key = attempt.key().clone();
    fixture.scope.cancellation().cancel();
    let failure = fixture
        .reservation
        .take()
        .unwrap()
        .spawn(admitted, fixture.home.path(), attempt)
        .unwrap_err();
    assert_eq!(failure.reason, Error::Stale);
    let proof = failure.outcome.into_not_spawned().unwrap();
    assert_eq!(proof.key(), &key);
    assert!(
        fixture.child.child.owns_unreaped(),
        "an unrelated actual child is unaffected"
    );
    assert!(
        fixture.registry.state.lock().unwrap().launches[&fixture.token]
            .process
            .is_none()
    );
    assert!(matches!(fixture.constructor_state(), Err(Error::Denied)));
}

#[test]
fn exited_environment_producer_refuses_birth_with_the_original_no_child_receipt() {
    let mut fixture = Fixture::new(true);
    let admitted = worker::Admitted::assumed(starter(&fixture));
    let attempt = BirthAttempt::new().unwrap();
    let key = attempt.key().clone();
    fixture.child.stop();
    let failure = fixture
        .reservation
        .take()
        .unwrap()
        .spawn(admitted, fixture.home.path(), attempt)
        .unwrap_err();
    assert_eq!(failure.reason, Error::Stale);
    assert_eq!(failure.outcome.into_not_spawned().unwrap().key(), &key);
}

#[test]
fn custody_transfer_keeps_original_child_and_attempt_until_actual_stop() {
    let mut fixture = Fixture::new(true);
    let admitted = worker::Admitted::assumed(starter(&fixture));
    let attempt = BirthAttempt::new().unwrap();
    let key = attempt.key().clone();
    let running = fixture
        .reservation
        .take()
        .unwrap()
        .spawn(admitted, fixture.home.path(), attempt)
        .unwrap();
    let mut actual = running.into_spawned().unwrap();
    assert_eq!(actual.key(), &key);
    assert!(actual.is_original_child());
    let input = aterm_uds::deadline::cat_echoes(actual.child_mut());
    assert!(fixture.constructor_state().is_ok());
    assert!(fixture.registry.state.lock().unwrap().members.is_empty());
    actual.kill_group().unwrap();
    actual.child_mut().wait().unwrap();
    drop(input);
    fixture.registry.reap_gone();
    assert!(
        !fixture
            .registry
            .state
            .lock()
            .unwrap()
            .launches
            .contains_key(&fixture.token)
    );
}

#[test]
fn native_spawn_failure_preserves_the_exact_no_child_attempt() {
    let mut fixture = Fixture::new(true);
    let admitted = worker::Admitted::assumed(starter(&fixture));
    let attempt = BirthAttempt::new().unwrap();
    let key = attempt.key().clone();
    let failure = fixture
        .reservation
        .take()
        .unwrap()
        .spawn(admitted, &fixture.home.path().join("absent-cwd"), attempt)
        .unwrap_err();
    match failure.outcome {
        SpawnFailure::NotSpawned(proof) => assert_eq!(proof.key(), &key),
        other => panic!("expected actual native no-child outcome, got {other:?}"),
    }
    assert!(matches!(fixture.constructor_state(), Err(Error::Denied)));
}

#[cfg(target_os = "macos")]
#[test]
fn executed_foreign_command_cannot_bind_the_constructor_and_keeps_actual_custody() {
    let mut fixture = Fixture::new(true);
    let owner = fixture.reservation.take().unwrap();
    let proof = fixture.home.path().join("executed");
    let shell = aterm_uds::deadline::stand_in(std::path::Path::new("/bin/sh"));
    let mut command = aterm_uds::spawnfd::command(shell);
    command
        .args([
            "-c",
            "printf 'executed retained image\\n' > executed; exec /bin/cat",
        ])
        .current_dir(fixture.home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let attempt = BirthAttempt::new().unwrap();
    let key = attempt.key().clone();
    let mut spawned = command.spawn_cohort(attempt).unwrap();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !std::fs::read_to_string(&proof).is_ok_and(|text| text == "executed retained image\n") {
        assert!(
            std::time::Instant::now() < until,
            "execution proof hang detector"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    // The shell's file precedes its exec. A reply from cat proves that exec
    // completed before the constructor reads the child's process facts.
    let input = aterm_uds::deadline::cat_echoes(spawned.child_mut());
    // Actual target execution is not admitted constructor custody. This raw
    // result also cannot supply the sealed path's private InstalledSpawn.
    assert_eq!(owner.bind(spawned.child_mut()), Err(Error::Denied));
    drop(owner);
    let actual_key = spawned.key().clone();
    let original = spawned.is_original_child();
    spawned.kill_group().unwrap();
    spawned.child_mut().wait().unwrap();
    drop(input);
    assert_eq!(actual_key, key);
    assert!(original);
    assert!(matches!(fixture.constructor_state(), Err(Error::Denied)));
    assert!(fixture.registry.state.lock().unwrap().members.is_empty());
}

#[test]
fn reaped_leader_cannot_acknowledge_a_group_with_an_unreaped_member() {
    struct Companion(aterm_uds::spawnfd::Child);
    impl Drop for Companion {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut fixture = Fixture::new(true);
    let admitted = worker::Admitted::assumed(starter(&fixture));
    let mut running = fixture
        .reservation
        .take()
        .unwrap()
        .spawn(admitted, fixture.home.path(), BirthAttempt::new().unwrap())
        .unwrap();
    let group = running.process().unwrap().pid;
    let program = aterm_uds::deadline::stand_in(std::path::Path::new("/bin/cat"));
    let mut command = aterm_uds::spawnfd::command(program);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // This actual second child pins the original group until its owning reap.
    // Its guard drops before running on assertion failure, avoiding a test leak.
    let mut companion = Companion(
        command
            .spawn_in_process_group(i32::try_from(group).unwrap())
            .unwrap(),
    );
    let input = aterm_uds::deadline::cat_echoes(&mut companion.0);
    assert_eq!(Process::observe(companion.0.id()).unwrap().group, group);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        assert_eq!(
            running.stop().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        if aterm_uds::process::process_presence(i32::try_from(group).unwrap())
            == aterm_uds::process::ProcessPresence::Absent
        {
            break;
        }
        assert!(
            std::time::Instant::now() < until,
            "original leader reap hang detector"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_ne!(
        aterm_uds::process::group_presence(group),
        aterm_uds::process::ProcessPresence::Absent
    );
    assert_eq!(
        running.stop().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "pending group cannot discard the actual receipt",
    );
    companion.0.wait().unwrap();
    loop {
        match running.stop() {
            Ok(()) => break,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("cohort retirement failed: {error}"),
        }
        assert!(
            std::time::Instant::now() < until,
            "kernel group absence hang detector"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(running.process(), Err(Error::Stale));
    running.stop().unwrap();
    assert_eq!(
        aterm_uds::process::group_presence(group),
        aterm_uds::process::ProcessPresence::Absent
    );
    drop(input);
}

#[test]
fn private_control_socket_reaches_only_the_bound_child_without_sdk_authority() {
    let mut fixture = Fixture::new(true);
    let admitted = worker::Admitted::assumed(starter(&fixture));
    let (mut running, mut control) = fixture
        .reservation
        .take()
        .unwrap()
        .spawn_with_control(admitted, fixture.home.path(), BirthAttempt::new().unwrap())
        .unwrap();
    let process = running.process().unwrap();
    assert_eq!(process.parent, std::process::id());
    assert_eq!(process.group, process.pid);
    assert!(
        running.take_stdin().is_none(),
        "the socket replaced the input pipe"
    );
    let mut output = running.take_stdout().unwrap();
    control
        .set_write_timeout(Some(std::time::Duration::from_secs(60)))
        .unwrap();
    let bytes = b"native-owned-control-endpoint\n";
    std::io::Write::write_all(&mut control, bytes).unwrap();
    let (sent, received) = std::sync::mpsc::channel();
    let length = bytes.len();
    std::thread::spawn(move || {
        let mut echoed = vec![0; length];
        let result = std::io::Read::read_exact(&mut output, &mut echoed).map(|()| echoed);
        let _ = sent.send(result);
    });
    assert_eq!(
        received
            .recv_timeout(std::time::Duration::from_secs(60))
            .unwrap()
            .unwrap(),
        bytes,
    );
    assert_eq!(running.check_sdk_construction(), Err(Error::Busy));
    assert!(fixture.registry.state.lock().unwrap().members.is_empty());
    drop(control);
    drop(running);
    assert!(Process::observe(process.pid).is_none_or(|now| now.birth != process.birth));
}

#[test]
fn control_challenge_is_tied_to_actual_owned_child_and_constructor_connection() {
    let mut fixture = Fixture::new(true);
    let admitted = worker::Admitted::assumed(starter(&fixture));
    let (mut running, _control) = fixture
        .reservation
        .take()
        .unwrap()
        .spawn_with_control(admitted, fixture.home.path(), BirthAttempt::new().unwrap())
        .unwrap();
    assert_eq!(running.check_control_binding(), Err(Error::Busy));
    let process = running.process().unwrap();
    // This fixture supplies the constructor front-door principal for the real
    // bound cat child. It creates no native reader or SDK readiness receipt.
    let peer = Peer {
        caller: slot::Caller {
            sid: "s-owned-constructor-fixture".into(),
            pid: process.pid,
            birth: process.birth,
        },
        process,
        launch: Some(fixture.token.clone()),
    };
    let connection = fixture.registry.fixture_connection();
    let foreign = fixture.registry.fixture_connection();
    let controller_operation = "a".repeat(32);
    let member = fixture
        .registry
        .open_frontend_controller(&peer, &connection, &controller_operation, &fixture.scope)
        .unwrap()
        .member;
    let operation = "b".repeat(32);
    assert_eq!(
        fixture.registry.begin_owned_control(
            &peer,
            &foreign,
            &member,
            &controller_operation,
            &operation
        ),
        Err(Error::Stale)
    );
    fixture
        .registry
        .begin_owned_control(
            &peer,
            &connection,
            &member,
            &controller_operation,
            &operation,
        )
        .unwrap();
    assert_eq!(running.check_control_binding(), Err(Error::Busy));
    let mut output = running.take_stdout().unwrap();
    let (sent, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = [0; 40];
        let result = std::io::Read::read_exact(&mut output, &mut bytes).map(|()| bytes);
        let _ = sent.send(result);
    });
    let bytes = received
        .recv_timeout(std::time::Duration::from_secs(60))
        .unwrap()
        .unwrap();
    assert_eq!(&bytes[..8], b"ALABCTL1");
    let nonce = std::str::from_utf8(&bytes[8..]).unwrap();
    fixture
        .registry
        .acknowledge_owned_control(
            &peer,
            &connection,
            &member,
            &controller_operation,
            &operation,
            nonce,
        )
        .unwrap();
    fixture
        .registry
        .acknowledge_owned_control(
            &peer,
            &connection,
            &member,
            &controller_operation,
            &operation,
            nonce,
        )
        .unwrap();
    running.check_control_binding().unwrap();
    assert_eq!(running.check_sdk_construction(), Err(Error::Busy));
    assert!(
        fixture.registry.state.lock().unwrap().actors.is_empty(),
        "physical process control must not allocate a credential actor"
    );
    let source_connection = fixture.registry.fixture_connection();
    let source_operation = "d".repeat(32);
    let source_member = fixture
        .registry
        .open_member(&peer, &source_connection, &source_operation)
        .unwrap()
        .member;
    assert!(
        fixture
            .registry
            .begin_owned_control(
                &peer,
                &source_connection,
                &source_member,
                &source_operation,
                &operation
            )
            .is_err(),
        "a credential member cannot substitute for an independent controller"
    );

    // Control allocation alone is not foreground permission, even for this
    // actual backend child with a successfully bound physical control socket.
    let second_controller_operation = "c".repeat(32);
    let controller_id = fixture
        .registry
        .open_frontend_controller(
            &peer,
            &connection,
            &second_controller_operation,
            &fixture.scope,
        )
        .unwrap()
        .member;
    let controller = fixture
        .registry
        .frontend_controller(
            &peer,
            &connection,
            &controller_id,
            &second_controller_operation,
        )
        .unwrap();
    let no_pty = std::fs::File::open("/dev/null").unwrap();
    use std::os::fd::AsFd;
    assert!(matches!(
        controller.foreground(no_pty.as_fd()),
        Err(Error::Denied)
    ));
    assert!(fixture.registry.state.lock().unwrap().members.is_empty());
    let mut service = running.into_service().unwrap();
    fixture.registry.connection_closed(connection.id);
    assert_eq!(
        fixture.registry.acknowledge_owned_control(
            &peer,
            &connection,
            &member,
            &controller_operation,
            &operation,
            nonce
        ),
        Err(Error::Stale)
    );
    service.stop().unwrap();
    drop(service);
    assert!(!peer.current());
}
