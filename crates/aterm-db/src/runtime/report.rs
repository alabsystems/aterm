// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded metadata from completed automatic repair workers. No payload or key
//! material leaves the transport through this observation.

use astream_wire::sync::Actor;
use std::net::SocketAddr;

/// One automatic owner's completed worker, including failed or cancelled work.
/// An authenticated actor proves this round's identity exchange, not successful
/// control repair, application repair or globally converged state.
#[derive(Clone, Debug)]
pub struct RepairRound {
    /// The advertised service dialled by an outbound worker; None for inbound.
    pub target: Option<SocketAddr>,
    pub actor: Option<Actor>,
    pub exchange_started: bool,
    pub cancelled: bool,
    /// Last local operation entered; None when a worker panicked before reporting.
    pub phase: Option<crate::control::Phase>,
    pub control: std::result::Result<crate::control::Report, String>,
    /// Signed membership records retained independently of application access.
    pub membership: Option<crate::control::Report>,
    pub membership_error: Option<String>,
    pub application: Option<crate::replica::Report>,
    pub application_error: Option<String>,
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
    pub handoff: Option<std::result::Result<usize, String>>,
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

impl RepairRound {
    /// A local diagnostic for this attempt's authenticated peer. Error strings,
    /// credentials and payloads never enter the published observation.
    pub(super) fn observation(
        &self,
    ) -> Option<(
        Actor,
        aterm_types::network_state::RepairObservation,
        std::time::Instant,
    )> {
        use aterm_types::network_state::{RepairObservation, RepairOutcome, RepairStage};
        if !self.exchange_started || self.completed_at_ms == 0 {
            return None;
        }
        let actor = self.actor?;
        let application = self.application.or(self.application_interrupted);
        let (stage, outcome) = if self.cancelled {
            (self.phase.map(stage), RepairOutcome::Cancelled)
        } else if self.membership_error.is_some() {
            (Some(RepairStage::Membership), RepairOutcome::Failed)
        } else if self.application_error.is_some() || self.application_interrupted.is_some() {
            (Some(RepairStage::Application), RepairOutcome::Failed)
        } else if self.handoff.as_ref().is_some_and(Result::is_err) {
            (Some(RepairStage::Credentials), RepairOutcome::Failed)
        } else if self.control.is_err() {
            (self.phase.map(stage), RepairOutcome::Failed)
        } else if self.application_deferral.is_some()
            || self.application_batch_deferral.is_some()
            || application.is_some_and(|report| report.deferred != 0)
        {
            (Some(RepairStage::Application), RepairOutcome::Deferred)
        } else if self.membership.is_some_and(|report| report.deferred != 0) {
            (Some(RepairStage::Membership), RepairOutcome::Deferred)
        } else if self
            .control
            .as_ref()
            .is_ok_and(|report| report.deferred != 0)
        {
            (Some(RepairStage::Control), RepairOutcome::Deferred)
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
            exchange_started: false,
            cancelled: false,
            phase: None,
            control: Err("automatic repair worker panicked".into()),
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
mod tests {
    use super::*;

    fn attempted() -> RepairRound {
        let mut round = RepairRound::panicked(Some("127.0.0.1:41384".parse().unwrap()));
        round.actor = Some([7; 48]);
        round.exchange_started = true;
        round.phase = Some(crate::control::Phase::Application);
        round.control = Ok(Default::default());
        round.completed_at_ms = 1000;
        round
    }

    #[test]
    fn repair_diagnostics_keep_failures_and_partial_counts_without_error_text() {
        use aterm_types::network_state::{RepairOutcome, RepairStage};
        let mut round = attempted();
        round.application_error = Some("private fixture error and payload".into());
        round.application_interrupted = Some(crate::replica::Report {
            table: Some([2; 32]),
            sent: 3,
            received: 4,
            deferred: 5,
            ..Default::default()
        });
        let (actor, observation, _) = round.observation().unwrap();
        assert_eq!(actor, [7; 48]);
        assert_eq!(observation.outcome, RepairOutcome::Failed);
        assert_eq!(observation.stage, Some(RepairStage::Application));
        assert_eq!(observation.table, Some([2; 32]));
        assert_eq!(observation.sent, Some(3));
        assert_eq!(observation.received, Some(4));
        assert_eq!(observation.deferred, Some(5));
        assert_eq!(observation.completed_at_ms, 1000);
        assert!(observation.outbound);
        assert!(!format!("{observation:?}").contains("private fixture"));
        for change in [
            (None, true, 1000),
            (Some([7; 48]), false, 1000),
            (Some([7; 48]), true, 0),
        ] {
            let mut invalid = round.clone();
            (
                invalid.actor,
                invalid.exchange_started,
                invalid.completed_at_ms,
            ) = change;
            assert!(invalid.observation().is_none());
        }
    }

    #[test]
    fn an_authenticated_round_does_not_imply_an_application_exchange() {
        use aterm_types::network_state::{RepairOutcome, RepairStage};
        let mut round = attempted();
        assert_eq!(
            round.observation().unwrap().1.outcome,
            RepairOutcome::NotObserved
        );
        round.application = Some(Default::default());
        assert_eq!(
            round.observation().unwrap().1.outcome,
            RepairOutcome::NoCommonTable
        );
        round.application.as_mut().unwrap().table = Some([1; 32]);
        assert_eq!(
            round.observation().unwrap().1.outcome,
            RepairOutcome::Exchanged
        );
        round.application.as_mut().unwrap().deferred = 1;
        assert_eq!(
            round.observation().unwrap().1.outcome,
            RepairOutcome::Deferred
        );
        round.application.as_mut().unwrap().deferred = 0;
        round.membership = Some(crate::control::Report {
            deferred: 1,
            ..Default::default()
        });
        assert_eq!(
            round.observation().unwrap().1.stage,
            Some(RepairStage::Membership)
        );
        round.membership = None;
        round.control.as_mut().unwrap().deferred = 1;
        assert_eq!(
            round.observation().unwrap().1.stage,
            Some(RepairStage::Control)
        );
        round.control.as_mut().unwrap().deferred = 0;
        round.handoff = Some(Err("private fixture credential error".into()));
        assert_eq!(
            round.observation().unwrap().1.stage,
            Some(RepairStage::Credentials)
        );
        assert_eq!(
            round.observation().unwrap().1.outcome,
            RepairOutcome::Failed
        );
        round.cancelled = true;
        assert_eq!(
            round.observation().unwrap().1.outcome,
            RepairOutcome::Cancelled
        );
    }

    #[test]
    fn only_successful_outbound_rounds_expose_authenticated_endpoints() {
        let address = "127.0.0.1:41384".parse().unwrap();
        let mut round = RepairRound::panicked(Some(address));
        round.actor = Some([7; 48]);
        round.exchange_started = true;
        round.phase = Some(crate::control::Phase::ControlComplete);
        round.control = Ok(crate::control::Report::default());
        round.completed_at_ms = 1000;
        let (actor, endpoint) = round.authenticated_endpoint().unwrap();
        assert_eq!(actor, [7; 48]);
        assert_eq!(endpoint.address, address);
        assert_eq!(endpoint.observed_at_ms, 1000);

        let failures: [fn(&mut RepairRound); 10] = [
            |r| r.target = None,
            |r| r.actor = None,
            |r| r.exchange_started = false,
            |r| r.cancelled = true,
            |r| r.control = Err("control failed after authentication".into()),
            |r| r.membership_error = Some("membership failed".into()),
            |r| r.application_error = Some("application failed".into()),
            |r| r.application_interrupted = Some(crate::replica::Report::default()),
            |r| r.handoff = Some(Err("handoff failed".into())),
            |r| r.completed_at_ms = 0,
        ];
        for (index, fail) in failures.into_iter().enumerate() {
            let mut failed = round.clone();
            fail(&mut failed);
            assert!(
                failed.authenticated_endpoint().is_none(),
                "failure {index} promoted an endpoint"
            );
        }
    }
}
