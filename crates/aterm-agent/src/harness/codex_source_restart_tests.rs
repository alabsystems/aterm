// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::native_act::RestartCommand;

fn pending() -> (Registry, RestartReservation, aterm_tempfile::TempDir) {
    // Constructor metadata injection only. The kernel child and shared gate
    // are real; this does not qualify an SDK image or OAuth/source owner.
    pending_child(Process::observe(std::process::id()).unwrap())
}

fn pending_child(child: Process) -> (Registry, RestartReservation, aterm_tempfile::TempDir) {
    let home = aterm_tempfile::tempdir().unwrap();
    let lifetime = crate::harness::account_work::Lifetime::default();
    let registry = Registry::for_app(&lifetime);
    let token = "a".repeat(32);
    let native = Arc::new(NativeRestart {
        child: child.clone(),
        granted: AtomicBool::new(false),
        revoked: AtomicBool::new(false),
    });
    let command = RestartCommand {
        settles: None,
        account_intent: None,
        image: child.image.clone(),
        program: std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        argv: vec!["resume".into(), "original-thread".into()],
        env: vec![(LAUNCH_ENV.into(), token.clone())],
        unset: None,
        cwd: Some(home.path().to_string_lossy().into_owned()),
        heal: None,
        dialect: crate::harness::upgrade::Dialect::Zsh,
    };
    registry.state.lock().unwrap().launches.insert(
        token.clone(),
        Launch {
            sid: "s-restart-source-fixture".into(),
            image: child.image.clone(),
            expected_env: Vec::new(),
            expected_unset: Vec::new(),
            process: None,
            typed: true,
            gate: Some(Arc::new(SourceWorkLease::for_home(home.path()).unwrap())),
            _scope: Some(lifetime.enter().unwrap()),
            initial: None,
            native_restart: Some(native.clone()),
        },
    );
    let reservation = RestartReservation {
        _launch: LaunchReservation {
            token,
            executable: std::env::current_exe().unwrap(),
            state: Arc::downgrade(&registry.state),
        },
        native,
        command,
    };
    (registry, reservation, home)
}

#[test]
fn native_constructor_requires_exact_go_child_and_keeps_immutable_recipe() {
    let (_registry, reservation, _home) = pending();
    let child = reservation.native.child.clone();
    let mut original = reservation.command.clone();
    original.env.retain(|(name, _)| name != LAUNCH_ENV);
    assert!(reservation.matches(&original, &child));
    assert!(!reservation.native.admits(&child));
    let first_delivery = reservation.command().encode().unwrap();
    assert_eq!(
        reservation.command().encode().unwrap(),
        first_delivery,
        "repeated Attach returns the same token and command"
    );
    let mut reused = child.clone();
    reused.birth += 1;
    assert!(reservation.grant(&reused).is_err());
    let mut changed = original.clone();
    changed.argv.push("invented-input".into());
    assert!(!reservation.matches(&changed, &child));
    changed = original.clone();
    changed
        .env
        .push(("CODEX_AUTH_PATH".into(), "/other/auth.json".into()));
    assert!(!reservation.matches(&changed, &child));
    reservation.grant(&child).unwrap();
    assert!(reservation.native.admits(&child));
    assert!(!reservation.native.admits(&reused));
}

#[test]
fn cancelled_ungranted_constructor_releases_gate_but_go_stays_owned_until_ready_or_death() {
    let (registry, reservation, home) = pending();
    assert!(crate::harness::codex_source_gate::PublicationLock::for_home(home.path()).is_err());
    drop(reservation);
    registry.reap_gone();
    assert!(registry.state.lock().unwrap().launches.is_empty());
    assert!(crate::harness::codex_source_gate::PublicationLock::for_home(home.path()).is_ok());

    let (registry, reservation, home) = pending();
    reservation.grant(&reservation.native.child).unwrap();
    drop(reservation);
    registry.reap_gone();
    assert_eq!(registry.state.lock().unwrap().launches.len(), 1);
    assert!(
        crate::harness::codex_source_gate::PublicationLock::for_home(home.path()).is_err(),
        "losing local operation ownership is not child death or SDK Ready"
    );
}
#[test]
fn granted_child_exec_is_not_death_and_actual_exit_releases_constructor_gate() {
    use std::io::Write as _;
    let mut child = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "read gate; exec /bin/cat"])
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .unwrap();
    let before = Process::observe(child.id()).unwrap();
    let (registry, reservation, home) = pending_child(before.clone());
    reservation.grant(&before).unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(b"go\n").unwrap();
    input.flush().unwrap();
    child.stdin = Some(input);
    // The echo proves cat runs and is waiting for more input. Reading its image
    // while exec changes the mapping can correctly return no observation.
    let input = aterm_uds::deadline::cat_echoes(&mut child);
    let current = Process::observe(child.id()).expect("the echoing cat remains alive");
    assert_ne!(current.image, before.image);
    assert_eq!(current.birth, before.birth);
    drop(reservation);
    registry.reap_gone();
    assert_eq!(registry.state.lock().unwrap().launches.len(), 1);
    assert!(crate::harness::codex_source_gate::PublicationLock::for_home(home.path()).is_err());
    drop(input);
    let mut exit = aterm_uds::deadline::ExitWait::new(&child);
    let by = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        let left = by.saturating_duration_since(std::time::Instant::now());
        assert!(!left.is_zero(), "cat exits after its input closes");
        exit.wait(left);
    };
    assert!(status.success());
    registry.reap_gone();
    assert!(registry.state.lock().unwrap().launches.is_empty());
    assert!(crate::harness::codex_source_gate::PublicationLock::for_home(home.path()).is_ok());
}

