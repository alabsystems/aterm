// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Publish retained native completion under the exact account-receipt writer.
//! No returned state grants native execution authority.

use super::super::{self as renewal, RestartCompletion};
use super::{Phase, Receipt, ReceiptWriter};
use crate::harness::{native_act::Prepare, upgrade_drive};

/// A missing record and an unresolved retained record have different recovery
/// consequences. In particular, `Pending` never authorizes another attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionProgress {
    Absent,
    Active,
    Pending,
    /// The current child is positively closed without execution. The logical
    /// root remains committing; a later child still needs native admission.
    ChildNotStarted,
    /// Both the native completion and this exact receipt are acknowledged.
    Published(Box<Receipt>),
}

impl ReceiptWriter<'_> {
    /// Acknowledge the root's write-ahead marker before native driving. An
    /// attached candidate on an already-marked root is retained on retry.
    /// This metadata grants no native execution authority.
    ///
    /// # Errors
    /// The exact root or preparation changed, is cancelled or terminal, or
    /// its receipt cannot be durably acknowledged.
    pub fn record_launch_attempted(
        &self,
        expected: &Receipt,
        prepare: &Prepare,
    ) -> Result<Receipt, &'static str> {
        if expected.revoked
            || renewal::read_cancellation(self.env, expected)?.is_some()
            || renewal::read_receipt(self.env, &expected.id).as_ref() != Some(expected)
            || expected.phase != Phase::Committing
            || expected.current_attempt.is_some()
            || expected.successor_pid.is_some()
            || expected.successor_birth.is_some()
            || expected.accepted_turn.is_some()
            || renewal::restart_wire(prepare).is_none()
            || renewal::active_prepare(self.env, expected).as_ref() != Some(prepare)
        {
            return Err("launch-root-changed");
        }
        if expected.launch_attempted {
            self.acknowledge(expected)?;
            return Ok(expected.clone());
        }
        if expected.candidate.is_some() {
            return Err("launch-unmarked-candidate");
        }
        let mut marked = expected.clone();
        marked.launch_attempted = true;
        renewal::write_receipt(self.env, &marked)?;
        Ok(marked)
    }

    /// Recover only the supplied current attempt, then durably publish its
    /// observed result. The native file lock is attempted without waiting; no
    /// host connection, signal, resume, child admission or seal release occurs.
    ///
    /// # Errors
    /// Store or receipt identity changed, cancellation is present, the active
    /// request differs, or native/receipt durability cannot be acknowledged.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RenewalCompletionPublication",
            action = "NextCase",
            reason = "The caller and durable records supply the bounded recovery scenario"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RenewalCompletionPublication",
            action = "NextFailure",
            reason = "The filesystem refuses the native lock or a receipt directory barrier"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "RenewalCompletionPublication",
            action = "Recover",
            project = "aterm_agent::harness::codex_renewal::attempts::completion::tests::publication_transition"
        )
    )]
    pub fn recover_completion(
        &self,
        expected: &Receipt,
        prepare: &Prepare,
        opts: &upgrade_drive::Opts,
    ) -> Result<CompletionProgress, &'static str> {
        self.recover_completion_using(expected, prepare, opts, |next| {
            renewal::write_receipt(self.env, next)
        })
    }

    fn recover_completion_using(
        &self,
        expected: &Receipt,
        prepare: &Prepare,
        opts: &upgrade_drive::Opts,
        publish: impl FnOnce(&Receipt) -> Result<(), &'static str>,
    ) -> Result<CompletionProgress, &'static str> {
        if opts.dry_run
            || opts.state != self.env.state
            || self.env.home.as_ref() != Some(&opts.home)
        {
            return Err("completion-store");
        }
        if expected.revoked || renewal::read_cancellation(self.env, expected)?.is_some() {
            return Err("completion-revoked");
        }
        let current = renewal::read_receipt(self.env, &expected.id).ok_or("completion-receipt")?;
        if &current != expected
            || current.revoked
            || renewal::restart_wire(prepare).is_none()
            || renewal::active_prepare(self.env, &current).as_ref() != Some(prepare)
            || !matches!(
                current.phase,
                Phase::Committing | Phase::Committed | Phase::Consumed | Phase::Retired
            )
        {
            return Err("completion-changed");
        }
        let key = format!("renewal-{}", prepare.id);
        let Some(state) =
            upgrade_drive::load_record(opts, &key).map_err(|_| "completion-native-unreadable")?
        else {
            return Ok(if current.phase == Phase::Committing {
                CompletionProgress::Absent
            } else {
                CompletionProgress::Pending
            });
        };
        let Some(outcome) = renewal::completion_value(&state, &current.sid, prepare) else {
            // Renewal journals are per attempt. A foreign completed request
            // here is inconsistent, unlike the shared per-tab upgrade journal.
            let active = current.phase == Phase::Committing
                && state.tab == current.sid
                && state.native_coordination
                && state.native_noexec_prepare.is_none()
                && !state.native_retired_unissued
                && state.native_completed_prepare.is_none()
                && state.native_account_candidate.is_none()
                && renewal::matching_restart(&state, prepare);
            return Ok(if active {
                CompletionProgress::Active
            } else {
                CompletionProgress::Pending
            });
        };
        let target =
            completion_target(&current, prepare, &outcome).ok_or("completion-candidate")?;
        // Acknowledge only after all receipt-side preconditions hold. This
        // shares the native reader's full validation and checked-save barrier.
        renewal::recover_completion(opts, &current.sid, prepare, &state)
            .ok_or("completion-native-unacknowledged")?;
        let Some(target) = target else {
            self.acknowledge(&current)?;
            return Ok(CompletionProgress::ChildNotStarted);
        };
        if target == current {
            self.acknowledge(&current)?;
        } else {
            publish(&target)?;
        }
        Ok(CompletionProgress::Published(Box::new(target)))
    }
}

