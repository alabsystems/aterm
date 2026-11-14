// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kernel constructor custody only. The fixtures insert no SDK member, Source,
//! Ready receipt or provider permission and do not bypass image admission in production.
use super::*;
use crate::harness::account_work::{Lifetime, Scope};
use aterm_uds::spawnfd::{Child, Stdio};
use std::os::unix::ffi::OsStringExt as _;

#[path = "codex_source_owned_spawn_tests.rs"]
mod spawn_tests;

struct Echo {
    child: Child,
    _input: std::process::ChildStdin,
}
impl Echo {
    fn spawn(home: &std::path::Path, token: &str, dedicated: bool) -> Self {
        Self::spawn_with(home, token, dedicated, |_| {})
    }
    fn spawn_with(
        home: &std::path::Path,
        token: &str,
        dedicated: bool,
        configure: impl FnOnce(&mut aterm_uds::spawnfd::Command),
    ) -> Self {
        let program = aterm_uds::deadline::stand_in(std::path::Path::new("/bin/cat"));
        let mut command = aterm_uds::spawnfd::command(program);
        command
            .env_clear()
            .env(LAUNCH_ENV, token)
            .env("CODEX_HOME", home)
            .env("ATERM_SESSION_ID", "s-owned-constructor-fixture")
            .env("ATERM_PARENT_SESSION_ID", "s-owned-constructor-fixture")
            .env("ALAB_OWNED_SOURCE_ONLY", "source-tab-setting")
            .env(
                "ALAB_OWNED_RAW",
                std::ffi::OsString::from_vec(b"raw-\xff".to_vec()),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        configure(&mut command);
        let mut child = if dedicated {
            command.spawn_in_dedicated_process_group()
        } else {
            command.spawn()
        }
        .unwrap();
        let input = aterm_uds::deadline::cat_echoes(&mut child);
        Self {
            child,
            _input: input,
        }
    }
    fn stop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
impl Drop for Echo {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Fixture {
    registry: Registry,
    reservation: Option<OwnedLaunchReservation>,
    child: Echo,
    home: aterm_tempfile::TempDir,
    scope: Scope,
    _lifetime: Lifetime,
    token: String,
    starter: worker::Starter,
}
impl Fixture {
    fn new(dedicated: bool) -> Self {
        let home = aterm_tempfile::tempdir().unwrap();
        let token = aterm_uds::rand::hex_token::<16>().unwrap();
        let child = Echo::spawn(home.path(), &token, dedicated);
        let process = Process::observe(child.child.id()).unwrap();
        let environment = OwnedLaunchEnvironment::capture(&process).unwrap();
        let starter = environment
            .prepare(worker::Starter {
                exe: std::ffi::OsString::from_vec(process.image.path.clone()).into(),
                env: vec![
                    (LAUNCH_ENV.into(), token.clone()),
                    ("CODEX_HOME".into(), home.path().to_str().unwrap().into()),
                ],
                args: Ok(Vec::new()),
                unset: vec!["CODEX_AUTH_PATH"],
            })
            .unwrap();
        let environment = environment.admit(&starter).unwrap();
        let lifetime = Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let binding = Arc::new(Binding::default());
        let registry = Registry::default();
        registry.state.lock().unwrap().launches.insert(
            token.clone(),
            Launch {
                sid: "s-owned-constructor-fixture".into(),
                image: process.image,
                expected_env: vec![("CODEX_HOME".into(), home.path().to_str().unwrap().into())],
                expected_unset: vec!["CODEX_AUTH_PATH"],
                process: None,
                binding: LaunchBinding::Native(binding.clone()),
                typed: false,
                gate: Some(Arc::new(SourceWorkLease::for_home(home.path()).unwrap())),
                _scope: Some(scope.clone()),
                initial: None,
                native_restart: None,
            },
        );
        let inner = LaunchReservation {
            token: token.clone(),
            executable: std::env::current_exe().unwrap(),
            state: Arc::downgrade(&registry.state),
        };
        Self {
            registry,
            reservation: Some(OwnedLaunchReservation::new(inner, binding, environment)),
            child,
            home,
            scope,
            _lifetime: lifetime,
            token,
            starter,
        }
    }
    fn peer(&self) -> Result<Peer, Error> {
        let process = Process::observe(self.child.child.id()).unwrap();
        self.registry.reserved_peer(
            &slot::Caller {
                sid: "s-owned-constructor-fixture".into(),
                pid: process.pid,
                birth: process.birth,
            },
            &self.token,
        )
    }
    fn owner(&self) -> &OwnedLaunchReservation {
        self.reservation.as_ref().unwrap()
    }
    fn constructor_state(&self) -> Result<(), Error> {
        self.registry
            .state
            .lock()
            .unwrap()
            .launches
            .get(&self.token)
            .ok_or(Error::Denied)?
            .binding
            .entry()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.child.stop();
        self.reservation.take();
        self.registry.reap_gone();
    }
}

#[test]
fn consumer_cannot_claim_before_its_owned_child_is_bound() {
    let mut fixture = Fixture::new(true);
    assert!(matches!(fixture.constructor_state(), Err(Error::Busy)));
    assert!(matches!(fixture.peer(), Err(Error::Unsupported)));
    assert!(
        fixture.registry.state.lock().unwrap().launches[&fixture.token]
            .process
            .is_none()
    );
    fixture.owner().bind(&fixture.child.child).unwrap();
    fixture.owner().bind(&fixture.child.child).unwrap();
    assert!(fixture.constructor_state().is_ok());
    assert_eq!(
        fixture.registry.state.lock().unwrap().launches[&fixture.token]
            .process
            .as_ref(),
        Process::observe(fixture.child.child.id()).as_ref()
    );
    assert!(
        matches!(fixture.peer(), Err(Error::Unsupported)),
        "kernel bind cannot make an echo process an admitted SDK consumer"
    );
    assert!(fixture.registry.state.lock().unwrap().members.is_empty());
    drop(fixture.reservation.take());
    fixture.registry.reap_gone();
    assert!(
        fixture.constructor_state().is_ok(),
        "losing the launcher value cannot revoke a bound SDK actor"
    );
    assert!(
        crate::harness::codex_source_gate::PublicationLock::for_home(fixture.home.path()).is_err()
    );
    fixture.child.stop();
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
    assert!(
        crate::harness::codex_source_gate::PublicationLock::for_home(fixture.home.path()).is_ok()
    );
}

#[test]
fn foreign_child_wrong_group_and_reaped_custody_cannot_bind() {
    let mut fixture = Fixture::new(true);
    let foreign = Echo::spawn(fixture.home.path(), "another-constructor", true);
    assert!(matches!(
        fixture.owner().bind(&foreign.child),
        Err(Error::Denied)
    ));
    assert!(matches!(fixture.constructor_state(), Err(Error::Busy)));
    fixture.child.stop();
    assert!(matches!(
        fixture.owner().bind(&fixture.child.child),
        Err(Error::Stale)
    ));
    let inherited = Fixture::new(false);
    assert!(matches!(
        inherited.owner().bind(&inherited.child.child),
        Err(Error::Denied)
    ));
    assert!(matches!(inherited.constructor_state(), Err(Error::Busy)));
}

#[test]
fn matching_constructor_token_cannot_bind_a_child_with_different_settings() {
    let fixture = Fixture::new(true);
    let altered = Echo::spawn_with(fixture.home.path(), &fixture.token, true, |command| {
        command.env("ALAB_OWNED_SOURCE_ONLY", "different-tab-setting");
    });
    assert_eq!(fixture.owner().bind(&altered.child), Err(Error::Denied));
    assert!(matches!(fixture.constructor_state(), Err(Error::Busy)));
    assert!(fixture.registry.state.lock().unwrap().members.is_empty());
}

#[test]
fn abandoned_constructor_is_revoked_without_claiming_child_death() {
    let mut fixture = Fixture::new(true);
    drop(fixture.reservation.take());
    assert!(fixture.child.child.owns_unreaped());
    assert!(matches!(fixture.constructor_state(), Err(Error::Denied)));
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
    assert!(fixture.child.child.owns_unreaped());
    assert!(
        crate::harness::codex_source_gate::PublicationLock::for_home(fixture.home.path()).is_ok()
    );
}

#[test]
fn cancelled_scope_cannot_bind_and_sweep_denies_late_constructor() {
    let fixture = Fixture::new(true);
    fixture.scope.cancellation().cancel();
    assert!(matches!(
        fixture.owner().bind(&fixture.child.child),
        Err(Error::Denied)
    ));
    assert_eq!(fixture.registry.revoke_cancelled_unclaimed().unwrap(), 1);
    assert!(matches!(fixture.constructor_state(), Err(Error::Denied)));
}