#[test]
fn actual_unrelated_ready_releases_constructor_without_claiming_child_death() {
    let (registry, reservation, home) = pending();
    let process = reservation.native.child.clone();
    reservation.grant(&process).unwrap();
    let token = reservation._launch.token.clone();
    let peer = Peer {
        caller: slot::Caller {
            sid: "s-restart-source-fixture".into(),
            pid: process.pid,
            birth: process.birth,
        },
        process: process.clone(),
        launch: Some(token.clone()),
    };
    registry
        .state
        .lock()
        .unwrap()
        .launches
        .get_mut(&token)
        .unwrap()
        .bind(process);
    let connection = registry.fixture_connection();
    let operation = "b".repeat(32);
    let member = registry
        .open_member(&peer, &connection, &operation)
        .unwrap();
    registry
        .complete_unrelated(
            peer,
            &connection,
            &member.member,
            &operation,
            NoNativeReaderKind::ApiKey,
        )
        .unwrap();
    assert!(crate::harness::codex_source_gate::PublicationLock::for_home(home.path()).is_ok());
    assert!(Process::observe(std::process::id()).is_some());
}

#[test]
fn source_exec_go_requires_registered_actor_and_explicit_retirement_after_eof() {
    // Private constructor injection is data only. Registry allocation,
    // connection closure, release-only retirement and kernel identity are real.
    let (registry, restart, _home) = pending();
    let process = restart.native.child.clone();
    let caller = slot::Caller {
        sid: "s-restart-source-fixture".into(),
        pid: process.pid,
        birth: process.birth,
    };
    let reservation = SourceExecReservation {
        restart,
        caller: caller.clone(),
    };
    assert_eq!(reservation.grant(), Err(Error::Stale));
    assert!(!reservation.restart.native.admits(&process));
    let connection = registry.fixture_connection();
    let member = registry
        .open_member(
            &Peer {
                caller: caller.clone(),
                process: process.clone(),
                launch: None,
            },
            &connection,
            &"b".repeat(32),
        )
        .unwrap();
    assert_eq!(reservation.grant(), Err(Error::Busy));
    assert!(!reservation.restart.native.admits(&process));
    drop(connection);
    registry.reap_gone();
    assert_eq!(
        reservation.grant(),
        Err(Error::Busy),
        "EOF is not a reader-retirement receipt"
    );
    let cleanup = registry.fixture_connection();
    registry
        .retire_nonpreloaded_member(&caller, &cleanup, &member.member, None)
        .unwrap();
    let original = reservation.command().encode().unwrap();
    reservation.grant().unwrap();
    reservation.grant().unwrap();
    assert_eq!(reservation.command().encode().unwrap(), original);
    assert!(reservation.restart.native.admits(&process));
    // Reopening any source member makes another Go read refuse. Actual target
    // constructor entry independently applies the same complete custody test.
    registry
        .open_member(
            &Peer {
                caller,
                process: process.clone(),
                launch: None,
            },
            &cleanup,
            &"c".repeat(32),
        )
        .unwrap();
    assert_eq!(reservation.grant(), Err(Error::Busy));
    assert!(
        reservation.restart.native.admits(&process),
        "a refused later check cannot retract an already issued Go"
    );
}

#[test]
fn source_exec_go_rejects_an_unregistered_or_replaced_native_constructor() {
    let (registry, restart, _home) = pending();
    let process = restart.native.child.clone();
    let caller = slot::Caller {
        sid: "s-restart-source-fixture".into(),
        pid: process.pid,
        birth: process.birth,
    };
    let connection = registry.fixture_connection();
    let source = Peer {
        caller: caller.clone(),
        process: process.clone(),
        launch: None,
    };
    let member = registry
        .open_member(&source, &connection, &"b".repeat(32))
        .unwrap();
    registry
        .retire_member(source, &connection, &member.member, None)
        .unwrap();
    let reservation = SourceExecReservation { restart, caller };
    let token = reservation.restart._launch.token.clone();
    registry.state.lock().unwrap().launches.remove(&token);
    assert_eq!(reservation.grant(), Err(Error::Stale));
    assert!(!reservation.restart.native.admits(&process));
}

