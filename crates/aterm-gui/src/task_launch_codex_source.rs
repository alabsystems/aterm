// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Owned worker-constructor custody; never a replacement source registry.

use super::*;
use aterm_agent::harness::{
    cli::Env,
    codex_source_gate::SourceWorkLease,
    codex_source_registry::{Error, LaunchReservation, Registry},
    worker::{Admitted, WorkerCli},
};

pub(super) struct Preparation {
    registry: Arc<Registry>,
    env: Env,
    sid: String,
    scope: aterm_agent::harness::account_work::Scope,
    reservation: Option<LaunchReservation>,
    typed: bool,
    // Covers native source selection and the initial sign-in check, before an
    // admitted executable exists. Managed reservation takes a second lease.
    _selection: SourceWorkLease,
}

impl Preparation {
    pub(super) fn begin(work: &Launch) -> Result<Option<Self>, String> {
        if work.agent != aterm_phase::Program::Codex {
            return Ok(None);
        }
        let registry = work
            .source_registry
            .clone()
            .ok_or("Codex source constructor has no native registry")?;
        let scope = work
            .account_scope
            .clone()
            .ok_or("Codex source constructor has no App admission")?;
        let mut env = Env::from_process().map_err(|_| "Codex source profile is unavailable")?;
        env.config = crate::app_config::config_path();
        Self::with_context(registry, env, work.ctx.self_id.as_str().into(), scope).map(Some)
    }

    pub(super) fn with_context(
        registry: Arc<Registry>,
        env: Env,
        sid: String,
        scope: aterm_agent::harness::account_work::Scope,
    ) -> Result<Self, String> {
        let home = env
            .home
            .as_deref()
            .ok_or("Codex source profile is unavailable")?;
        let selection =
            SourceWorkLease::for_home(home).map_err(|_| "Codex source custody is unavailable")?;
        Ok(Self {
            registry,
            env,
            sid,
            scope,
            reservation: None,
            typed: false,
            _selection: selection,
        })
    }

    pub(super) fn arm(&mut self, admitted: Admitted, cli: &WorkerCli) -> Result<Admitted, String> {
        match self
            .registry
            .reserve_worker(&self.env, &self.sid, &admitted, &self.scope)
        {
            Ok(reservation) => {
                // Store before any fallible preparation: Drop revokes only an
                // unclaimed mandatory-bind permission, never a running actor.
                self.reservation = Some(reservation);
                self.reservation
                    .as_ref()
                    .ok_or("Codex constructor is unavailable")?
                    .arm(admitted, cli, self.env.home.as_deref())
                    .map_err(|_| "Codex source constructor could not be armed".into())
            }
            Err(Error::Unsupported) if legacy_source_allowed(&admitted, &self.env) => {
                // Existing unsupported consumers remain legacy launches. This
                // branch mints no member, Source0, Slot or publication proof.
                Ok(admitted)
            }
            Err(Error::Unsupported) => {
                Err("this Codex build cannot read the native-owned source".into())
            }
            Err(_) => Err("Codex source constructor is unavailable".into()),
        }
    }

    pub(super) fn check(&self, admitted: &Admitted) -> Result<(), String> {
        if self.scope.cancelled() {
            return Err("Codex account preparation was cancelled".into());
        }
        match &self.reservation {
            Some(reservation) => reservation
                .check_for_write(admitted)
                .map_err(|_| "Codex constructor changed before its launch".into()),
            None if legacy_source_allowed(admitted, &self.env) => Ok(()),
            None => Err("the legacy Codex source became native-owned before launch".into()),
        }
    }

    pub(super) fn typed(&mut self) -> Result<(), &'static str> {
        // This is called only after the main-thread endpoint observed a whole
        // or partial write. An unobserved outcome is never retried as untyped.
        self.typed = true;
        if let Some(reservation) = &self.reservation {
            reservation
                .mark_typed()
                .map_err(|_| "Codex constructor write receipt is unavailable")?;
        }
        Ok(())
    }

    /// The Rust-only joined fixture launches through spawnfd rather than the
    /// worker UI. Bind only its actual kernel child to the same reservation;
    /// this is not evidence that the normal main-thread UI wrote a command.
    #[cfg(debug_assertions)]
    pub(super) fn spawned(&mut self, pid: u32) -> Result<(), &'static str> {
        let process = aterm_agent::harness::native_act::Process::observe(pid)
            .ok_or("source proof child unavailable")?;
        let caller = aterm_agent::harness::codex_session_slot::Caller {
            sid: self.sid.clone(),
            pid,
            birth: process.birth,
        };
        let reservation = self
            .reservation
            .as_ref()
            .ok_or("source proof reservation unavailable")?;
        self.registry
            .reserved_peer(&caller, reservation.token())
            .map_err(|_| "source proof child does not match its reservation")?;
        self.typed = true;
        reservation
            .mark_typed()
            .map_err(|_| "source proof spawn receipt unavailable")
    }
}

