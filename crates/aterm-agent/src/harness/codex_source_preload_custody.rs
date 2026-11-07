// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Count source work before its first I/O. An old Ready row must not hide a
//! preload, and an unknown result must retain custody until exact completion.
use super::*;

pub(super) struct Pending {
    operation: String,
    connection: Connection,
    worker: AtomicBool,
    revoked: AtomicBool,
    scope: Mutex<Option<super::super::account_work::Scope>>,
    retirement: super::super::account_work::Cancellation,
}
pub(super) struct Worker {
    pending: Arc<Pending>,
    _scope: super::super::account_work::Scope,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.pending.worker.store(false, Ordering::Release);
    }
}
impl Pending {
    /// Called outside registry State only after exact kernel departure. The
    /// worker keeps its own real capture; terminal metadata must not pin the
    /// original operation while waiting for that capture to retire.
    pub(super) fn revoke_departed(&self) {
        self.revoked.store(true, Ordering::Release);
        self.retirement.cancel();
        let scope = self.scope.lock().ok().and_then(|mut scope| scope.take());
        drop(scope);
    }
    pub(super) fn retired(&self) -> bool {
        self.retirement.retired()
    }
}
pub(super) fn enter(
    registry: &Registry,
    peer: &Peer,
    member: &str,
    operation: &str,
    connection: &Connection,
) -> Result<Worker, Error> {
    let key = member_key(&peer.caller, member)?;
    let mut state = lock(&registry.state)?;
    issued_on(&state, peer, member, connection)?;
    reservation::reservation_allows_member(&state, &peer.caller, member)?;
    let pending = if let Some(pending) = state.preloads.get(&key) {
        if pending.operation != operation
            || !pending.connection.same(connection)
            || pending.revoked.load(Ordering::Acquire)
        {
            return Err(Error::Busy);
        }
        pending.clone()
    } else {
        let scope = state
            .app_lifetime
            .as_ref()
            .and_then(|app| app.enter())
            .ok_or(Error::Busy)?;
        let pending = Arc::new(Pending {
            operation: operation.into(),
            connection: connection.clone(),
            worker: false.into(),
            revoked: false.into(),
            retirement: scope.cancellation(),
            scope: Mutex::new(Some(scope)),
        });
        state.preloads.insert(key, pending.clone());
        pending
    };
    let scope = lock(&pending.scope)?
        .as_ref()
        .cloned()
        .ok_or(Error::Stale)?;
    if pending.revoked.load(Ordering::Acquire) || scope.cancelled() {
        return Err(Error::Stale);
    }
    pending
        .worker
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| Error::Busy)?;
    Ok(Worker {
        pending,
        _scope: scope,
    })
}
pub(super) fn check_completion(
    state: &State,
    key: &str,
    operation: &str,
    connection: &Connection,
) -> Result<(), Error> {
    let Some(pending) = state.preloads.get(key) else {
        return Ok(());
    };
    if pending.operation != operation
        || !pending.connection.same(connection)
        || pending.revoked.load(Ordering::Acquire)
        || pending.worker.load(Ordering::Acquire)
    {
        return Err(Error::Busy);
    }
    Ok(())
}
pub(super) fn running(state: &State, key: &str) -> bool {
    state
        .preloads
        .get(key)
        .is_some_and(|pending| pending.worker.load(Ordering::Acquire))
}
