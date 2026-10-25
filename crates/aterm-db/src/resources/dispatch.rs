// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A durable invocation guard, not provider authority or an exactly-once effect.

use super::{Resources, ledger};
use crate::{
    Error, Result,
    table::{
        Session,
        store::{Receipt, Store},
    },
};
use astream_wire::sync::Digest;

/// The adapter's terminal observation. Neither outcome refunds capacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Rejected,
}

/// A bounded adapter observation, not an independently verified provider receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observation {
    outcome: Outcome,
    evidence: Digest,
}
impl Observation {
    /// Bind the adapter's observation to a nonzero digest of its evidence.
    pub fn new(outcome: Outcome, evidence: Digest) -> Result<Self> {
        if evidence == [0; 32] {
            return Err(Error::new(
                "dispatch observation requires an evidence digest",
            ));
        }
        Ok(Self { outcome, evidence })
    }
    pub const fn outcome(self) -> Outcome {
        self.outcome
    }
    pub const fn evidence(self) -> Digest {
        self.evidence
    }
}

/// What is known about the adapter's result and its durable observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservationState {
    /// The original adapter can still be running, or it ended without durable evidence.
    Unknown,
    Recorded(Observation),
    /// In-memory adapter evidence; persistence is unconfirmed and may have succeeded.
    Unconfirmed {
        observation: Observation,
        reason: String,
    },
}
impl ObservationState {
    pub const fn confirmed(&self) -> Option<Observation> {
        match self {
            Self::Recorded(observation) => Some(*observation),
            _ => None,
        }
    }
}

/// Retained invocation state. No field grants permission to repeat an effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    pub receipt: Receipt,
    pub operation: Digest,
    pub observation: ObservationState,
    /// Whether this call entered the adapter, not whether a remote effect occurred.
    pub invoked: bool,
}

// Only fresh durable admission creates this non-cloneable invocation permit.
pub(super) enum Admission {
    Existing(Attempt),
    Fresh(Permit),
}
pub(super) struct Permit {
    request: [u8; 16],
    cost: u64,
    attempt: Attempt,
}
pub(super) struct Invoked {
    permit: Permit,
    observation: Option<Observation>,
}
impl Invoked {
    #[cfg(test)]
    pub(super) const fn observation(&self) -> Option<Observation> {
        self.observation
    }
}
impl Permit {
    pub(super) fn invoke(mut self, run: impl FnOnce() -> Option<Observation>) -> Invoked {
        self.attempt.invoked = true;
        let observation = run();
        Invoked {
            permit: self,
            observation,
        }
    }
}

