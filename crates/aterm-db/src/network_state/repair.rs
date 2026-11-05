// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Historical repair metadata. None of these observations refresh peer presence.

use super::*;
use aterm_types::network_state::{RepairCause, RepairObservation, RepairOutcome, RepairStage};

pub(super) fn value(observation: &RepairObservation) -> Value {
    json!({
        "completed_at_ms": observation.completed_at_ms,
        "outbound": observation.outbound,
        "stage": observation.stage.map(stage),
        "outcome": outcome(observation.outcome),
        "cause": observation.cause.map(RepairCause::word),
        "needs_update": observation.cause.map(|cause| matches!(cause, RepairCause::LegacyPastCrossing | RepairCause::NeedsUpdate)),
        "table": observation.table.map(|id| aterm_codec::hex::encode_lower(&id)),
        "sent": observation.sent,
        "received": observation.received,
        "deferred": observation.deferred,
    })
}

pub(super) fn read(row: &Value) -> Result<Option<RepairObservation>> {
    let Some(value) = row.get("last_repair").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let completed_at_ms = number(value, "completed_at_ms")?;
    if completed_at_ms == 0 {
        return Err(bad());
    }
    let stage = match value.get("stage").and_then(Value::as_str) {
        Some("transport") => Some(RepairStage::Transport),
        Some("membership") => Some(RepairStage::Membership),
        Some("control") => Some(RepairStage::Control),
        Some("application") => Some(RepairStage::Application),
        Some("credentials") => Some(RepairStage::Credentials),
        None if value.get("stage").is_some_and(Value::is_null) => None,
        _ => return Err(bad()),
    };
    let outcome = match value.get("outcome").and_then(Value::as_str) {
        Some("failed") => RepairOutcome::Failed,
        Some("cancelled") => RepairOutcome::Cancelled,
        Some("deferred") => RepairOutcome::Deferred,
        Some("not-observed") => RepairOutcome::NotObserved,
        Some("no-common-table") => RepairOutcome::NoCommonTable,
        Some("exchanged") => RepairOutcome::Exchanged,
        _ => return Err(bad()),
    };
    let count = |key: &str| match value.get(key) {
        Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(bad),
        None => Err(bad()),
    };
    let sent = count("sent")?;
    let received = count("received")?;
    let deferred = count("deferred")?;
    if sent.is_some() != received.is_some() || sent.is_some() != deferred.is_some() {
        return Err(bad());
    }
    Ok(Some(RepairObservation {
        cause: match value.get("cause").and_then(Value::as_str) {
            Some("legacy-past-crossing") => Some(RepairCause::LegacyPastCrossing),
            Some("table-undeclared") => Some(RepairCause::TableUndeclared),
            Some("quota") => Some(RepairCause::Quota),
            Some("io") => Some(RepairCause::Io),
            Some("decode") => Some(RepairCause::Decode),
            Some("not-ready") => Some(RepairCause::NotReady),
            Some("needs-update") => Some(RepairCause::NeedsUpdate),
            Some("other") => Some(RepairCause::Other),
            None if value.get("cause").is_none_or(Value::is_null) => None,
            _ => return Err(bad()),
        },
        completed_at_ms,
        outbound: value
            .get("outbound")
            .and_then(Value::as_bool)
            .ok_or_else(bad)?,
        stage,
        outcome,
        table: value
            .get("table")
            .filter(|value| !value.is_null())
            .map(|_| bytes(value, "table"))
            .transpose()?,
        sent,
        received,
        deferred,
    }))
}

fn stage(stage: RepairStage) -> &'static str {
    match stage {
        RepairStage::Transport => "transport",
        RepairStage::Membership => "membership",
        RepairStage::Control => "control",
        RepairStage::Application => "application",
        RepairStage::Credentials => "credentials",
    }
}

fn outcome(outcome: RepairOutcome) -> &'static str {
    match outcome {
        RepairOutcome::Failed => "failed",
        RepairOutcome::Cancelled => "cancelled",
        RepairOutcome::Deferred => "deferred",
        RepairOutcome::NotObserved => "not-observed",
        RepairOutcome::NoCommonTable => "no-common-table",
        RepairOutcome::Exchanged => "exchanged",
    }
}

