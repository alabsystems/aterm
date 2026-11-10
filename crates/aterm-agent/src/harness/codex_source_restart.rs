// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A fresh source constructor for an actual native launcher child. A pending
//! token cannot bind before its exact host operation grants Execute.
use super::*;
use crate::harness::native_act::RestartCommand;

pub(super) struct NativeRestart {
    pub(super) child: Process,
    granted: AtomicBool,
    revoked: AtomicBool,
}
impl NativeRestart {
    pub(super) fn admits(&self, process: &Process) -> bool {
        !self.revoked.load(Ordering::Acquire)
            && self.granted.load(Ordering::Acquire)
            && self.child.pid == process.pid
            && self.child.birth == process.birth
            && self.child.parent == process.parent
            && self.child.group == process.group
            && self.child.session == process.session
    }
    pub(super) fn cancelled(&self) -> bool {
        self.revoked.load(Ordering::Acquire)
    }
}

pub struct RestartReservation {
    _launch: LaunchReservation,
    native: Arc<NativeRestart>,
    command: RestartCommand,
}
impl std::fmt::Debug for RestartReservation {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str("RestartSourceConstructor(<private>)")
    }
}
impl RestartReservation {
    pub fn command(&self) -> &RestartCommand {
        &self.command
    }
    /// The native host calls this only while publishing the exact journaled Go
    /// for this retained child and command. No reader or readiness is fabricated.
    pub fn grant(&self, child: &Process) -> Result<(), Error> {
        if self.native.child != *child || self.native.cancelled() {
            return Err(Error::Stale);
        }
        self.native.granted.store(true, Ordering::Release);
        Ok(())
    }
    pub fn matches(&self, command: &RestartCommand, child: &Process) -> bool {
        if self.native.child != *child {
            return false;
        }
        let mut original = self.command.clone();
        original.env.retain(|(name, _)| name != LAUNCH_ENV);
        original == *command
    }
}
impl Drop for RestartReservation {
    fn drop(&mut self) {
        // Synchronous revocation needs no registry/native/account lock. The
        // registry sweep disposes the ungranted gate outside its State guard.
        if !self.native.granted.load(Ordering::Acquire) {
            self.native.revoked.store(true, Ordering::Release);
        }
    }
}

impl Registry {
    /// Called off the native operation locks after actual launcher observation.
    /// This prepares constructor custody, not a native execution grant.
    pub fn reserve_native_restart(
        &self,
        env: &Env,
        sid: &str,
        command: &RestartCommand,
        child: &Process,
    ) -> Result<Option<RestartReservation>, Error> {
        use crate::harness::codex_app_login::legacy_file_reader_allowed;
        if Process::observe(child.pid).as_ref() != Some(child)
            || !sid.starts_with("s-")
            || sid.len() > 64
            || command.env.iter().any(|(name, _)| name == LAUNCH_ENV)
            || command
                .unset
                .as_ref()
                .is_some_and(|names| names.iter().any(|name| name == LAUNCH_ENV))
        {
            return Err(Error::Denied);
        }
        let executable = std::path::PathBuf::from(&command.program);
        if Image::at(&executable).as_ref() != Some(&command.image) {
            return Err(Error::Stale);
        }
        let relevant = ["CODEX_HOME", "CODEX_AUTH_PATH", "CODEX_AUTH_SLOT"];
        let starter = worker::Starter {
            exe: executable.clone(),
            env: command.env.clone(),
            args: Ok(command.argv.clone()),
            unset: relevant
                .iter()
                .copied()
                .filter(|name| {
                    command
                        .unset
                        .as_ref()
                        .is_some_and(|names| names.iter().any(|unset| unset == name))
                })
                .collect(),
        };
        let cwd = match command.cwd.as_deref() {
            Some(cwd) => std::path::PathBuf::from(cwd),
            None => aterm_sysprobe::process_cwd(child.pid).ok_or(Error::Unavailable)?,
        };
        let auth = worker::declared_codex_auth_file(&starter, env.home.as_deref(), &cwd)
            .ok_or(Error::Unavailable)?;
        if !crate::harness::codex_alab::managed_credential_capable(&executable, true)
            .map_err(|_| Error::Unavailable)?
        {
            return if legacy_file_reader_allowed(&auth) {
                Ok(None)
            } else {
                Err(Error::Unsupported)
            };
        }
        if !crate::harness::codex_alab::alab_bytes(&executable) {
            return Err(Error::Unsupported);
        }
        let scope = {
            let state = lock(&self.state)?;
            state.app_lifetime.clone().ok_or(Error::Denied)?
        }
        .enter()
        .ok_or(Error::Denied)?;
        let gate = SourceWorkLease::for_home(env.home.as_deref().ok_or(Error::Unavailable)?)
            .map_err(|_| Error::Busy)?;
        let choice = owner::capture_canonical_launch_choice(env, &auth, &gate)
            .map_err(|_| Error::Unavailable)?;
        let initial = choice.map(|choice| {
            Arc::new(InitialCell {
                choice,
                alive: AtomicBool::new(true),
                installing: AtomicBool::new(false),
                published: AtomicBool::new(false),
            })
        });
        let native = Arc::new(NativeRestart {
            child: child.clone(),
            granted: AtomicBool::new(false),
            revoked: AtomicBool::new(false),
        });
        let token = aterm_uds::rand::hex_token::<16>().map_err(|_| Error::Unavailable)?;
        let mut state = lock(&self.state)?;
        if scope.cancelled() || state.launches.len() >= MAX_LAUNCHES {
            return Err(Error::Busy);
        }
        state.launches.insert(
            token.clone(),
            Launch {
                sid: sid.into(),
                image: command.image.clone(),
                expected_env: command
                    .env
                    .iter()
                    .filter(|(name, _)| relevant.contains(&name.as_str()))
                    .cloned()
                    .collect(),
                expected_unset: starter.unset,
                process: None,
                typed: true,
                gate: Some(Arc::new(gate)),
                _scope: Some(scope),
                initial,
                native_restart: Some(native.clone()),
            },
        );
        drop(state);
        let mut command = command.clone();
        command.env.push((LAUNCH_ENV.into(), token.clone()));
        Ok(Some(RestartReservation {
            _launch: LaunchReservation {
                token,
                executable,
                state: Arc::downgrade(&self.state),
            },
            native,
            command,
        }))
    }
}

#[cfg(all(test, unix))]
mod tests {
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
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "read gate; exec /bin/cat"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let before = Process::observe(child.id()).unwrap();
        let (registry, reservation, home) = pending_child(before.clone());
        reservation.grant(&before).unwrap();
        let mut input = child.stdin.take().unwrap();
        input.write_all(b"go\n").unwrap();
        input.flush().unwrap();
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        loop {
            let current = Process::observe(child.id()).unwrap();
            if current.image != before.image {
                assert_eq!(current.birth, before.birth);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "actual child never execed"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        drop(reservation);
        registry.reap_gone();
        assert_eq!(registry.state.lock().unwrap().launches.len(), 1);
        assert!(crate::harness::codex_source_gate::PublicationLock::for_home(home.path()).is_err());
        drop(input);
        assert!(child.wait().unwrap().success());
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
}
