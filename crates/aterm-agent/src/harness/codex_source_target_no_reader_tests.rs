// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::codex_source_registry::FixtureConnection;
struct Fixture {
    registry: Registry,
    app: crate::harness::account_work::Lifetime,
    caller: slot::Caller,
    connection: FixtureConnection,
    scope: Option<crate::harness::account_work::Scope>,
}
impl Fixture {
    fn new() -> Self {
        Self::for_process(Process::observe(std::process::id()).unwrap())
    }
    fn for_process(process: Process) -> Self {
        // Explicit completed-NoReader receipt injection only, not an
        // InstalledTarget/issuer/consumer/source constructor qualification.
        let app = crate::harness::account_work::Lifetime::default();
        let scope = app.enter().unwrap();
        let registry = Registry::for_app(&app);
        let connection = registry.fixture_connection();
        let id = connection.id;
        let caller = slot::Caller {
            sid: format!("s-no-reader-{id}"),
            pid: process.pid,
            birth: process.birth,
        };
        let member = "f".repeat(32);
        let operation = "1".repeat(32);
        let mut state = registry.state.lock().unwrap();
        state.actors.insert(
            actor_key(&caller),
            Actor {
                process: process.clone(),
                epoch: "e".repeat(16),
                highwater: 1,
                issued: [(
                    member.clone(),
                    Issued {
                        operation: operation.clone(),
                        connection: connection.clone(),
                        parent_use: None,
                        target: Some(Weak::new()),
                    },
                )]
                .into(),
            },
        );
        state.unrelated.insert(
            member_key(&caller, &member).unwrap(),
            UnrelatedMember {
                process: process.clone(),
                operation: operation.clone(),
                registration: "5".repeat(32),
                connection: connection.clone(),
                kind: NoNativeReaderKind::ApiKey,
            },
        );
        state.target_opens.insert(
            open_key(&caller, &operation),
            OpenRecord {
                caller: caller.clone(),
                process,
                context: "4".repeat(64),
                assignment: "3".repeat(32),
                member,
                parent_use: None,
                validated: true,
                preloads: Default::default(),
                latest_preload: None,
                cleanup: None,
                closed: None,
                member_retirement: None,
                member_retired: false,
                no_reader: Some(Completion {
                    body: Body {
                        kind: NoNativeReaderKind::ApiKey,
                        registration: "5".repeat(32),
                    },
                    retirement: scope.cancellation(),
                }),
                cleanup_turn: None,
            },
        );
        drop(state);
        Self {
            registry,
            app,
            caller,
            connection,
            scope: Some(scope),
        }
    }
    fn cleanup(&self) -> Result<TargetOpenCleanup, Error> {
        self.registry.prepare_target_open_retirement(
            &self.caller,
            &self.connection,
            TargetOpenRetirement {
                operation: &"2".repeat(32),
                open_operation: &"1".repeat(32),
                assignment: &"3".repeat(32),
                context: &"4".repeat(64),
                member: Some(&"f".repeat(32)),
            },
        )
    }
}
#[test]
fn no_reader_open_close_waits_actual_constructor_retirement_and_counts_paused_cleanup() {
    let mut f = Fixture::new();
    assert!(matches!(f.cleanup(), Err(Error::Busy)));
    drop(f.scope.take());
    let pause = f.app.pause().unwrap();
    let cleanup = f.cleanup().unwrap();
    let pause = match pause.try_wait() {
        Err(pause) => pause,
        Ok(_) => panic!("accepted cleanup remains counted"),
    };
    f.registry.retire_target_open(cleanup).unwrap();
    assert!(pause.try_wait().is_ok());
    assert!(
        !f.registry
            .state
            .lock()
            .unwrap()
            .unrelated
            .contains_key(&member_key(&f.caller, &"f".repeat(32)).unwrap())
    );
    // Exact known Closed correlation after Drained creates no new turn.
    f.registry.retire_target_open(f.cleanup().unwrap()).unwrap();
}
#[test]
fn no_reader_completion_cannot_turn_unknown_open_into_closed_or_change_context() {
    let mut f = Fixture::new();
    drop(f.scope.take());
    assert!(
        f.registry
            .prepare_target_open_retirement(
                &f.caller,
                &f.connection,
                TargetOpenRetirement {
                    operation: &"2".repeat(32),
                    open_operation: &"1".repeat(32),
                    assignment: &"3".repeat(32),
                    context: &"6".repeat(64),
                    member: Some(&"f".repeat(32))
                }
            )
            .is_err()
    );
    assert!(
        f.registry
            .prepare_target_open_retirement(
                &f.caller,
                &f.connection,
                TargetOpenRetirement {
                    operation: &"2".repeat(32),
                    open_operation: &"a".repeat(32),
                    assignment: &"3".repeat(32),
                    context: &"4".repeat(64),
                    member: Some(&"f".repeat(32))
                }
            )
            .is_err()
    );
    let pause = f.app.pause().unwrap();
    assert!(pause.try_wait().is_ok());
    assert!(
        matches!(f.cleanup(), Err(Error::Busy)),
        "an unrecorded cleanup cannot mutate after Drained"
    );
}
#[test]
fn real_caller_death_releases_unknown_no_reader_cleanup_turn() {
    struct Child(aterm_uds::spawnfd::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Child(
        aterm_uds::spawnfd::command("/bin/sleep")
            .arg("120")
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let process = loop {
        if let Some(process) = Process::observe(child.0.id())
            && process.image.path.ends_with(b"/sleep")
        {
            break process;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "child reached its own executable"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let mut f = Fixture::for_process(process);
    drop(f.scope.take());
    let pause = f.app.pause().unwrap();
    drop(f.cleanup().unwrap()); // Lost reply does not discard native count.
    let pause = match pause.try_wait() {
        Err(pause) => pause,
        Ok(_) => panic!("unknown cleanup was uncounted"),
    };
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    f.registry.reap_gone();
    assert!(pause.try_wait().is_ok());
    let state = f.registry.state.lock().unwrap();
    assert!(!state.actors.contains_key(&actor_key(&f.caller)));
    assert!(
        !state
            .target_opens
            .contains_key(&open_key(&f.caller, &"1".repeat(32)))
    );
}
