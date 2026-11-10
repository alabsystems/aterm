// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Owned refresh of an actual admitted manager use, including response custody.
use super::*;
pub struct OwnedUseRefresh {
    namespace: UseNamespace,
    request: slot::RefreshRequest,
}
impl std::fmt::Debug for OwnedUseRefresh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnedUseRefresh(redacted)")
    }
}
impl SourceUse {
    pub fn prepare_refresh(
        &self,
        caller: &slot::Caller,
        home: &std::path::Path,
        request: slot::RefreshRequest,
    ) -> Result<OwnedUseRefresh, Error> {
        let namespace = self.namespace(caller, home)?;
        namespace.refresh_allowed()?;
        if request.slot != namespace.source().slot() {
            return Err(Error::Denied);
        }
        Ok(OwnedUseRefresh { namespace, request })
    }
}
impl OwnedUseRefresh {
    pub fn run(&self, env: &Env) -> Result<slot::RefreshProgress, Error> {
        loop {
            let progress = slot::refresh_for_use(
                env,
                self.namespace.caller(),
                &self.request,
                &self.namespace.context,
            )
            .map_err(|_| Error::Unavailable)?;
            let slot::RefreshProgress::Retaining(mut retention) = progress else {
                return Ok(progress);
            };
            // Provider response settlement precedes cancellation. No new HTTP
            // exchange is performed by retention retry/reconcile.
            loop {
                match retention.retry() {
                    Ok(continuation) => match continuation.reconcile(env, self.namespace.scope()) {
                        slot::RefreshRecovery::Complete => break,
                        slot::RefreshRecovery::Unavailable => return Err(Error::Unavailable),
                        slot::RefreshRecovery::Retaining(owned) => retention = owned,
                    },
                    Err(owned) => retention = owned,
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    }
}
