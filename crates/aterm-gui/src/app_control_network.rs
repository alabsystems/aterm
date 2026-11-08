// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read the GUI's retained network projection without advancing its producers.

use std::hash::{Hash, Hasher};

use aterm_json::{Value, json};
use aterm_types::app_inspection::{InspectionEnvelope, InspectionSubject};
use aterm_types::network_state::{
    MachineIdentity, Reachability, ServiceState, VaultNetworkRefreshOutcome, VaultNetworkSource,
};

fn identity(value: &MachineIdentity) -> Value {
    json!({
        "machine": aterm_codec::hex::encode_lower(&value.machine_id),
        "incarnation": aterm_codec::hex::encode_lower(&value.incarnation),
    })
}

fn vault_source(value: VaultNetworkSource) -> &'static str {
    match value {
        VaultNetworkSource::ConfiguredUrl => "configured_url",
        VaultNetworkSource::PeerUrl => "peer_url",
    }
}

fn service(value: &ServiceState) -> Value {
    // Make the wire decision explicit for each service field; free text stays private.
    let ServiceState {
        availability,
        observed_at_ms,
        last_success_ms,
        network_materialization,
        http204_response,
        network_refresh,
        revision: _,
        detail: _,
        refresh_failed,
    } = value;
    json!({
        "availability": format!("{availability:?}"),
        "observed_at_ms": observed_at_ms,
        "last_success_ms": last_success_ms,
        "refresh_failed": refresh_failed,
        // Historical producer evidence never promotes availability or renews a timestamp.
        "network_materialization": network_materialization.map(|receipt| json!({
            "source": vault_source(receipt.source),
            "sequence": receipt.sequence,
            "sampled_at_ms": receipt.sampled_at_ms,
        })),
        "http204_response": http204_response.map(|response| json!({
            "source": vault_source(response.source),
            "responded_at_ms": response.responded_at_ms,
        })),
        "network_refresh": network_refresh.map(|refresh| json!({
            "completed_at_ms": refresh.completed_at_ms,
            "outcome": match refresh.outcome {
                VaultNetworkRefreshOutcome::Succeeded => "Succeeded",
                VaultNetworkRefreshOutcome::Failed => "Failed",
            },
        })),
    })
}

/// How long an inspection of the projection keeps the window reading the services: a minute,
/// the time one of their samples stays current.
pub(crate) const INSPECTED_FOR: std::time::Duration = std::time::Duration::from_secs(60);

impl crate::App {
    /// An inspection of the projection reports the services' states, so after one the window
    /// reads the services, drawn or not, each second for [`INSPECTED_FOR`]
    /// ([`crate::App::network_state_deadline`]). The inspection itself still neither reads nor
    /// ages them ([`inspect`]): its turn read before it, so the first after a minute uninspected
    /// replies with the services' aged sample, and its next turn reads them at once, asking for
    /// the sample a later inspection replies with.
    pub(crate) fn note_inspection(
        &mut self,
        request: &super::InspectRequest,
        now: std::time::Instant,
    ) {
        if matches!(request, super::InspectRequest::Network) {
            if !self.network_inspected_at(now) {
                self.network_next_poll = None;
            }
            self.network_inspected = Some(now);
        }
    }

    /// Whether an inspection at most [`INSPECTED_FOR`] before `now` asks for the services.
    pub(crate) fn network_inspected_at(&self, now: std::time::Instant) -> bool {
        self.network_inspected
            .is_some_and(|at| now.saturating_duration_since(at) <= INSPECTED_FOR)
    }
}

