// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit before-effect cancellation. A dropped reservation, missing reply,
//! or prepared record never proves that installation did not start.

use super::*;

pub(in crate::harness) struct Cancelled {
    caller: Caller,
    operation: String,
    namespace: BookNamespace,
}

/// Borrowed only while the exact signed no-effect source Book is claimed.
/// Kernel restoration grants no credential, member, or target authority.
pub(in crate::harness) struct CancelledSource<'a> {
    intent: &'a TargetIntent,
    namespace: &'a BookNamespace,
    _claimed: &'a Claimed,
}
impl CancelledSource<'_> {
    pub(in crate::harness) fn caller(&self) -> &Caller {
        self.intent.caller()
    }
    pub(in crate::harness) fn matches_source(
        &self,
        source: &crate::harness::codex_session_slot::SourceHandle,
    ) -> bool {
        source.binding() == &self.intent.record.source
            && source.source_authority() == &self.intent.record.source_authority
            && source.assignment_generation() == self.intent.record.body.expected_assignment
            && source
                .namespaces()
                .is_some_and(|scope| scope.book() == self.namespace)
    }
}

impl Cancelled {
    pub(in crate::harness) fn with_current_source<R>(
        &self,
        env: &Env,
        intent: &TargetIntent,
        namespace: &BookNamespace,
        restore: impl FnOnce(&CancelledSource<'_>) -> Result<R, Failure>,
    ) -> Result<R, Failure> {
        if !self.matches(intent.caller(), intent.operation(), namespace) {
            return Err(Failure::Unavailable);
        }
        let lease = open(env, intent.caller())?;
        let claimed = lease.try_lock().map_err(|_| Failure::Busy)?;
        if claimed.book.consumer != Consumer::observe(intent.caller())? {
            return Err(Failure::Unavailable);
        }
        with_cancelled_claim(&claimed, intent, namespace, restore)
    }
    pub(in crate::harness) fn matches(
        &self,
        caller: &Caller,
        operation: &str,
        namespace: &BookNamespace,
    ) -> bool {
        &self.caller == caller && self.operation == operation && &self.namespace == namespace
    }
}

fn with_cancelled_claim<R>(
    claimed: &Claimed,
    intent: &TargetIntent,
    namespace: &BookNamespace,
    restore: impl FnOnce(&CancelledSource<'_>) -> Result<R, Failure>,
) -> Result<R, Failure> {
    claimed.store.unchanged().map_err(str::to_owned)?;
    if claimed.pending_install.is_some()
        || !namespace.current()
        || claimed.store.physical_identity().map_err(str::to_owned)? != *namespace
        || !claimed.book.valid(intent.caller())
        || !origin_namespace_matches(&claimed.book, intent)?
        || !claimed
            .book
            .prepared
            .iter()
            .any(|record| record.body.operation == intent.operation() && record.cancelled)
    {
        return Err(Failure::Unavailable);
    }
    let result = restore(&CancelledSource {
        intent,
        namespace,
        _claimed: claimed,
    })?;
    claimed.store.unchanged().map_err(str::to_owned)?;
    if !namespace.current() {
        return Err(Failure::Unavailable);
    }
    Ok(result)
}

impl std::fmt::Debug for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CancelledAssignment(<private>)")
    }
}

pub(in crate::harness) enum Progress {
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "the opaque cancellation receipt contains an uninhabited unsupported physical namespace"
        )
    )]
    Cancelled(Cancelled),
    // Reconciliation is required. This is no target, source, or Applied proof.
    EffectsStarted,
}

pub(in crate::harness) enum Failure {
    Busy,
    Unavailable,
}
impl From<String> for Failure {
    fn from(_: String) -> Self {
        Self::Unavailable
    }
}
impl From<&str> for Failure {
    fn from(_: &str) -> Self {
        Self::Unavailable
    }
}

/// `validate` checks the already-fenced actual reservation while Book is
/// claimed. It must perform no I/O or acquire owner/account custody.
pub(in crate::harness) fn before_effects(
    env: &Env,
    intent: &TargetIntent,
    namespace: &BookNamespace,
    validate: impl FnOnce() -> Result<(), Failure>,
) -> Result<Progress, Failure> {
    platform::require_available()?;
    let home = env.home.as_deref().ok_or("assignment home unavailable")?;
    let identity = platform::machine_identity(home)
        .map_err(|_| "assignment identity unavailable")?
        .ok_or("assignment identity unavailable")?;
    let lease = open(env, intent.caller())?;
    let mut claimed = lease.try_lock().map_err(|_| Failure::Busy)?;
    claimed.store.unchanged().map_err(str::to_owned)?;
    if !namespace.current()
        || claimed.store.physical_identity().map_err(str::to_owned)? != *namespace
    {
        return Err("assignment cancellation namespace changed".into());
    }
    validate()?;
    persist_cancelled(&mut claimed, &identity, intent, namespace).map_err(Failure::from)
}

fn persist_cancelled(
    claimed: &mut Claimed,
    identity: &age::Identity,
    intent: &TargetIntent,
    namespace: &BookNamespace,
) -> Result<Progress, String> {
    claimed.store.unchanged().map_err(str::to_owned)?;
    if !namespace.current()
        || claimed.store.physical_identity().map_err(str::to_owned)? != *namespace
    {
        return Err("assignment cancellation namespace changed".into());
    }
    let index = claimed
        .book
        .prepared
        .iter()
        .position(|record| record.body.operation == intent.operation())
        .ok_or("assignment cancellation intent unavailable")?;
    let record = &claimed.book.prepared[index];
    let mut expected = intent.record.clone();
    expected.cancelled = record.cancelled;
    if !same_prepared(record, &expected) {
        return Err("assignment cancellation intent changed".into());
    }
    if claimed.pending_install.is_some()
        || record.commit_boundary.is_some()
        || claimed.book.current.authority.native_intent() == Some(intent.operation())
    {
        return Ok(Progress::EffectsStarted);
    }
    if claimed.book.current.binding != record.source
        || claimed.book.current.authority != record.source_authority
        || claimed.book.current.generation != record.body.expected_assignment
        || !matches!(claimed.book.current.phase, Phase::Startup | Phase::Applied)
    {
        return Err("assignment cancellation source changed".into());
    }
    // The exact terminal intent remains signed and retained. A replay retries
    // the same write, including after a save whose result was not observed.
    claimed.book.prepared[index].cancelled = true;
    publish(claimed, identity)?;
    claimed.store.unchanged().map_err(str::to_owned)?;
    if !namespace.current() {
        return Err("assignment cancellation namespace changed".into());
    }
    #[cfg(unix)]
    {
        Ok(Progress::Cancelled(Cancelled {
            caller: intent.caller().clone(),
            operation: intent.operation().into(),
            namespace: namespace.clone(),
        }))
    }
    #[cfg(not(unix))]
    {
        Err("native credential custody is unsupported on this platform".into())
    }
}

#[cfg(all(test, unix))]
#[path = "codex_assignment_prepare_cancel_tests.rs"]
mod tests;
