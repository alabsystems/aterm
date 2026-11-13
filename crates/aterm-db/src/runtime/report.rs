// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded metadata from completed automatic repair workers. No payload or key
//! material leaves the transport through this observation.

use crate::control::Failure;
use astream_wire::sync::Actor;
use std::net::SocketAddr;

/// Socket endpoints and original clocks for an authenticated exchange. This proves
/// contact only; the subsequent control or application exchange may have failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthenticatedConnection {
    /// The actual socket's local endpoint, captured before authentication.
    pub local: SocketAddr,
    /// The actual socket's peer, including an accepted connection's source port.
    pub peer: SocketAddr,
    /// When the attempt began, before connecting or authenticating.
    pub began_ms: u64,
    /// When the peer authenticated, never the later report's collection time.
    pub authenticated_ms: u64,
}

/// One automatic owner's completed worker, including failed or cancelled work.
/// An authenticated actor proves this round's identity exchange, not successful
/// control repair, application repair or globally converged state.
#[derive(Clone, Debug)]
pub struct RepairRound {
    /// The advertised service dialled by an outbound worker; None for inbound.
    pub target: Option<SocketAddr>,
    pub actor: Option<Actor>,
    /// Authenticated socket metadata, independent of later repair success.
    pub connection: Option<AuthenticatedConnection>,
    pub exchange_started: bool,
    pub cancelled: bool,
    /// Last local operation entered; None when a worker panicked before reporting.
    pub phase: Option<crate::control::Phase>,
    /// The control exchange's report, or its typed failure. Each failure below carries its own
    /// value-free cause beside bounded local text, so an observation of a failed round always
    /// names one.
    pub control: std::result::Result<crate::control::Report, Failure>,
    /// Signed membership records retained independently of application access.
    pub membership: Option<crate::control::Report>,
    pub membership_error: Option<Failure>,
    pub application: Option<crate::replica::Report>,
    pub application_error: Option<Failure>,
    /// Lower bounds observed before an application error. These are not a
    /// completed round or proof that the peer received local receipts.
    pub application_interrupted: Option<crate::replica::Report>,
    /// Why a completed round deferred an unavailable inventory. The local
    /// error is bounded like other worker errors; the peer's cause is unknown.
    pub application_deferral: Option<crate::replica::InventoryDeferral>,
    /// First local refusal in a received batch, independently of inventory
    /// availability. The reason contains bounded metadata, never a wire value.
    pub application_batch_deferral: Option<crate::replica::BatchDeferral>,
    /// Optional credential exchange; values and ciphertext never enter this report.
    pub handoff: Option<std::result::Result<usize, Failure>>,
    /// Process-local completion order, independent of wall-clock adjustment.
    pub completed_at: std::time::Instant,
    pub completed_at_ms: u64,
    /// Earlier metadata evicted from this bounded queue before this record.
    /// This reports an observation gap, not missing application operations.
    pub dropped_before: usize,
}

pub(super) fn bounded_error(error: impl std::fmt::Display) -> String {
    let mut value = error.to_string();
    const MAX: usize = 512;
    if value.len() > MAX {
        let mut end = MAX - 3;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
        value.push('…');
    }
    value
}

/// `failure` with its text bounded like other worker errors; its cause is unchanged.
pub(super) fn bounded_failure(failure: &Failure) -> Failure {
    Failure::new(failure.cause, bounded_error(&failure.text))
}

impl RepairRound {
    /// A completed, authenticated round around `application`.
    #[cfg(test)]
    pub(crate) fn completed_with(application: crate::replica::Report) -> Self {
        let mut round = Self::panicked(None);
        round.actor = Some([1; 48]);
        round.exchange_started = true;
        round.control = Ok(Default::default());
        round.application = Some(application);
        round
    }

    /// The table this round left nothing to fetch of, from an authenticated peer that holds at
    /// least one of its operations: no local or peer refusal, nothing deferred and nothing
    /// received, so a read now misses nothing that peer had when the round read its inventory. A
    /// round that received something may have left more for a later batch, and a receive-only
    /// recovery round reads from a store this Mac's writer does not hold.
    pub fn drained_table(&self) -> Option<astream_wire::sync::Digest> {
        if self.cancelled
            || !self.exchange_started
            || self.actor.is_none()
            || self.control.is_err()
            || self.membership_error.is_some()
            || self.application_error.is_some()
            || self.application_interrupted.is_some()
            || self.application_deferral.is_some()
            || self.application_batch_deferral.is_some()
        {
            return None;
        }
        let report = self.application.as_ref()?;
        let drained = !report.recovery
            && report.deferred == 0
            && report.received == 0
            && report.conflicts_received == 0
            && report.peer_known.is_some_and(|known| known.count > 0);
        report.table.filter(|_| drained)
    }