/// The native effects of one root coordinator attempt. Calls happen outside
/// the account writer. The driver still needs the native host's own admission.
pub trait RootDriver {
    fn now_us(&self) -> Option<u64> {
        crate::harness::native_act::monotonic_us()
    }

    fn prepare(
        &self,
        opts: &upgrade_drive::Opts,
        sid: &str,
        prepare: &Prepare,
    ) -> Result<(), &'static str> {
        renewal::prepare_restart(opts, sid, prepare)
    }

    /// False means the owning session can no longer authorize driving.
    fn before_signal(&self) -> bool;

    fn drive(&self, opts: &upgrade_drive::Opts, sid: &str, prepare: &Prepare) {
        let _observed = renewal::drive_restart(opts, sid, prepare);
    }
}

fn recover_current(
    env: &crate::harness::cli::Env,
    expected: &Receipt,
    prepare: &Prepare,
    opts: &upgrade_drive::Opts,
) -> Result<CompletionProgress, &'static str> {
    let current = renewal::read_receipt(env, &expected.id).ok_or("root-receipt-unavailable")?;
    if !current.same_intent(expected) || current.current_attempt.is_some() {
        return Err("root-intent-changed");
    }
    renewal::with_receipt_writer(env, |writer| {
        writer.recover_completion(&current, prepare, opts)
    })?
    .ok_or("root-writer-busy")
}

