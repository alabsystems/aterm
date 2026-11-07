// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Isolated real encrypted-custody failure for native GUI lifetime controls.

use super::*;

/// Owns a fresh private world; no caller-supplied account authority is accepted.
pub struct PendingCustodyFixture {
    control: Control,
    env: super::super::cli::Env,
    restore: PendingCustodyRestore,
}

/// Keeps the isolated world alive and restores only its original custody directory.
pub struct PendingCustodyRestore {
    physical: super::super::codex_grant_owner::PendingFixtureRestore,
    env: super::super::cli::Env,
    profile: std::path::PathBuf,
    callback: std::net::SocketAddr,
    _world: aterm_tempfile::TempDir,
}

impl PendingCustodyRestore {
    pub fn restore(&mut self) -> Result<(), String> {
        self.physical.restore()
    }

    /// Probe actual private-world legacy, profile, callback and exchange resources.
    /// These observations cannot construct a grant or a retirement proof.
    pub fn resources_available(&self) -> Result<[bool; 4], String> {
        let store = self
            .env
            .accounts_store_for(accounts::Cli::Codex)
            .ok_or("fixture store unavailable")?;
        let legacy = store
            .signin_now()
            .map_err(|_| "fixture legacy probe unavailable")?
            .is_some();
        let profile = super::super::browser_window::ProfileLock::claim(&self.profile).is_ok();
        let callback = std::net::TcpListener::bind(self.callback).is_ok();
        let exchange =
            super::super::native_enrollment_admission::RefreshExchangeLease::for_env(&self.env)
                .is_ok();
        Ok([legacy, profile, callback, exchange])
    }
}

impl PendingCustodyFixture {
    /// Serialize the complete fixture test, including multiple private worlds.
    pub fn serial_callbacks() -> Result<impl Sized, String> {
        super::super::native_enrollment_admission::callback_test_turn()
    }

    pub fn new(scope: &super::super::account_work::Scope) -> Result<Self, String> {
        let world = aterm_tempfile::tempdir().map_err(|_| "fixture world unavailable")?;
        let home = world.path().join("home");
        let stage = world.path().join("stage");
        let config = world.path().join("config");
        for directory in [&home, &stage, &config] {
            aterm_secrets::fsx::ensure_private_dir(directory)
                .map_err(|_| "fixture private directory unavailable")?;
        }
        aterm_secrets::client::machine_identity_as(
            &aterm_secrets::client::Paths::locate_for_home(&home),
            aterm_secrets::Making {
                test_binary: true,
                ..aterm_secrets::Making::default()
            },
        )
        .map_err(|_| "fixture private identity unavailable")?;
        let config_path = config.join("aterm.toml");
        aterm_uds::atomic_file::write(&config_path, b"", 0o600)
            .map_err(|_| "fixture config unavailable")?;
        let env = super::super::cli::Env {
            state: world.path().join("state"),
            aterm_state: Some(world.path().join("aterm")),
            cwd: home.clone(),
            home: Some(home),
            claude_dir: None,
            claude_config_unusable: None,
            claude_profile: None,
            claude_custom_oauth: false,
            claude_session: None,
            claude_pid: None,
            codex_thread: None,
            codex_home: None,
            codex_login: None,
            agy_conversation: None,
            now: 1_790_000_000,
            sid: "s-isolated-custody-fixture".into(),
            sock: None,
            config: Some(config_path),
            claude_tmp: None,
            claude_running: None,
            tz_s: 0,
        };
        let store = env
            .accounts_store_for(accounts::Cli::Codex)
            .ok_or("fixture store unavailable")?;
        let roster = env
            .accounts_roster_for(accounts::Cli::Codex)
            .ok_or("fixture roster unavailable")?;
        let row = accounts::Row {
            label: "fixture".into(),
            class: accounts::Class::Preferred,
            dir: Some(
                stage
                    .to_str()
                    .ok_or("fixture directory is not text")?
                    .to_owned(),
            ),
            email: Some("fixture@example.test".into()),
            ..Default::default()
        };
        let text = accounts::append_rows_for(accounts::Cli::Codex, "", std::slice::from_ref(&row))?;
        aterm_uds::atomic_file::write(&roster, text.as_bytes(), 0o600)
            .map_err(|_| "fixture roster publication failed")?;
        let records = store
            .read_account_records(Some(&roster), env.now)
            .map_err(|_| "fixture records unavailable")?;
        let request = Request {
            id: "a".repeat(32),
            caller: "b".repeat(64),
            label: "fixture".into(),
            revision: accounts::pool_authority_revision(&records.raw_roster, &records.logins)?,
        };
        store.transact_logins(env.now, &mut None, |_, logins| {
            if !logins.enrollment.admit_native(&request)? {
                return Err("fixture admission was not fresh".into());
            }
            Ok(Vec::new())
        })?;
        let turn = super::super::native_enrollment_admission::NativeBrowserTurn::acquire(
            &store, &request, &row, env.now,
        )?;
        let callback = turn
            .callback()?
            .local_addr()
            .map_err(|_| "fixture callback unavailable")?;
        let profile = super::super::headless_oauth::vendor_profile_dir(
            &store.shared_dir(),
            &super::super::headless_oauth::vendor_label_key(accounts::Cli::Codex, &row.label),
            row.email.as_deref(),
        );
        let (work, control) = owned_work(request.clone(), scope.clone());
        // This isolated bridge is the only public constructor: it admits its own
        // private record before minting the original-work proof bit.
        work.admitted
            .store(true, std::sync::atomic::Ordering::Release);
        let (outcome, physical) = super::super::codex_grant_owner::pending_custody_fixture(
            &env,
            &request,
            &stage.join("auth.json"),
            scope,
            turn,
        )?;
        let _ = work.native.capture(outcome, &work.scope);
        if !control.has_pending_custody() {
            return Err("fixture lost pending custody".into());
        }
        drop(work);
        Ok(Self {
            control,
            env: env.clone(),
            restore: PendingCustodyRestore {
                physical,
                env,
                profile,
                callback,
                _world: world,
            },
        })
    }

    pub fn into_parts(self) -> (Control, super::super::cli::Env, PendingCustodyRestore) {
        (self.control, self.env, self.restore)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_pending_custody_retains_the_original_scope_until_restored_settlement() {
        let _callbacks = PendingCustodyFixture::serial_callbacks().unwrap();
        let lifetime = super::super::super::account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let (control, env, mut restore) = PendingCustodyFixture::new(&scope).unwrap().into_parts();
        let pending_resources = restore.resources_available();
        drop(scope);
        let pause = lifetime.pause().unwrap();
        assert!(control.has_pending_custody());
        assert!(!control.work_retired());
        assert!(control.retired().is_none());
        control.settle_custody(&env).unwrap();
        assert!(
            control.has_pending_custody(),
            "failed retry restores the actual carrier and scope"
        );
        let pause = pause
            .try_wait()
            .expect_err("the pending carrier owns its original count");
        restore.restore().unwrap();
        control.settle_custody(&env).unwrap();
        assert!(!control.has_pending_custody());
        assert!(control.work_retired());
        let retired_resources = restore.resources_available();
        control.retired().unwrap().finish(&env).unwrap();
        pause.try_wait().expect("actual pending work retired");
        assert_eq!(pending_resources.unwrap(), [false; 4]);
        assert_eq!(retired_resources.unwrap(), [true; 4]);
    }
}