    /// A local diagnostic for this attempt's authenticated peer. Error strings,
    /// credentials and payloads never enter the published observation.
    pub(crate) fn observation(
        &self,
    ) -> Option<(
        Actor,
        aterm_types::network_state::RepairObservation,
        std::time::Instant,
    )> {
        use aterm_types::network_state::{
            RepairCause, RepairObservation, RepairOutcome, RepairStage,
        };
        if !self.exchange_started || self.completed_at_ms == 0 {
            return None;
        }
        let actor = self.actor?;
        let application = self.application.or(self.application_interrupted);
        // A local deferral's cause: an unavailable inventory's local refusal, else a batch's.
        let deferral = self
            .application_deferral
            .as_ref()
            .and_then(|why| why.local_cause)
            .or_else(|| {
                self.application_batch_deferral
                    .as_ref()
                    .map(|why| why.cause)
            });
        let (stage, outcome) = if self.cancelled {
            (self.phase.map(stage), RepairOutcome::Cancelled)
        } else if let Some(failure) = &self.membership_error {
            (
                Some(RepairStage::Membership),
                RepairOutcome::Failed(failure.cause),
            )
        } else if let Some(failure) = &self.application_error {
            (
                Some(RepairStage::Application),
                RepairOutcome::Failed(failure.cause),
            )
        } else if self.application_interrupted.is_some() {
            // The replica records an interruption only beside the exchange error that
            // `application_error` carries; a round assembled with one alone names no category.
            (
                Some(RepairStage::Application),
                RepairOutcome::Failed(RepairCause::Other),
            )
        } else if let Some(Err(failure)) = &self.handoff {
            (
                Some(RepairStage::Credentials),
                RepairOutcome::Failed(failure.cause),
            )
        } else if let Err(failure) = &self.control {
            (self.phase.map(stage), RepairOutcome::Failed(failure.cause))
        } else if self.application_deferral.is_some()
            || self.application_batch_deferral.is_some()
            || application.is_some_and(|report| report.deferred != 0)
        {
            (
                Some(RepairStage::Application),
                RepairOutcome::Deferred(deferral),
            )
        } else if self.membership.is_some_and(|report| report.deferred != 0) {
            (Some(RepairStage::Membership), RepairOutcome::Deferred(None))
        } else if self
            .control
            .as_ref()
            .is_ok_and(|report| report.deferred != 0)
        {
            (Some(RepairStage::Control), RepairOutcome::Deferred(None))
        } else {
            (
                Some(RepairStage::Application),
                match application {
                    None => RepairOutcome::NotObserved,
                    Some(report) if report.table.is_none() => RepairOutcome::NoCommonTable,
                    Some(_) => RepairOutcome::Exchanged,
                },
            )
        };
        Some((
            actor,
            RepairObservation {
                completed_at_ms: self.completed_at_ms,
                outbound: self.target.is_some(),
                stage,
                outcome,
                table: application.and_then(|report| report.table),
                sent: application.and_then(|report| u64::try_from(report.sent).ok()),
                received: application.and_then(|report| u64::try_from(report.received).ok()),
                deferred: application.and_then(|report| u64::try_from(report.deferred).ok()),
            },
            self.completed_at,
        ))
    }

