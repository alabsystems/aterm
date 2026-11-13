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
                "lease_ms": peer.lease_ms,
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
#[path = "app_control_network_tests.rs"]
mod tests;
