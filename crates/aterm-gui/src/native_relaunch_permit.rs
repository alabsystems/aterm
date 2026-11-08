// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One scoped checked-producer authorization in the session's existing native state.

use super::*;
use aterm_agent::harness::relaunch::{CheckedRelaunch, CheckedRelaunchStep};
use std::sync::atomic::{AtomicU8, Ordering};

const PENDING: u8 = 0;
const TRANSFERRED: u8 = 1;
const ACCEPTED: u8 = 2;
const SIGNAL_READY: u8 = 3;
const CONSUMED: u8 = 4;
const REVOKED: u8 = 5;

/// Data and atomic identity only: never owns a callback, session, Scope or descriptor.
#[derive(Debug)]
pub(super) struct Permit {
    request: Prepare,
    state: AtomicU8,
}

impl Permit {
    pub(super) fn matches(&self, request: &Prepare) -> bool {
        self.request == *request
            && matches!(
                self.state.load(Ordering::Acquire),
                PENDING | TRANSFERRED | ACCEPTED
            )
    }
    pub(super) fn accepted(&self, request: &Prepare) -> bool {
        self.request == *request && self.state.load(Ordering::Acquire) == ACCEPTED
    }
    pub(super) fn consume_signal(&self, request: &Prepare) -> bool {
        self.request == *request
            && self
                .state
                .compare_exchange(SIGNAL_READY, CONSUMED, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }
    pub(super) fn finish_admission(&self, request: &Prepare) -> bool {
        self.request == *request
            && (self.state.load(Ordering::Acquire) == ACCEPTED
                || self
                    .state
                    .compare_exchange(TRANSFERRED, ACCEPTED, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok())
    }
}

struct Scoped {
    ctx: std::sync::Weak<crate::SessionCtx>,
    permit: Arc<Permit>,
}

impl Scoped {
    fn revoke_unused(&self) -> bool {
        // Revocation wins a delayed final publish/first Signal even before cleanup gets the lock.
        self.permit
            .state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                matches!(state, PENDING | TRANSFERRED | SIGNAL_READY).then_some(REVOKED)
            })
            .is_ok()
    }

    fn cleanup_exact(&self) {
        let Some(ctx) = self.ctx.upgrade() else {
            return;
        };
        let mut table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if table
            .relaunch_pending
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &self.permit))
        {
            table.relaunch_pending = None;
        }
        let operation = table.by_id.get(&self.permit.request.id).cloned();
        drop(table);
        if let Some(operation) = operation {
            let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(qualification) = op.authority.asked.as_mut()
                && qualification
                    .signal
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &self.permit))
            {
                qualification.signal = None;
            }
        }
    }
}

impl Drop for Scoped {
    fn drop(&mut self) {
        if self.revoke_unused() {
            self.cleanup_exact();
        }
    }
}

/// Called only by the actual session's in-process restart hook, outside native/Meta locks.
pub(crate) fn authorize(
    ctx: &Arc<crate::SessionCtx>,
    checked: &CheckedRelaunch,
) -> Result<Box<dyn Send>, &'static str> {
    authorize_scope(ctx, checked).map(|scope| match scope {
        Some(scope) => Box::new(scope) as Box<dyn Send>,
        None => Box::new(()) as Box<dyn Send>,
    })
}