    /// A matching retained-ID observation only after this authenticated round completed.
    /// It makes no statement about account projections, provider access, or current currency.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "NoActor",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "ForeignActor",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "NotStarted",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "Cancelled",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "ControlError",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "MembershipError",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "MissingApplication",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "ApplicationError",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "Interrupted",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "InventoryDeferral",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "BatchDeferral",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "HandoffError",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "MissingWitness",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "CompletedScopeChanged",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "CompletedInventoryChanged",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "CompletedDeferred",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "CompletedPeerInventoryChanged",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "CompletedPeerInventoryMissing",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    pub fn matching_inventory(&self) -> Option<crate::replica::MatchingInventory> {
        if !self.exchange_started
            || self.cancelled
            || self.control.is_err()
            || self.membership_error.is_some()
            || self.application_error.is_some()
            || self.application_interrupted.is_some()
            || self.application_deferral.is_some()
            || self.application_batch_deferral.is_some()
            || self.handoff.as_ref().is_some_and(Result::is_err)
        {
            return None;
        }
        let application = self.application?;
        let witness = application.matching_inventory?;
        (self.actor == Some(witness.peer_actor())
            && application.table == Some(witness.scope().digest())
            && application.local_known == Some(witness.inventory())
            && application.peer_known == Some(witness.inventory())
            && application.deferred == 0
            && witness.observed_at_ms() != 0)
            .then_some(witness)
    }

    /// A successful outbound identity exchange. Repair/application currency is separate.
    pub(super) fn authenticated_endpoint(
        &self,
    ) -> Option<(Actor, aterm_types::network_state::AuthenticatedEndpoint)> {
        if !self.exchange_started
            || self.cancelled
            || self.control.is_err()
            || self.membership_error.is_some()
            || self.application_error.is_some()
            || self.application_interrupted.is_some()
            || self.handoff.as_ref().is_some_and(Result::is_err)
            || self.completed_at_ms == 0
        {
            return None;
        }
        Some((
            self.actor?,
            aterm_types::network_state::AuthenticatedEndpoint {
                address: self.target?,
                observed_at_ms: self.completed_at_ms,
            },
        ))
    }

    pub(super) fn panicked(target: Option<SocketAddr>) -> Self {
        Self {
            target,
            actor: None,
            connection: None,
            exchange_started: false,
            cancelled: false,
            phase: None,
            control: Err(Failure::new(
                aterm_types::network_state::RepairCause::Other,
                "automatic repair worker panicked",
            )),
            membership: None,
            membership_error: None,
            application: None,
            application_error: None,
            application_interrupted: None,
            application_deferral: None,
            application_batch_deferral: None,
            handoff: None,
            completed_at: std::time::Instant::now(),
            completed_at_ms: aterm_time::unix_millis().unwrap_or(0),
            dropped_before: 0,
        }
    }
}

/// Only a clean round on a table some peer holds, with nothing left to fetch, is evidence: one
/// failed, cancelled, deferred or recovery round is not, nor one that still received something,
/// nor one with a peer that knows nothing of the table.
/// NEGATIVE CONTROL: with `drained_table` not requiring `received == 0`, the `received` case below
/// answers the table.
#[cfg(test)]
#[test]
fn only_a_clean_round_that_left_nothing_to_fetch_is_evidence() {
    use crate::replica::{InventorySummary, Report};
    let table = [7; 32];
    let clean = || {
        RepairRound::completed_with(Report {
            table: Some(table),
            peer_known: Some(InventorySummary::of(&[[9; 32]].into_iter().collect())),
            ..Default::default()
        })
    };
    assert_eq!(clean().drained_table(), Some(table));
    assert_eq!(RepairRound::panicked(None).drained_table(), None);
    fn no() -> Failure {
        Failure::new(aterm_types::network_state::RepairCause::Other, "no")
    }
    let edited: [fn(&mut RepairRound); 12] = [
        |r| r.cancelled = true,
        |r| r.exchange_started = false,
        |r| r.actor = None,
        |r| r.control = Err(no()),
        |r| r.membership_error = Some(no()),
        |r| r.application_error = Some(no()),
        |r| r.application = None,
        |r| r.application.as_mut().unwrap().received = 1,
        |r| r.application.as_mut().unwrap().deferred = 1,
        |r| r.application.as_mut().unwrap().recovery = true,
        |r| r.application.as_mut().unwrap().table = None,
        |r| {
            r.application.as_mut().unwrap().peer_known =
                Some(InventorySummary::of(&Default::default()));
        },
    ];
    for (n, edit) in edited.iter().enumerate() {
        let mut round = clean();
        edit(&mut round);
        assert_eq!(round.drained_table(), None, "case {n}");
    }
}

fn stage(phase: crate::control::Phase) -> aterm_types::network_state::RepairStage {
    use crate::control::Phase;
    use aterm_types::network_state::RepairStage;
    match phase {
        Phase::Connecting | Phase::Route | Phase::Tls => RepairStage::Transport,
        Phase::Membership | Phase::PeerAdmission | Phase::MembershipRecords => {
            RepairStage::Membership
        }
        Phase::ControlInventory
        | Phase::InventorySend
        | Phase::InventoryReceive
        | Phase::ControlBatch
        | Phase::BatchSend
        | Phase::BatchReceive
        | Phase::ControlComplete => RepairStage::Control,
        Phase::Application | Phase::VersionReport | Phase::Complete => RepairStage::Application,
        Phase::Credentials => RepairStage::Credentials,
    }
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