impl Resources {
    /// Charge and persist the exact attempt before entering a trusted adapter.
    /// Repeated requests only report existing state and never invoke the adapter.
    /// This deduplication is scoped to one writer incarnation and allocation;
    /// moving the same work to another writer is a different invocation.
    /// An uncertain result stays charged and cannot be automatically reissued.
    /// The caller must supply an already-authorized principal and allocation,
    /// bind every effect-relevant input into `operation`, and bound `cost`.
    /// An error means this call did not enter the adapter, not that an earlier
    /// attempt never ran. An I/O error can still leave a durable debit. After
    /// entry, persistence failures return [`ObservationState::Unconfirmed`].
    /// Adapter panics propagate.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceDispatch",
            action = "Complete",
            project = "aterm_db::resources::tests::dispatch::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceDispatch",
            action = "Reject",
            project = "aterm_db::resources::tests::dispatch::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceDispatch",
            action = "CrashBefore",
            project = "aterm_db::resources::tests::dispatch::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceDispatch",
            action = "CrashAfter",
            project = "aterm_db::resources::tests::dispatch::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceDispatch",
            action = "Unknown",
            project = "aterm_db::resources::tests::dispatch::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceDispatch",
            action = "Legacy",
            project = "aterm_db::resources::tests::dispatch::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceDispatch",
            action = "NotAdmitted",
            project = "aterm_db::resources::tests::dispatch::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceDispatch",
            action = "Retry",
            project = "aterm_db::resources::tests::dispatch::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceDispatch",
            action = "WrongOperation",
            project = "aterm_db::resources::tests::dispatch::dispatch_transition"
        )
    )]
    pub fn dispatch_once(
        &self,
        request: [u8; 16],
        cost: u64,
        operation: Digest,
        run: impl FnOnce() -> Option<Observation>,
    ) -> Result<Attempt> {
        self.dispatch_with(request, cost, operation, || {}, run)
    }

    pub(super) fn dispatch_with(
        &self,
        request: [u8; 16],
        cost: u64,
        operation: Digest,
        before_entry: impl FnOnce(),
        run: impl FnOnce() -> Option<Observation>,
    ) -> Result<Attempt> {
        match self.admit(request, cost, operation)? {
            Admission::Existing(attempt) => Ok(attempt),
            Admission::Fresh(permit) => {
                // Admission returned only after dropping all persistent locks.
                before_entry();
                Ok(self.record(permit.invoke(run)))
            }
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceDispatch",
            action = "Admit",
            project = "aterm_db::resources::tests::dispatch::dispatch_transition"
        )
    )]
    pub(super) fn admit(
        &self,
        request: [u8; 16],
        cost: u64,
        operation: Digest,
    ) -> Result<Admission> {
        if request == [0; 16] || cost == 0 || operation == [0; 32] {
            return Err(Error::new(
                "dispatch requires a nonzero request, cost and operation",
            ));
        }
        self.with_store(false, |session, store, _| {
            self.pin(false)?;
            session.writer()?;
            let ledger = ledger::Ledger::read(&self.allocation, session, store)?;
            if let Some(old) = ledger.request(&session.actor(), &request) {
                if old.cost != cost || old.operation != Some(operation) {
                    return Err(Error::new(
                        "request already reserved for different cost or operation",
                    ));
                }
                return Ok(Admission::Existing(report(session, store, old)?));
            }
            let debit = self.new_debit(session, &ledger, request, cost)?;
            let receipt = store.commit_catalog(session, &debit.encode_attempt(operation))?;
            Ok(Admission::Fresh(Permit {
                request,
                cost,
                attempt: Attempt {
                    receipt,
                    operation,
                    observation: ObservationState::Unknown,
                    invoked: false,
                },
            }))
        })
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceDispatch",
            action = "Record",
            project = "aterm_db::resources::tests::dispatch::dispatch_transition"
        )
    )]
    pub(super) fn record(&self, invoked: Invoked) -> Attempt {
        let Invoked {
            permit:
                Permit {
                    request,
                    cost,
                    mut attempt,
                },
            observation,
        } = invoked;
        let Some(observation) = observation else {
            return attempt;
        };
        let operation = attempt.operation;
        let recorded = self.with_store(false, |session, store, _| {
            self.pin(false)?;
            session.writer()?;
            let ledger = ledger::Ledger::read(&self.allocation, session, store)?;
            let old = ledger
                .request(&session.actor(), &request)
                .ok_or_else(|| Error::new("dispatch intent is missing"))?;
            if old.id != attempt.receipt.id || old.operation != Some(operation) || old.cost != cost
            {
                return Err(Error::new("dispatch intent changed before observation"));
            }
            if let Some(prior) = old.observation {
                if prior != observation {
                    return Err(Error::new("dispatch already has a different observation"));
                }
            } else {
                store.commit_catalog(
                    session,
                    &ledger::Completion {
                        allocation: self.allocation.id(),
                        request,
                        claim: attempt.receipt.id,
                        observation,
                    }
                    .encode(),
                )?;
            }
            Ok(())
        });
        attempt.observation = match recorded {
            Ok(()) => ObservationState::Recorded(observation),
            Err(why) => ObservationState::Unconfirmed {
                observation,
                reason: why.to_string(),
            },
        };
        attempt
    }

    /// Read this writer incarnation's retained attempt. Absence is no execution
    /// permission. A plain reservation produces an explicit kind-mismatch error.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceDispatch",
            action = "LoseHistory",
            project = "aterm_db::resources::tests::dispatch::dispatch_transition"
        )
    )]
    pub fn attempt(&self, request: [u8; 16]) -> Result<Option<Attempt>> {
        if request == [0; 16] {
            return Err(Error::new("dispatch lookup requires a nonzero request"));
        }
        self.with_store(false, |session, store, _| {
            self.pin(false)?;
            let ledger = ledger::Ledger::read(&self.allocation, session, store)?;
            ledger
                .request(&session.actor(), &request)
                .map(|record| report(session, store, record))
                .transpose()
        })
    }
}

fn report(session: &Session<'_>, store: &Store, record: &ledger::Recorded) -> Result<Attempt> {
    Ok(Attempt {
        receipt: store
            .receipt(session, &record.id)?
            .ok_or_else(|| Error::new("dispatch receipt is unavailable"))?,
        operation: record
            .operation
            .ok_or_else(|| Error::new("reservation has no dispatch intent"))?,
        observation: record
            .observation
            .map_or(ObservationState::Unknown, ObservationState::Recorded),
        invoked: false,
    })
}
