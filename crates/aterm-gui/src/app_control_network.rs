// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read the GUI's retained network projection without advancing its producers.

use std::hash::{Hash, Hasher};

use aterm_json::{Value, json};
use aterm_types::app_inspection::{InspectionEnvelope, InspectionSubject};
use aterm_types::network_state::{MachineIdentity, Reachability, ServiceState};

fn identity(value: &MachineIdentity) -> Value {
    json!({
        "machine": aterm_codec::hex::encode_lower(&value.machine_id),
        "incarnation": aterm_codec::hex::encode_lower(&value.incarnation),
    })
}

fn service(value: &ServiceState) -> Value {
    json!({
        "availability": format!("{:?}", value.availability),
        "observed_at_ms": value.observed_at_ms,
        "last_success_ms": value.last_success_ms,
    })
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
        HistoryState, NetworkStateSnapshot, PeerObservation, ServiceAvailability, Visibility,
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
        retained.services.secrets.detail = "not-exposed-secret-detail".into();
        retained.services.secrets.revision = Some("not-exposed-service-revision".into());
        let retained = Arc::new(retained);
        app.network_snapshot = Arc::clone(&retained);
        let reads = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&reads);
        app.network_source = Some(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Some(Arc::new(NetworkStateSnapshot {
                revision: 99,
                ..Default::default()
            }))
        }));
        app.network_source_busy = true;
        let due = std::time::Instant::now() + std::time::Duration::from_secs(60);
        app.network_next_poll = Some(due);
        for _ in 0..2 {
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
        }
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert!(Arc::ptr_eq(&app.network_snapshot, &retained));
        assert_eq!(app.network_snapshot.as_ref(), retained.as_ref());
        assert!(app.network_source_busy);
        assert_eq!(app.network_next_poll, Some(due));

        // Positive control: only the ordinary scheduled read adopts the new publication.
        app.service_network_state(due);
        let lines = app
            .inspect_app(super::super::InspectRequest::Network)
            .unwrap();
        let value: Value = aterm_json::from_str(&lines[1]).unwrap();
        assert_eq!(value["composition_revision"].as_u64(), Some(99));
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert!(!app.network_source_busy);
        assert!(!Arc::ptr_eq(&app.network_snapshot, &retained));
    }
}
