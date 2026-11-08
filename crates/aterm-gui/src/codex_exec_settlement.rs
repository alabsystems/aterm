// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A native execution gate and its strictly closing settlement handle. The
//! worker retains no launch binding, command, checkpoint or account proposal.

use super::*;
use std::sync::MutexGuard;
use std::time::Instant;

#[derive(Debug)]
pub(super) struct Initial {
    pub receipt: codex_renewal::Receipt,
    pub authority: SupervisorAuthority,
    pub source_connection: u64,
    pub human_seq: u64,
    pub deadline: u64,
    pub tx: Transaction,
    pub custody: Arc<NativeCustody>,
}

#[derive(Debug)]
struct Pending {
    reason: codex_renewal::CancellationKind,
    reached_seq: u64,
    acknowledged: bool,
    retry_at: Instant,
    delay: Duration,
}

#[derive(Debug)]
pub(super) struct State {
    pub receipt: codex_renewal::Receipt,
    pub authority: SupervisorAuthority,
    pub source_connection: u64,
    pub source_disconnected: bool,
    pub human_seq: u64,
    pub deadline: u64,
    pub tx: Transaction,
    pub custody: Option<Arc<NativeCustody>>,
    pub terminate: bool,
    pub active: bool,
    granted: bool,
    env: Env,
    target: String,
    pending: Option<Pending>,
    connection_released: bool,
}

#[derive(Debug)]
pub(super) struct Execution(Arc<Mutex<State>>);

pub(super) struct Settlement(Arc<Mutex<State>>);