#[test]
fn same_process_exec_requires_fresh_go_and_explicit_old_member_retirement() {
    use std::io::Write as _;
    for case in 0..3 {
        // Constructor metadata is private fixture data, not signed package or
        // account admission. The process, exec, environment and EOF are real.
        // macOS /bin/sh may enter its selected shell image. Use the concrete
        // shell so this fixture can require an exact stable source mapping.
        let shell = if cfg!(target_os = "macos") {
            "/bin/bash"
        } else {
            "/bin/sh"
        };
        let mut child = aterm_uds::spawnfd::command(shell)
            .args(["-c", "read gate; exec /bin/cat"])
            .env(LAUNCH_ENV, "a".repeat(32))
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .spawn()
            .unwrap();
        let source_image = Image::at(std::path::Path::new(shell)).unwrap();
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let before = loop {
            if let Some(process) = Process::observe(child.id())
                && process.image == source_image
            {
                break process;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "source shell must enter its image"
            );
            aterm_uds::deadline::poll_yield();
        };
        let (registry, mut reservation, _home) = pending_child(before.clone());
        let caller = slot::Caller {
            sid: "s-restart-source-fixture".into(),
            pid: before.pid,
            birth: before.birth,
        };
        let source = Peer {
            caller: caller.clone(),
            process: before.clone(),
            launch: None,
        };
        let original_connection = registry.fixture_connection();
        let old = registry
            .open_member(&source, &original_connection, &"b".repeat(32))
            .unwrap_or_else(|error| {
                panic!(
                    "case {case}: source OPEN {error:?}, expected {:?}, actual {:?}, connection {}",
                    source.process,
                    Process::observe(source.caller.pid),
                    original_connection.is_live()
                )
            });
        if case != 1 {
            registry
                .retire_member(source, &original_connection, &old.member, None)
                .unwrap();
        }
        let token = reservation._launch.token.clone();
        let target_image = Image::at(std::path::Path::new("/bin/cat")).unwrap();
        reservation.command.image = target_image.clone();
        reservation.command.program = "/bin/cat".into();
        registry
            .state
            .lock()
            .unwrap()
            .launches
            .get_mut(&token)
            .unwrap()
            .image = target_image;
        if case != 2 {
            reservation.grant(&before).unwrap();
        }
        child.stdin.as_mut().unwrap().write_all(b"go\n").unwrap();
        child.stdin.as_mut().unwrap().flush().unwrap();
        let input = aterm_uds::deadline::cat_echoes(&mut child);
        let current = Process::observe(child.id()).unwrap();
        assert_ne!(current.image, before.image);
        assert_eq!(current.birth, before.birth);
        // EOF is not explicit SDK retirement, even after the source exec'd.
        drop(original_connection);
        let target_connection = registry.fixture_connection();
        // Inject only front-door metadata: /bin/cat is not a managed SDK
        // consumer. The real source-token/consumer-image front door is not
        // qualified by this registry test.
        registry
            .state
            .lock()
            .unwrap()
            .launches
            .get_mut(&token)
            .unwrap()
            .bind(current.clone());
        let target = Peer {
            caller: caller.clone(),
            process: current.clone(),
            launch: Some(token),
        };
        {
            let result = registry.open_member(&target, &target_connection, &"c".repeat(32));
            if case == 2 {
                assert!(matches!(result, Err(Error::Denied)));
            } else if case == 1 {
                assert!(matches!(result, Err(Error::Busy)));
                assert_eq!(
                    registry.state.lock().unwrap().actors[&actor_key(&caller)].process,
                    before
                );
            } else {
                let new = result.unwrap();
                assert_ne!(&new.member[..16], &old.member[..16]);
                assert_eq!(
                    registry
                        .open_member(&target, &target_connection, &"c".repeat(32))
                        .unwrap()
                        .member,
                    new.member
                );
                assert!(
                    registry
                        .retire_member(target, &target_connection, &old.member, None)
                        .is_err()
                );
                assert_eq!(
                    registry.state.lock().unwrap().actors[&actor_key(&caller)].process,
                    current
                );
            }
        }
        drop(input);
        let mut exit = aterm_uds::deadline::ExitWait::new(&child);
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(!left.is_zero(), "cat must exit after input closes");
            exit.wait(left);
        }
        registry.reap_gone();
    }
}