fn authorize_scope(
    ctx: &Arc<crate::SessionCtx>,
    checked: &CheckedRelaunch,
) -> Result<Option<Scoped>, &'static str> {
    let request = checked.request();
    if checked.tab() != ctx.self_id.as_str()
        || crate::harness_host::published_snapshot(ctx.self_id.as_str())
            .and_then(|snapshot| snapshot.session)
            .as_deref()
            != Some(checked.conversation())
    {
        return Err("native-checked-conversation-changed");
    }
    let now_us = Clock::System.now_us().ok_or("native-checked-clock")?;
    if !matches!(request.asked, Some(AskedIntent::Relaunch(_)))
        || !asked_upgrade::shape(request)
        || host_epoch() != Some(request.host_epoch.as_str())
        || request.expected_human_seq != Some(ctx.human_input.seq())
        || now_us >= request.deadline_us
    {
        return Err("native-checked-session-changed");
    }
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let authority = meta.supervisor_authority(crate::metrics::now_us());
    if authority.revision() != request.revision
        || !authority.builtin_keeps(&request.holder, Scope::Relaunch)
    {
        return Err("native-checked-authority-changed");
    }
    let mut table = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    reclaim_pending(
        &mut table,
        Clock::System.now_us().ok_or("native-checked-clock")?,
    );
    if request.expected_human_seq != Some(ctx.human_input.seq())
        || Clock::System.remaining(request.deadline_us).is_none()
    {
        return Err("native-checked-session-changed");
    }
    if let Some(operation) = table.by_id.get(&request.id).cloned() {
        let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
        if request.expected_human_seq != Some(ctx.human_input.seq())
            || Clock::System.remaining(request.deadline_us).is_none()
        {
            return Err("native-checked-session-changed");
        }
        if op.request != *request
            || op
                .authority
                .asked
                .as_ref()
                .is_none_or(|qualification| !qualification.matches_request(request))
        {
            return Err("native-checked-operation-changed");
        }
        if checked.step() == CheckedRelaunchStep::Prepare && op.phase == Phase::Queued {
            if !matches!(&table.asked_binding, asked_upgrade::CurrentBinding::Unsettled {binding, ..} if binding == &op.binding)
                || table.relaunch_pending.is_some()
            {
                return Err("native-checked-prepare-superseded");
            }
            let qualification = op
                .authority
                .asked
                .as_mut()
                .ok_or("native-checked-unqualified")?;
            if qualification
                .producer
                .as_ref()
                .is_some_and(|permit| permit.matches(request))
            {
                return Err("native-checked-prepare-busy");
            }
            let permit = Arc::new(Permit {
                request: request.clone(),
                state: AtomicU8::new(TRANSFERRED),
            });
            qualification.producer = Some(Arc::clone(&permit));
            table.relaunch_pending = Some(Arc::clone(&permit));
            return Ok(Some(Scoped {
                ctx: Arc::downgrade(ctx),
                permit,
            }));
        }
        if checked.step() == CheckedRelaunchStep::Prepare || op.signal_sent {
            // The exact accepted operation already owns its producer identity. This is no
            // new admission or repeated effect and supplies no replacement authorization.
            return Ok(None);
        }
        if op.phase != Phase::Started || op.clock.remaining(request.deadline_us).is_none() {
            return Err("native-checked-operation-phase");
        }
        let Some(qualification) = op.authority.asked.as_mut() else {
            return Err("native-checked-unqualified");
        };
        if qualification
            .producer
            .as_ref()
            .is_none_or(|permit| !permit.accepted(request))
            || qualification.signal.is_some()
        {
            return Err("native-checked-signal-busy");
        }
        let permit = Arc::new(Permit {
            request: request.clone(),
            state: AtomicU8::new(SIGNAL_READY),
        });
        qualification.signal = Some(Arc::clone(&permit));
        return Ok(Some(Scoped {
            ctx: Arc::downgrade(ctx),
            permit,
        }));
    }
    if checked.step() != CheckedRelaunchStep::Prepare || table.relaunch_pending.is_some() {
        return Err("native-checked-prepare-busy");
    }
    let permit = Arc::new(Permit {
        request: request.clone(),
        state: AtomicU8::new(PENDING),
    });
    table.relaunch_pending = Some(Arc::clone(&permit));
    Ok(Some(Scoped {
        ctx: Arc::downgrade(ctx),
        permit,
    }))
}

/// Exercise delayed cleanup from a genuinely issued, already revoked producer.
#[cfg(all(test, unix))]
pub(super) fn authorize_after_old_cleanup(
    ctx: &Arc<crate::SessionCtx>,
    checked: &CheckedRelaunch,
) -> Result<Box<dyn Send>, &'static str> {
    let old = authorize_scope(ctx, checked)?.ok_or("expected-new-producer")?;
    if !old.revoke_unused() {
        return Err("expected-unused-producer");
    }
    let current = authorize(ctx, checked)?;
    old.cleanup_exact();
    Ok(current)
}

pub(super) fn pending(table: &Operations, request: &Prepare) -> Option<Arc<Permit>> {
    table
        .relaunch_pending
        .as_ref()
        .filter(|permit| permit.matches(request))
        .cloned()
}

pub(super) fn superseded(table: &mut Operations, request: Option<&Prepare>) {
    if table
        .relaunch_pending
        .as_ref()
        .is_some_and(|permit| request != Some(&permit.request))
        && let Some(permit) = table.relaunch_pending.take()
    {
        permit.state.store(REVOKED, Ordering::Release);
    }
}

pub(super) fn reclaim_pending(table: &mut Operations, now_us: u64) {
    if table.relaunch_pending.as_ref().is_some_and(|permit| {
        permit.request.deadline_us <= now_us || permit.state.load(Ordering::Acquire) == REVOKED
    }) {
        superseded(table, None);
    }
}

/// Transfer into the queued operation; the producer can still revoke across journal I/O.
pub(super) fn accept(table: &mut Operations, op: &Operation) -> bool {
    if !matches!(op.request.asked, Some(AskedIntent::Relaunch(_))) {
        return true;
    }
    let Some(permit) = op
        .authority
        .asked
        .as_ref()
        .and_then(|q| q.producer.as_ref())
    else {
        return false;
    };
    table
        .relaunch_pending
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, permit))
        && permit.matches(&op.request)
        && permit
            .state
            .compare_exchange(PENDING, TRANSFERRED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
}

/// The last post-journal admission point, under Meta -> table -> operation.
pub(super) fn finish_admission(table: &mut Operations, op: &Operation) -> bool {
    if !matches!(op.request.asked, Some(AskedIntent::Relaunch(_))) {
        return true;
    }
    let Some(permit) = op
        .authority
        .asked
        .as_ref()
        .and_then(|q| q.producer.as_ref())
    else {
        return false;
    };
    if !table
        .relaunch_pending
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, permit))
        || !permit.finish_admission(&op.request)
    {
        return false;
    }
    table.relaunch_pending = None;
    true
}