impl Execution {
    pub(super) fn new(env: Env, target: String, initial: Initial) -> (Self, Settlement) {
        let shared = Arc::new(Mutex::new(State {
            receipt: initial.receipt,
            authority: initial.authority,
            source_connection: initial.source_connection,
            source_disconnected: false,
            human_seq: initial.human_seq,
            deadline: initial.deadline,
            tx: initial.tx,
            custody: Some(initial.custody),
            terminate: false,
            active: false,
            granted: false,
            env,
            target,
            pending: None,
            connection_released: false,
        }));
        (Self(Arc::clone(&shared)), Settlement(shared))
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl State {
    pub(super) fn activate(&mut self) -> bool {
        if self.pending.is_some() || self.tx.stage() == Stage::Cancelled {
            return false;
        }
        self.active = true;
        true
    }

    pub(super) fn environment(&self, now: i64) -> Env {
        let mut env = self.env.clone();
        env.now = now;
        env
    }

    pub(super) fn close(
        &mut self,
        reason: codex_renewal::CancellationKind,
        reached_seq: u64,
        explicit_stop: bool,
        now: Instant,
    ) {
        if matches!(self.tx.stage(), Stage::Consumed(_)) {
            return;
        }
        let granted = self.tx.revoke();
        self.active = false;
        self.granted |= granted;
        self.receipt.launch_attempted |= self.granted;
        self.terminate |= granted || explicit_stop;
        self.pending.get_or_insert(Pending {
            reason,
            reached_seq,
            acknowledged: false,
            retry_at: now,
            delay: Duration::from_millis(100),
        });
        // Closing admission proves no later Go; it does not prove that a
        // source without Go has finished writing its checkpoint. Retain native
        // custody until source closure, independently of metadata settlement.
    }
}

pub(super) struct Observation {
    pub authority: SupervisorAuthority,
    pub human_seq: u64,
    pub held: bool,
    pub policy: bool,
}

pub(super) struct Poll {
    pub done: bool,
    pub terminate: bool,
    pub release: Option<u64>,
    pub delay: Duration,
}

struct Attempt {
    env: Env,
    receipt: codex_renewal::Receipt,
    target: String,
    reason: codex_renewal::CancellationKind,
    human_seq: u64,
    reached_seq: u64,
    never_granted: bool,
}

impl Settlement {
    pub(super) fn deadline(&self) -> u64 {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).deadline
    }

    pub(super) fn poll(
        &mut self,
        owner: Option<Observation>,
        source_gone: bool,
        expired: bool,
        now: Instant,
        unix_seconds: i64,
    ) -> Poll {
        self.poll_using(
            owner,
            source_gone,
            expired,
            now,
            unix_seconds,
            aterm_types::update_boundary::Boundary::process(),
        )
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ExecCancellationSettlement",
            action = "NextCase",
            reason = "The fixture supplies reachable native transaction and receipt states"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ExecCancellationSettlement",
            action = "Fail",
            reason = "The filesystem can refuse a durability barrier"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ExecCancellationSettlement",
            action = "Recover",
            reason = "A later due poll may find healthy storage"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ExecCancellationSettlement",
            action = "SourceGone",
            reason = "The host supplies a separate process closure observation"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ExecCancellationSettlement",
            action = "Poll",
            project = "native_act::exec_handoff::operation::settlement::tests::model::transition"
        )
    )]
    fn poll_using(
        &self,
        owner: Option<Observation>,
        source_gone: bool,
        expired: bool,
        now: Instant,
        unix_seconds: i64,
        boundary: &aterm_types::update_boundary::Boundary,
    ) -> Poll {
        let attempt = self.prepare(owner, source_gone, expired, now, unix_seconds);
        if let Some(attempt) = attempt {
            // The owned snapshot contains no mutex guard. Closed admission
            // cannot reopen while a slow store or fsync settles this record.
            let result = boundary
                .try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)
                .and_then(|_guard| {
                    settle(
                        &attempt.env,
                        &attempt.receipt,
                        &attempt.target,
                        attempt.reason,
                        attempt.human_seq,
                        attempt.reached_seq,
                        attempt.never_granted,
                    )
                    .ok()
                    .flatten()
                });
            let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(receipt) = result {
                state.receipt = receipt;
                state
                    .pending
                    .as_mut()
                    .expect("pending cancellation")
                    .acknowledged = true;
            } else {
                let pending = state.pending.as_mut().expect("pending cancellation");
                pending.retry_at = now + pending.delay;
                pending.delay = (pending.delay * 2).min(Duration::from_secs(30));
            }
        }
        let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let done = state.custody.is_none()
            && state.pending.as_ref().is_none_or(|p| p.acknowledged)
            && (!state.terminate || source_gone);
        let release =
            (state.custody.is_none() && state.source_disconnected && !state.connection_released)
                .then_some(state.source_connection);
        state.connection_released |= release.is_some();
        let delay = if source_gone {
            state
                .pending
                .as_ref()
                .filter(|p| !p.acknowledged)
                .map_or(Duration::from_millis(100), |p| {
                    p.retry_at.saturating_duration_since(now)
                })
                .max(Duration::from_millis(100))
        } else {
            Duration::from_millis(100)
        };
        Poll {
            done,
            terminate: state.terminate && !source_gone,
            release,
            delay,
        }
    }

    fn prepare(
        &self,
        owner: Option<Observation>,
        source_gone: bool,
        expired: bool,
        now: Instant,
        unix_seconds: i64,
    ) -> Option<Attempt> {
        let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if state.custody.is_some() || state.pending.is_some() {
            let reason = match owner {
                None => Some((codex_renewal::CancellationKind::OwnerStop, state.human_seq)),
                Some(owner) if owner.human_seq != state.human_seq => Some((
                    codex_renewal::CancellationKind::NewPersonTask,
                    owner.human_seq,
                )),
                Some(owner) if owner.held || !owner.policy => Some((
                    codex_renewal::CancellationKind::PolicyRevoked,
                    owner.human_seq,
                )),
                Some(owner) if owner.authority != state.authority || expired || source_gone => {
                    Some((
                        codex_renewal::CancellationKind::ChangedRecord,
                        owner.human_seq,
                    ))
                }
                Some(_) => None,
            };
            if let Some((reason, reached)) = reason {
                state.close(reason, reached, false, now);
            }
        }
        if source_gone {
            state.custody.take();
        }
        state
            .pending
            .as_ref()
            .filter(|p| !p.acknowledged && now >= p.retry_at)
            .map(|pending| Attempt {
                env: state.environment(unix_seconds),
                receipt: state.receipt.clone(),
                target: state.target.clone(),
                reason: pending.reason,
                human_seq: state.human_seq,
                reached_seq: pending.reached_seq,
                never_granted: !state.granted,
            })
    }
}

fn settle(
    env: &Env,
    receipt: &codex_renewal::Receipt,
    target: &str,
    reason: codex_renewal::CancellationKind,
    human_seq: u64,
    reached_seq: u64,
    never_granted: bool,
) -> Result<Option<codex_renewal::Receipt>, &'static str> {
    let observed = codex_renewal::with_receipt_writer(env, |writer| {
        writer.cancel(receipt, reason, human_seq, reached_seq)?;
        codex_renewal::read_receipt(env, &receipt.id).ok_or("cancel-receipt-unavailable")
    })?;
    let Some(observed) = observed else {
        return Ok(None);
    };
    if never_granted
        && observed.phase == codex_renewal::Phase::Retired
        && !observed.launch_attempted
        && observed.accepted_turn.is_none()
        && observed.current_attempt.is_none()
        && observed.candidate.is_none()
        && observed.successor_pid.is_none()
        && observed.successor_birth.is_none()
    {
        // The preceding cancellation closed admission under the live gate.
        // This second writer rechecks exact bytes before releasing a reservation.
        return codex_renewal::cancel_unexecuted_rotation(env, &observed, target);
    }
    // An attempted cancellation cannot reclaim account rights. A Consumed
    // observation acknowledges history but cannot consume the live transaction.
    Ok(Some(observed))
}

#[cfg(test)]
#[path = "codex_exec_settlement_tests.rs"]
mod tests;