pub(super) fn inspect(app: &crate::App) -> Result<Vec<String>, String> {
    let snapshot = &app.network_snapshot;
    let network = &snapshot.network;
    // Deliberately do not refresh or expire this snapshot here. A reader must be
    // able to distinguish what the GUI adopted from what its producer now knows.
    let value = json!({
        "schema": 1,
        "kind": "network-projection",
        "composition_revision": snapshot.revision,
        "source_present": app.network_source.is_some(),
        "reader_busy": app.network_source_busy,
        "network": {
            "revision": network.revision,
            "observed_at_ms": network.observed_at_ms,
            "via_local_process": network.via_local_process,
            "observer": network.observer.as_ref().map(identity),
            "visibility": format!("{:?}", network.visibility),
            "history": format!("{:?}", network.history),
            "peers": network.peers.iter().map(|peer| json!({
                "slot": peer.slot,
                "identity": identity(&peer.identity),
                "first_seen_ms": peer.first_seen_ms,
                "last_seen_ms": peer.last_seen_ms,
                "last_checked_ms": peer.last_checked_ms,
                "reachability": match peer.reachability {
                    Reachability::Reachable => "reachable",
                    Reachability::LastSeen => "last-seen",
                    Reachability::Unknown => "unknown",
                },
            })).collect::<Vec<_>>(),
        },
        "services": {
            "accounts": service(&snapshot.services.accounts),
            "secrets": service(&snapshot.services.secrets),
        },
    });
    let body = aterm_json::to_string(&value).map_err(|error| error.to_string())?;
    let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut fingerprint);
    Ok(vec![
        InspectionEnvelope {
            revision: super::inspection_revision(fingerprint.finish()),
            subject: InspectionSubject::Network,
        }
        .header_line(),
        body,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_types::network_state::{
        HistoryState, NetworkStateSnapshot, PeerObservation, ServiceAvailability,
        VaultHttp204Response, VaultNetworkMaterialization, VaultNetworkRefresh,
        VaultNetworkRefreshOutcome, VaultNetworkSource, Visibility,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn retained_gui_network_inspection_does_not_refresh_producers() {
        let mut app = crate::App::headless_for_test();
        let mut retained = NetworkStateSnapshot {
            revision: 10,
            ..Default::default()
        };
        retained.network.revision = 20;
        retained.network.observed_at_ms = 10_000;
        retained.network.visibility = Visibility::Active;
        retained.network.history = HistoryState::Ready;
        retained.network.detail = "not-exposed-network-detail".into();
        let peer = MachineIdentity {
            machine_id: [7; 16],
            incarnation: [8; 16],
            label: Some("not-exposed-machine-label".into()),
        };
        retained.network.observer = Some(peer.clone());
        retained.network.peers.push(PeerObservation {
            slot: 3,
            identity: peer,
            first_seen_ms: 7_000,
            last_seen_ms: 9_000,
            last_checked_ms: Some(9_000),
            last_authenticated_endpoint: None,
            last_repair: None,
            reachability: Reachability::Reachable,
            instances: None,
            samples: Vec::new(),
        });
        retained.services.accounts.availability = ServiceAvailability::Cached;
        retained.services.accounts.observed_at_ms = 9_500;
        retained.services.secrets.availability = ServiceAvailability::Cached;
        retained.services.secrets.observed_at_ms = 9_400;
        retained.services.secrets.network_materialization = Some(VaultNetworkMaterialization {
            source: VaultNetworkSource::ConfiguredUrl,
            sequence: 7,
            sampled_at_ms: 8_000,
        });
        retained.services.secrets.http204_response = Some(VaultHttp204Response {
            source: VaultNetworkSource::PeerUrl,
            responded_at_ms: 8_500,
        });
        retained.services.secrets.network_refresh = Some(VaultNetworkRefresh {
            completed_at_ms: 9_000,
            outcome: VaultNetworkRefreshOutcome::Succeeded,
        });
        retained.services.secrets.detail = "not-exposed-secret-detail".into();
        retained.services.secrets.revision = Some("not-exposed-service-revision".into());
        let retained = Arc::new(retained);
        app.network_snapshot = Arc::clone(&retained);
        let reads = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&reads);
        app.network_source = Some(Arc::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Some(Arc::new(NetworkStateSnapshot {
                revision: 99,
                ..Default::default()
            }))
        }));
        app.network_source_busy = true;
        let due = std::time::Instant::now() + std::time::Duration::from_secs(60);
        app.network_next_poll = Some(due);
        let mut inspected = Vec::new();
        for (availability, refresh_failed, outcome, materialized) in [
            (
                ServiceAvailability::Cached,
                false,
                Some(VaultNetworkRefreshOutcome::Succeeded),
                true,
            ),
            (
                ServiceAvailability::Cached,
                true,
                Some(VaultNetworkRefreshOutcome::Failed),
                true,
            ),
            (ServiceAvailability::Unknown, true, None, false),
            (
                ServiceAvailability::Unavailable,
                false,
                Some(VaultNetworkRefreshOutcome::Succeeded),
                true,
            ),
        ] {
            let mut publication = retained.as_ref().clone();
            publication.services.secrets.availability = availability;
            publication.services.secrets.refresh_failed = refresh_failed;
            publication.services.secrets.network_refresh =
                outcome.map(|outcome| VaultNetworkRefresh {
                    completed_at_ms: 9_000,
                    outcome,
                });
            if !materialized {
                publication.services.secrets.network_materialization = None;
            }
            if availability == ServiceAvailability::Unavailable {
                if let Some(receipt) = &mut publication.services.secrets.network_materialization {
                    receipt.source = VaultNetworkSource::PeerUrl;
                }
                if let Some(response) = &mut publication.services.secrets.http204_response {
                    response.source = VaultNetworkSource::ConfiguredUrl;
                }
            }
            let publication = Arc::new(publication);
            app.network_snapshot = Arc::clone(&publication);
            let mut previous_body = None;
            for repeat in 0..2 {
                let lines = app
                    .inspect_app(super::super::InspectRequest::Network)
                    .unwrap();
                assert!(lines[0].ends_with(" subject=network"));
                let value: Value = aterm_json::from_str(&lines[1]).unwrap();
                assert_eq!(value["schema"].as_u64(), Some(1));
                assert_eq!(value["composition_revision"].as_u64(), Some(10));
                assert_eq!(value["network"]["revision"].as_u64(), Some(20));
                assert_eq!(value["network"]["observed_at_ms"].as_u64(), Some(10_000));
                assert_eq!(value["network"]["visibility"].as_str(), Some("Active"));
                assert_eq!(
                    value["network"]["peers"][0]["reachability"].as_str(),
                    Some("reachable")
                );
                assert_eq!(value["reader_busy"].as_bool(), Some(true));
                assert_eq!(value["source_present"].as_bool(), Some(true));
                assert_eq!(
                    value["services"]["accounts"]["availability"].as_str(),
                    Some("Cached")
                );
                assert!(!lines[1].contains("not-exposed-"));
                if let Some(body) = previous_body.replace(lines[1].clone()) {
                    assert_eq!(lines[1], body);
                }
                if repeat == 0 {
                    inspected.push((
                        lines,
                        value,
                        availability,
                        refresh_failed,
                        outcome,
                        materialized,
                    ));
                }
            }
            assert_eq!(reads.load(Ordering::SeqCst), 0);
            assert!(Arc::ptr_eq(&app.network_snapshot, &publication));
            assert_eq!(app.network_snapshot.as_ref(), publication.as_ref());
            assert!(app.network_source_busy);
            assert_eq!(app.network_next_poll, Some(due));
        }
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert!(app.network_source_busy);
        assert_eq!(app.network_next_poll, Some(due));

        // Positive control: only the ordinary scheduled read adopts the new publication.
        let before_scheduled = Arc::clone(&app.network_snapshot);
        app.service_network_state(due);
        let lines = app
            .inspect_app(super::super::InspectRequest::Network)
            .unwrap();
        let value: Value = aterm_json::from_str(&lines[1]).unwrap();
        assert_eq!(value["composition_revision"].as_u64(), Some(99));
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert!(!app.network_source_busy);
        assert!(!Arc::ptr_eq(&app.network_snapshot, &before_scheduled));

        assert_ne!(
            inspected[0].0[1], inspected[1].0[1],
            "owner inspection must distinguish healthy and failed refreshes of the same Cached service"
        );
        assert_ne!(inspected[0].0[0], inspected[1].0[0]);
        for (_, value, availability, refresh_failed, outcome, materialized) in inspected {
            let accounts = &value["services"]["accounts"];
            assert_eq!(accounts["refresh_failed"].as_bool(), Some(false));
            for key in [
                "network_refresh",
                "network_materialization",
                "http204_response",
            ] {
                assert_eq!(accounts.get(key), Some(&Value::Null), "{key}");
            }
            let secrets = &value["services"]["secrets"];
            assert_eq!(secrets["availability"], json!(format!("{availability:?}")));
            assert_eq!(secrets["observed_at_ms"].as_u64(), Some(9_400));
            assert_eq!(secrets["last_success_ms"], Value::Null);
            assert_eq!(secrets["refresh_failed"].as_bool(), Some(refresh_failed));
            let expected_refresh = outcome.map_or(Value::Null, |outcome| {
                json!({
                    "completed_at_ms":9_000,
                    "outcome":match outcome {
                        VaultNetworkRefreshOutcome::Succeeded => "Succeeded",
                        VaultNetworkRefreshOutcome::Failed => "Failed",
                    },
                })
            });
            assert_eq!(secrets.get("network_refresh"), Some(&expected_refresh));
            let (materialization_source, response_source) =
                if availability == ServiceAvailability::Unavailable {
                    ("peer_url", "configured_url")
                } else {
                    ("configured_url", "peer_url")
                };
            let expected_materialization = if materialized {
                json!({"source":materialization_source, "sequence":7, "sampled_at_ms":8_000})
            } else {
                Value::Null
            };
            assert_eq!(
                secrets.get("network_materialization"),
                Some(&expected_materialization)
            );
            assert_eq!(
                secrets.get("http204_response"),
                Some(&json!({
                    "source":response_source, "responded_at_ms":8_500,
                }))
            );
        }
    }
}