impl Observer {
    /// Only current membership may associate an attempt with an already observed incarnation.
    pub(crate) fn repair(
        &mut self,
        identity: &MachineIdentity,
        observation: RepairObservation,
        sampled_at: Instant,
    ) {
        if !self.accepting || !self.local_verified || observation.completed_at_ms == 0 {
            return;
        }
        let Some(peer) = self
            .state
            .peers
            .iter_mut()
            .find(|peer| peer.identity == *identity)
        else {
            return;
        };
        if self
            .repair_sampled
            .get(&identity.machine_id)
            .is_some_and(|old| *old > sampled_at)
        {
            return;
        }
        self.repair_sampled.insert(identity.machine_id, sampled_at);
        if peer.last_repair.as_ref() == Some(&observation) {
            return;
        }
        // Checkpoint outcome changes promptly. Counts and table scans are published live
        // and retained by the periodic checkpoint, like continuing authenticated contacts.
        self.dirty |= peer
            .last_repair
            .as_ref()
            .is_none_or(|old| old.stage != observation.stage || old.outcome != observation.outcome);
        peer.last_repair = Some(observation);
        self.changed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cause_json_round_trips_without_diagnostic_text() {
        for cause in [
            RepairCause::LegacyPastCrossing,
            RepairCause::TableUndeclared,
            RepairCause::Quota,
            RepairCause::Io,
            RepairCause::Decode,
            RepairCause::NotReady,
            RepairCause::NeedsUpdate,
        ] {
            let mut observation = failed(1000);
            observation.cause = Some(cause);
            let encoded = value(&observation);
            assert_eq!(encoded["cause"].as_str(), Some(cause.word()));
            assert_eq!(
                read(&json!({"last_repair":encoded})).unwrap(),
                Some(observation)
            );
        }
        assert!(
            read(&json!({"last_repair": {"completed_at_ms":1000,"cause":"private payload"}}))
                .is_err()
        );
    }

    fn identity(n: u8) -> MachineIdentity {
        MachineIdentity {
            machine_id: [n; 16],
            incarnation: [1; 16],
            label: None,
        }
    }

    fn failed(at: u64) -> RepairObservation {
        RepairObservation {
            cause: None,
            completed_at_ms: at,
            outbound: true,
            stage: Some(RepairStage::Application),
            outcome: RepairOutcome::Failed,
            table: Some([7; 32]),
            sent: Some(2),
            received: Some(3),
            deferred: Some(0),
        }
    }

    fn trust() -> aterm_secrets::commons::journal::Trust {
        aterm_secrets::commons::journal::Trust {
            network: [1; 16],
            authority: [2; 32],
            genesis: [3; 32],
        }
    }

    #[test]
    fn repair_history_preserves_failure_without_refreshing_contact_or_relabelling_a_peer() {
        let root = aterm_tempfile::tempdir().unwrap();
        let config = root.path().join("aterm.toml");
        let published = ObservationHandle::default();
        let mut observer = Observer::open(&config, published.clone()).unwrap();
        let peer = identity(2);
        let now = Instant::now();
        observer.local(identity(1), trust()).unwrap();
        observer.repair(&peer, failed(1500), now);
        assert!(
            observer.state.peers.is_empty(),
            "a report cannot create an observed peer"
        );
        observer.contact(peer.clone(), 1000, now);
        observer.repair(&peer, failed(1500), now);
        observer.repair(&peer, failed(1200), now - Duration::from_secs(1));
        observer.observe(
            Visibility::Active,
            0,
            "observing",
            2000,
            now + Duration::from_secs(1),
        );
        let state = published.snapshot();
        assert_eq!(state.peers[0].last_seen_ms, 1000);
        assert_eq!(state.peers[0].last_repair, Some(failed(1500)));
        let history = read_history(&config).unwrap();
        assert_eq!(history.peers[0].last_repair, Some(failed(1500)));
        assert_eq!(history.peers[0].reachability, Reachability::Unknown);
        assert_eq!(
            value(&history.peers[0].last_repair.clone().unwrap())["outcome"],
            json!("failed")
        );
        let expired = published
            .try_publication()
            .unwrap()
            .current_at(now + Duration::from_secs(60), Some(61_000));
        assert_eq!(expired.peers[0].last_repair, Some(failed(1500)));
        assert_eq!(expired.peers[0].reachability, Reachability::Unknown);

        let mut replacement = peer.clone();
        replacement.incarnation = [2; 16];
        observer.repair(&replacement, failed(2500), now + Duration::from_secs(2));
        assert_eq!(observer.state.peers[0].last_repair, Some(failed(1500)));
        observer.contact(replacement.clone(), 3000, now + Duration::from_secs(2));
        assert!(observer.state.peers[0].last_repair.is_none());
        observer.repair(&peer, failed(3500), now + Duration::from_secs(3));
        assert!(observer.state.peers[0].last_repair.is_none());
        observer.repair(&replacement, failed(3500), now + Duration::from_secs(3));
        assert_eq!(observer.state.peers[0].last_repair, Some(failed(3500)));
        observer.repair(&replacement, failed(500), now + Duration::from_secs(4));
        assert_eq!(
            observer.state.peers[0].last_repair,
            Some(failed(500)),
            "wall rollback does not suppress a later attempt"
        );
        observer.unconfirm_local();
        observer.repair(&replacement, failed(4000), now + Duration::from_secs(5));
        assert_eq!(observer.state.peers[0].last_repair, Some(failed(500)));
        let mut new_local = identity(1);
        new_local.incarnation = [2; 16];
        observer.local(new_local, trust()).unwrap();
        assert!(observer.state.peers[0].last_repair.is_none());
        assert!(observer.repair_sampled.is_empty());
    }

    #[test]
    fn old_history_is_compatible_and_malformed_repair_metadata_is_refused() {
        assert!(read(&json!({})).unwrap().is_none());
        assert!(read(&json!({"last_repair":null})).unwrap().is_none());
        let valid = json!({"last_repair":value(&failed(1000))});
        assert_eq!(read(&valid).unwrap(), Some(failed(1000)));
        let changes: [(&str, Value); 5] = [
            ("completed_at_ms", json!(0)),
            ("stage", json!("private remote text")),
            ("outcome", json!("all tables synchronized")),
            ("outbound", json!("true")),
            ("received", Value::Null),
        ];
        for (key, replacement) in changes {
            let mut invalid = valid.clone();
            invalid
                .as_object_mut()
                .unwrap()
                .get_mut("last_repair")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(key.into(), replacement);
            assert!(read(&invalid).is_err(), "{key}");
        }
    }

    #[test]
    fn repair_metadata_fits_a_full_bounded_history_checkpoint() {
        let stamp = 1_793_564_553_000;
        let mut observation = failed(stamp);
        observation.outcome = RepairOutcome::Exchanged;
        observation.sent = Some(u64::MAX);
        observation.received = Some(u64::MAX);
        observation.deferred = Some(u64::MAX);
        let state = NetworkObservation {
            observer: Some(identity(1)),
            observed_at_ms: stamp,
            peers: (0..MAX_PEERS)
                .map(|slot| PeerObservation {
                    slot: u64::try_from(slot).unwrap(),
                    identity: identity(u8::try_from(slot).unwrap()),
                    first_seen_ms: stamp,
                    last_seen_ms: stamp,
                    last_checked_ms: Some(stamp),
                    last_repair: Some(observation.clone()),
                    last_authenticated_endpoint: Some(AuthenticatedEndpoint {
                        address: "[2001:db8:aaaa:bbbb:cccc:dddd:eeee:ffff]:65535"
                            .parse()
                            .unwrap(),
                        observed_at_ms: stamp,
                    }),
                    reachability: Reachability::Reachable,
                    instances: None,
                    samples: vec![
                        PresenceSample {
                            at_ms: stamp,
                            state: Reachability::Reachable
                        };
                        MAX_SAMPLES
                    ],
                })
                .collect(),
            ..Default::default()
        };
        let wire = encode(&state, Some(trust())).unwrap();
        let decoded = decode(&wire).unwrap();
        assert_eq!(decoded.peers.len(), MAX_PEERS);
        assert!(
            decoded
                .peers
                .iter()
                .all(|peer| peer.last_repair == Some(observation.clone()))
        );
    }
}