/// Advance one root attempt, consuming retained completion before considering
/// its deadline. Raw transport returns never publish a receipt or release a seal.
///
/// # Errors
/// The root changed or local durability cannot be acknowledged. Native failure
/// leaves its exact retained journal for the next attempt.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RenewalRootRecovery",
        action = "NextCase",
        reason = "The caller, native journal and driver provide each bounded scenario"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "RenewalRootRecovery",
        action = "Step",
        project = "aterm_agent::harness::codex_renewal::attempts::completion::tests::root::transition"
    )
)]
pub fn advance_root(
    env: &crate::harness::cli::Env,
    expected: &Receipt,
    prepare: &Prepare,
    opts: &upgrade_drive::Opts,
    driver: &impl RootDriver,
) -> Result<CompletionProgress, &'static str> {
    if expected.current_attempt.is_some() {
        return Err("root-is-child");
    }
    let progress = renewal::with_receipt_writer(env, |writer| {
        writer.recover_completion(expected, prepare, opts)
    })?
    .ok_or("root-writer-busy")?;
    if !matches!(
        progress,
        CompletionProgress::Absent | CompletionProgress::Active
    ) || !driver.now_us().is_some_and(|now| now < prepare.deadline_us)
    {
        return Ok(progress);
    }
    if driver.prepare(opts, &expected.sid, prepare).is_err() {
        return recover_current(env, expected, prepare, opts);
    }
    let progress = recover_current(env, expected, prepare, opts)?;
    if !matches!(
        progress,
        CompletionProgress::Absent | CompletionProgress::Active
    ) || !driver.now_us().is_some_and(|now| now < prepare.deadline_us)
    {
        return Ok(progress);
    }
    let latest = renewal::read_receipt(env, &expected.id).ok_or("root-receipt-unavailable")?;
    if !latest.same_intent(expected) {
        return Err("root-intent-changed");
    }
    renewal::with_receipt_writer(env, |writer| {
        writer.record_launch_attempted(&latest, prepare)
    })?
    .ok_or("root-writer-busy")?;
    if driver.now_us().is_some_and(|now| now < prepare.deadline_us) && driver.before_signal() {
        driver.drive(opts, &expected.sid, prepare);
    }
    recover_current(env, expected, prepare, opts)
}

/// `Some(None)` closes a child without retiring its still-pending root.
fn completion_target(
    current: &Receipt,
    prepare: &Prepare,
    outcome: &RestartCompletion,
) -> Option<Option<Receipt>> {
    let mut next = current.clone();
    match outcome {
        RestartCompletion::NotStarted => {
            if current.candidate.is_some()
                || current.successor_pid.is_some()
                || current.successor_birth.is_some()
                || current.accepted_turn.is_some()
            {
                return None;
            }
            if current.current_attempt.is_some() {
                return (current.phase == Phase::Committing && current.launch_attempted)
                    .then_some(None);
            }
            if !matches!(current.phase, Phase::Committing | Phase::Retired)
                || (current.phase == Phase::Retired && current.launch_attempted)
            {
                return None;
            }
            next.phase = Phase::Retired;
            next.launch_attempted = false;
        }
        RestartCompletion::Forward(observed) | RestartCompletion::Rollback(observed) => {
            let forward = matches!(outcome, RestartCompletion::Forward(_));
            let process = &observed.registration.vendor;
            if !current.launch_attempted
                || current.exec_candidate(Some(process), prepare) != Some(forward)
            {
                return None;
            }
            let phase = if forward {
                Phase::Committed
            } else {
                Phase::Retired
            };
            if current.phase == Phase::Committing {
                if current.successor_pid.is_some()
                    || current.successor_birth.is_some()
                    || current.accepted_turn.is_some()
                {
                    return None;
                }
                next.phase = phase;
                next.successor_pid = Some(process.pid);
                next.successor_birth = Some(process.birth);
            } else {
                let accepted = if current.phase == Phase::Consumed && forward {
                    current.accepted_turn.as_deref() == Some(current.intent.wall.as_str())
                } else {
                    current.phase == phase && current.accepted_turn.is_none()
                };
                if !accepted
                    || current.successor_pid != Some(process.pid)
                    || current.successor_birth != Some(process.birth)
                {
                    return None;
                }
            }
        }
    }
    Some(Some(next))
}

#[cfg(test)]
#[path = "codex_renewal_completion_tests.rs"]
mod tests;
