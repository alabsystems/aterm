// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Count source work before its first I/O. An old Ready row must not hide a
//! preload, and an unknown result must retain custody until exact completion.
use super::*;

pub(super) struct Pending {
    pub(super) operation: String,
    pub(super) connection: Connection,
    pub(super) worker: AtomicBool,
    pub(super) revoked: AtomicBool,
    pub(super) read_namespace: Mutex<Option<Arc<reservation::namespace::PreloadReadNamespace>>>,
    pub(super) retirement: super::super::account_work::Cancellation,
    pub(super) scope: Mutex<Option<super::super::account_work::Scope>>,
}
pub(super) struct Worker {
    pub(super) pending: Arc<Pending>,
    pub(super) _scope: super::super::account_work::Scope,
}
impl Worker {
    pub(super) fn scope(&self) -> &super::super::account_work::Scope {
        &self._scope
    }
    pub(super) fn check_completion(&self, state: &State, key: &str) -> Result<(), Error> {
        if self._scope.cancelled()
            || self.pending.revoked.load(Ordering::Acquire)
            || !self.pending.worker.load(Ordering::Acquire)
            || !state
                .preloads
                .get(key)
                .is_some_and(|pending| Arc::ptr_eq(pending, &self.pending))
        {
            return Err(Error::Stale);
        }
        Ok(())
    }
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
        let proof = self
            .read_namespace
            .lock()
            .ok()
            .and_then(|mut proof| proof.take());
        let scope = self.scope.lock().ok().and_then(|mut scope| scope.take());
        drop(proof);
        drop(scope);
    }
    /// Transfer an existing counted occurrence, never create a fresh Scope.
    /// The enclosing State transaction authenticates the original OPEN/body.
    pub(super) fn prepare_rebound(
        self: &Arc<Self>,
        connection: &Connection,
    ) -> Result<Arc<Self>, Error> {
        if self.connection.is_live()
            || self.worker.load(Ordering::Acquire)
            || self.revoked.load(Ordering::Acquire)
        {
            return Err(Error::Busy);
        }
        let scope = lock(&self.scope)?.as_ref().cloned().ok_or(Error::Stale)?;
        if scope.cancelled() {
            return Err(Error::Stale);
        }
        let proof = lock(&self.read_namespace)?.as_ref().cloned();
        Ok(Arc::new_cyclic(|weak| Self {
            operation: self.operation.clone(),
            connection: connection.clone(),
            worker: false.into(),
            revoked: false.into(),
            read_namespace: Mutex::new(
                proof.map(|proof| Arc::new(proof.rebound(weak.clone(), connection.clone()))),
            ),
            retirement: scope.cancellation(),
            scope: Mutex::new(Some(scope)),
        }))
    }
    /// Called outside State after the replacement holds the original Scope.
    /// Cancelling that Scope would revoke the valid replacement too.
    pub(super) fn release_rebound(&self) {
        self.revoked.store(true, Ordering::Release);
        let proof = self
            .read_namespace
            .lock()
            .ok()
            .and_then(|mut proof| proof.take());
        let scope = self.scope.lock().ok().and_then(|mut scope| scope.take());
        drop(proof);
        drop(scope);
    }
    pub(super) fn retired(&self) -> bool {
        self.retirement.retired()
    }
}
#[cfg(all(test, unix))]
pub(super) fn enter(
    registry: &Registry,
    peer: &Peer,
    member: &str,
    operation: &str,
    connection: &Connection,
) -> Result<Worker, Error> {
    enter_checked(registry, peer, member, operation, connection, |_| Ok(()))
}
pub(super) fn enter_checked(
    registry: &Registry,
    peer: &Peer,
    member: &str,
    operation: &str,
    connection: &Connection,
    new_intent: impl FnOnce(&mut State) -> Result<(), Error>,
) -> Result<Worker, Error> {
    enter_inner(
        registry, peer, member, operation, connection, None, new_intent,
    )
}
pub(super) fn enter_sequenced(
    registry: &Registry,
    peer: &Peer,
    member: &str,
    operation: &str,
    connection: &Connection,
    attempt: &preload_sequence::Attempt,
) -> Result<Worker, Error> {
    enter_inner(
        registry,
        peer,
        member,
        operation,
        connection,
        Some(attempt),
        |_| Ok(()),
    )
}
fn enter_inner(
    registry: &Registry,
    peer: &Peer,
    member: &str,
    operation: &str,
    connection: &Connection,
    attempt: Option<&preload_sequence::Attempt>,
    new_intent: impl FnOnce(&mut State) -> Result<(), Error>,
) -> Result<Worker, Error> {
    let key = member_key(&peer.caller, member)?;
    let mut state = lock(&registry.state)?;
    issued_on(&state, peer, member, connection)?;
    reservation::reservation_allows_member(&state, &peer.caller, member)?;
    let pending = if let Some(pending) = state.preloads.get(&key) {
        if let Some(attempt) = attempt {
            preload_sequence::check_current(&state, &key, &peer.process, attempt)?;
        }
        if pending.operation != operation
            || !pending.connection.same(connection)
            || pending.revoked.load(Ordering::Acquire)
        {
            return Err(Error::Busy);
        }
        pending.clone()
    } else {
        if attempt
            .is_some_and(|attempt| matches!(attempt.origin, preload_sequence::Origin::Ordinary))
        {
            // A fresh generic request is not an already-admitted root use or
            // a typed repair permission. Exact pre-reserve Pending may finish.
            reservation::fresh_ordinary_preload_allowed(&state, &peer.caller)?;
        }
        let scope = state
            .app_lifetime
            .as_ref()
            .and_then(|app| app.enter())
            .ok_or(Error::Busy)?;
        if let Err(error) = new_intent(&mut state) {
            drop(state);
            drop(scope);
            return Err(error);
        }
        if let Some(attempt) = attempt
            && let Err(error) =
                preload_sequence::accept(&mut state, &key, &peer.process, attempt, &scope)
        {
            drop(state);
            drop(scope);
            return Err(error);
        }
        let pending = Arc::new(Pending {
            read_namespace: Mutex::new(None),
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
/// A Ready worker must retain the already accepted preload occurrence. It
/// cannot create a new operation when an earlier completion or retirement won.
pub(super) fn enter_completion(
    registry: &Registry,
    peer: &Peer,
    member: &str,
    operation: &str,
    connection: &Connection,
) -> Result<Worker, Error> {
    let state = lock(&registry.state)?;
    issued_on(&state, peer, member, connection)?;
    reservation::reservation_allows_member(&state, &peer.caller, member)?;
    let pending = state
        .preloads
        .get(&member_key(&peer.caller, member)?)
        .ok_or(Error::Stale)?
        .clone();
    if pending.operation != operation
        || !pending.connection.same(connection)
        || pending.revoked.load(Ordering::Acquire)
    {
        return Err(Error::Stale);
    }
    let scope = lock(&pending.scope)?
        .as_ref()
        .cloned()
        .ok_or(Error::Stale)?;
    if scope.cancelled()
        || state
            .app_lifetime
            .as_ref()
            .is_none_or(|app| !scope.cancellation().owned_by(app))
    {
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

#[cfg(all(test, unix))]
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
