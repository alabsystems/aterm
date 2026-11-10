// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Private-world native publication controls for the GUI dependency boundary.
use super::super::{account_work, cli};
use super::*;

/// Synthetic responses and identity are created only inside a fresh private
/// world. This fixture accepts no paths, credentials, or publication authority.
pub struct NativeRowRecoveryFixture {
    _world: aterm_tempfile::TempDir,
    env: cli::Env,
    requests: Vec<Request>,
    lifetime: account_work::Lifetime,
}

impl NativeRowRecoveryFixture {
    pub(super) fn source_host() -> Result<Self, String> {
        let caller = aterm_uds::rand::hex_token::<32>().map_err(|_| "fixture nonce unavailable")?;
        let (world, env, requests) =
            super::super::codex_grant_owner::native_source_host_fixture(&caller);
        Ok(Self {
            _world: world,
            env,
            requests: Vec::from(requests),
            lifetime: account_work::Lifetime::default(),
        })
    }

    pub(super) fn directory(&self) -> &std::path::Path {
        self._world.path()
    }
    pub(super) fn keep(self) -> std::path::PathBuf {
        self._world.keep()
    }

    pub(super) fn publish_source_rows(&self) -> Result<(), String> {
        self.retire_originals()?;
        for request in &self.requests {
            let scope = self.lifetime.enter().ok_or("fixture admission closed")?;
            let (work, _) = owned_work(request.clone(), scope);
            if self.run_recovery(work)?.phase != Phase::Applied {
                return Err("source fixture publication did not settle".into());
            }
        }
        Ok(())
    }

    pub fn new(caller: &str) -> Self {
        let (world, env, requests) =
            super::super::codex_grant_owner::native_recovery_row_fixture(caller);
        Self {
            _world: world,
            env,
            requests: Vec::from(requests),
            lifetime: account_work::Lifetime::default(),
        }
    }

    #[cfg(test)]
    pub(in crate::harness) fn five(caller: &str) -> Self {
        let (world, env, requests) =
            super::super::codex_grant_owner::native_recovery_five_fixture(caller);
        Self {
            _world: world,
            env,
            requests: Vec::from(requests),
            lifetime: account_work::Lifetime::default(),
        }
    }

    pub fn env(&self) -> &cli::Env {
        &self.env
    }

    pub fn requests(&self) -> &[Request] {
        &self.requests
    }

    /// Attach the fixture's original synthetic attempt to an actual test App
    /// scope, so GUI cancellation can observe it remaining live until Drop.
    pub fn first_original(&self, scope: account_work::Scope) -> (Work, Control) {
        let (work, control) = owned_work(self.requests[0].clone(), scope);
        work.admitted
            .store(true, std::sync::atomic::Ordering::Release);
        (work, control)
    }

    /// The synthetic original attempt has no external resources. Its real
    /// counted Scope and canonical retirement transition still have to settle.
    pub fn retire_originals(&self) -> Result<(), String> {
        for request in &self.requests {
            let scope = self.lifetime.enter().ok_or("fixture admission closed")?;
            let (work, control) = owned_work(request.clone(), scope);
            work.admitted
                .store(true, std::sync::atomic::Ordering::Release);
            drop(work);
            control
                .retired()
                .ok_or("fixture scope still live")?
                .finish(&self.env)?;
        }
        Ok(())
    }

    /// Drive only this world's own request through the same native custody
    /// entrypoint as Work::run, with a host that cannot contact a provider.
    pub fn run_recovery(&self, work: Work) -> Result<Status, String> {
        if !self.requests.contains(&work.request) {
            return Err("work is not from this isolated fixture".into());
        }
        let cancellation = work.scope.cancellation();
        let operation = work.scope.operation_identity();
        let admission = Admission {
            scope: &work.scope,
            request: &work.request,
            original: &work.admitted,
            recovery: &work.recovery,
        };
        let retirement = std::sync::Mutex::new(None);
        let context = NativeContext {
            env: &self.env,
            runtime: &work.native,
            scope: &work.scope,
            retirement: &retirement,
        };
        let result = cli::enroll_codex_operation_custody(
            &self.env,
            &LocalOnly(self.env.now),
            &work.request,
            Some(&admission),
            Some(&context),
        );
        let _retirement = retirement.into_inner().unwrap();
        drop(work.scope);
        if result
            .as_ref()
            .is_ok_and(|status| matches!(status.phase, Phase::Running | Phase::CancelRequested))
            && let Some(proof) = retired_for(
                &work.request,
                &work.admitted,
                &work.recovery,
                &operation,
                cancellation.retired(),
            )
        {
            return proof.finish(&self.env);
        }
        result
    }

    /// Leave an actual native publication claim cancelled after its worker
    /// and kernel resources are gone, as in lost-host recovery.
    pub fn cancel_first_after_publication_claim(&self) -> Result<(), String> {
        let request = &self.requests[0];
        let store = self
            .env
            .accounts_store_for(accounts::Cli::Codex)
            .ok_or("fixture store unavailable")?;
        let roster = self
            .env
            .accounts_roster_for(accounts::Cli::Codex)
            .ok_or("fixture roster unavailable")?;
        let (logins, _) = store.load_logins(self.env.now)?;
        let reference = logins
            .enrollment
            .0
            .get(&request.id)
            .and_then(|record| record.grant_reference.as_ref())
            .ok_or("fixture retained response unavailable")?;
        let fresh = super::super::codex_grant_owner::recover(&self.env, request, reference)?;
        let scope = self.lifetime.enter().ok_or("fixture admission closed")?;
        let (work, control) = owned_work(request.clone(), scope);
        let candidate = super::super::codex_native_row::recovery_candidate(
            &self.env,
            (&store, &roster),
            request,
            &fresh,
        )?;
        let resources = super::super::codex_grant_owner::admit_native_recovery_publication(
            &self.env,
            &store,
            candidate,
            &fresh,
            &work.scope,
            super::super::wake_deadline::WakeDeadline::after(super::super::login_drive::HANG),
        )?;
        let admission = Admission {
            scope: &work.scope,
            request,
            original: &work.admitted,
            recovery: &work.recovery,
        };
        let turn = super::super::codex_native_row::claim_retired_recovery(
            &self.env,
            (&store, &roster),
            resources,
            &fresh,
            &admission,
        )?;
        control.cancel(&self.env)?;
        drop(turn);
        drop(work);
        Ok(())
    }

    pub fn publish_first(&self) -> Result<Status, String> {
        let scope = self.lifetime.enter().ok_or("fixture admission closed")?;
        let (work, _) = owned_work(self.requests[0].clone(), scope);
        self.run_recovery(work)
    }
}

struct LocalOnly(i64);
impl cli::AccountsHost for LocalOnly {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<super::super::align::Captured, String> {
        panic!("isolated recovery must not request provider auth")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("isolated recovery must not start login")
    }
    fn now(&self) -> i64 {
        self.0
    }
    fn vendor_sign_in(
        &self,
        _: &super::super::login_drive::Request<'_>,
    ) -> super::super::login_drive::Drove {
        panic!("isolated recovery must not replay exchange")
    }
}