impl Drop for Preparation {
    fn drop(&mut self) {
        if !self.typed
            && let Some(reservation) = self.reservation.take()
        {
            // Also safe after an uncertain write: SourceV1 must bind before ANY
            // credential use. If it already bound, revocation is denied and its
            // gate survives until actual Ready or kernel-proven death.
            let _ = reservation.revoke_unclaimed();
        }
    }
}

fn legacy_source_allowed(admitted: &Admitted, env: &Env) -> bool {
    let starter = admitted.starter();
    // The application strips CODEX_* before spawning this shell. Its own
    // environment is not the worker's inherited credential source.
    let selected = |name: &str| {
        starter
            .env
            .iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    // A Slot declaration is a managed-source request, even if malformed.
    if selected("CODEX_AUTH_SLOT").is_some() {
        return false;
    }
    let Some(auth_file) = aterm_agent::harness::worker::declared_codex_auth_file(
        starter,
        env.home.as_deref(),
        &env.cwd,
    ) else {
        return false;
    };
    aterm_agent::harness::codex_app_login::legacy_file_reader_allowed(&auth_file)
}

#[cfg(test)]
pub(super) fn pending_for_drain() -> (
    aterm_tempfile::TempDir,
    Arc<Registry>,
    aterm_agent::harness::account_work::Lifetime,
) {
    let (root, mut preparation, admitted, host_registry, lifetime) = tests::fixture();
    let cli = aterm_agent::harness::worker::worker_cli(aterm_phase::Program::Codex).unwrap();
    preparation.arm(admitted, cli).unwrap();
    preparation.typed().unwrap();
    drop(preparation);
    (root, host_registry, lifetime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};

    pub(super) fn fixture() -> (
        aterm_tempfile::TempDir,
        Preparation,
        Admitted,
        Arc<Registry>,
        aterm_agent::harness::account_work::Lifetime,
    ) {
        // Advertised-capability/layout fixture only: nothing executes this file
        // and no qualified SDK peer, source grant or activation is fabricated.
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let auth = home.join(".codex");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&auth)
            .unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(auth.join("auth.json"))
            .unwrap()
            .write_all(br#"{"OPENAI_API_KEY":"synthetic-constructor-fixture"}"#)
            .unwrap();
        let package = root.path().join("package");
        let binary = aterm_agent::harness::codex_alab::lay_package(&package, true);
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::OpenOptions::new().append(true).open(&binary).unwrap()
            .write_all(b"aterm machine credential slot v1\naterm machine credential slot pending v1\naterm session credential source v1\n").unwrap();
        let alias = root.path().join("selected-codex");
        std::os::unix::fs::symlink(&binary, &alias).unwrap();
        let cli = aterm_agent::harness::worker::worker_cli(aterm_phase::Program::Codex).unwrap();
        let mut starter = cli.plain();
        starter.exe = alias;
        let admitted = aterm_agent::harness::worker::admit(
            cli,
            starter,
            Some(&home),
            |_| true,
            |_| None,
            |_| Ok(()),
        )
        .unwrap();
        let mut env = Env::from_process().unwrap();
        env.home = Some(home.clone());
        env.cwd = root.path().into();
        env.config = None;
        env.state = root.path().join("harness");
        env.aterm_state = Some(home.join(".aterm"));
        let lifetime = aterm_agent::harness::account_work::Lifetime::default();
        let host_registry = Arc::new(Registry::default());
        let preparation = Preparation::with_context(
            host_registry.clone(),
            env,
            "s-constructor-fixture".into(),
            lifetime.enter().unwrap(),
        )
        .unwrap();
        (root, preparation, admitted, host_registry, lifetime)
    }

    fn exclusive(home: &Path) -> bool {
        let gate = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(home.join(".aterm/codex-source-custody/gate"))
            .unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "observe the real constructor kernel exclusion"
        )]
        gate.try_lock().is_ok()
    }

    #[test]
    fn unobserved_constructor_outcome_revokes_only_unclaimed_permission() {
        let (_root, mut preparation, admitted, _host_registry, _lifetime) = fixture();
        let home = preparation.env.home.clone().unwrap();
        assert!(!exclusive(&home));
        let cli = aterm_agent::harness::worker::worker_cli(aterm_phase::Program::Codex).unwrap();
        let admitted = preparation.arm(admitted, cli).unwrap();
        assert!(
            admitted
                .starter()
                .exe
                .starts_with(_root.path().join("package").canonicalize().unwrap())
        );
        preparation.check(&admitted).unwrap();
        let token = &admitted
            .starter()
            .env
            .iter()
            .find(|(name, _)| name == aterm_agent::harness::codex_source_registry::LAUNCH_ENV)
            .unwrap()
            .1;
        assert_eq!(token.len(), 32);
        let fresh = worker_fresh(
            None,
            false,
            false,
            aterm_agent::supervise::config::Approve::None,
        );
        let line =
            aterm_agent::harness::worker::line(cli, Dialect::Zsh, &admitted, &fresh).unwrap();
        assert!(line.contains(token));
        assert!(!exclusive(&home));
        drop(preparation); // also the uncertain-main-thread-reply path
        assert!(exclusive(&home));
    }

    #[test]
    fn observed_write_retains_native_reservation_after_launcher_returns() {
        let (_root, mut preparation, admitted, _host_registry, _lifetime) = fixture();
        let home = preparation.env.home.clone().unwrap();
        let cli = aterm_agent::harness::worker::worker_cli(aterm_phase::Program::Codex).unwrap();
        let admitted = preparation.arm(admitted, cli).unwrap();
        preparation.typed().unwrap();
        assert!(preparation.check(&admitted).is_err());
        let reservation = preparation.reservation.take().unwrap();
        drop(preparation);
        assert!(!exclusive(&home));
        // Explicit permission revocation, not inferred task completion/death.
        reservation.revoke_unclaimed().unwrap();
        assert!(exclusive(&home));
    }
    #[test]
    fn unsupported_legacy_constructor_never_adopts_native_owned_source() {
        let (root, mut preparation, admitted, _host_registry, _lifetime) = fixture();
        let cli = aterm_agent::harness::worker::worker_cli(aterm_phase::Program::Codex).unwrap();
        // Remove the SOURCE advertisement from fixture bytes, not a live image.
        let binary = std::fs::canonicalize(&admitted.starter().exe).unwrap();
        std::fs::write(&binary, b"legacy fixture").unwrap();
        let custom = root.path().join("custom.credentials");
        let mut starter = admitted.starter().clone();
        starter
            .env
            .push(("CODEX_AUTH_PATH".into(), custom.to_str().unwrap().into()));
        starter.unset.extend(["CODEX_AUTH_SLOT", "CODEX_HOME"]);
        let admitted = aterm_agent::harness::worker::admit(
            cli,
            starter,
            preparation.env.home.as_deref(),
            |_| true,
            |_| None,
            |_| Ok(()),
        )
        .unwrap();
        let admitted = preparation.arm(admitted, cli).unwrap();
        assert!(preparation.reservation.is_none());
        assert!(
            !admitted
                .starter()
                .env
                .iter()
                .any(|(key, _)| key == aterm_agent::harness::codex_source_registry::LAUNCH_ENV)
        );
        preparation.check(&admitted).unwrap();
        let marker = PathBuf::from(format!("{}.aterm-owner", custom.display()));
        std::fs::write(&marker, b"malformed-is-still-owned").unwrap();
        assert!(preparation.check(&admitted).is_err());
        assert!(preparation.arm(admitted.clone(), cli).is_err());
        std::fs::remove_file(&marker).unwrap();
        let (slot, ()) = admitted
            .prepare(cli, preparation.env.home.as_deref(), |mut starter| {
                starter
                    .env
                    .push(("CODEX_AUTH_SLOT".into(), "native-slot-declaration".into()));
                Ok((starter, ()))
            })
            .unwrap();
        assert!(preparation.arm(slot, cli).is_err());
    }
}
