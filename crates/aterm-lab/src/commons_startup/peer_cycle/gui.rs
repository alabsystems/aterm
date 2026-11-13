// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed retained-GUI observations, independent of the live backend reader.

use super::{StageEvidence, same_identity};
use aterm_json::Value;
use aterm_types::network_state::{
    HistoryState, MachineIdentity, NetworkObservation, PeerObservation, REACHABLE_MS, Reachability,
    ServiceAvailability, ServiceState, Visibility,
};
use std::collections::BTreeSet;

pub(super) struct Projection {
    source_present: bool,
    reader_busy: bool,
    network: NetworkObservation,
    accounts: ServiceState,
    secrets: ServiceState,
}

fn number(value: &Value, key: &str) -> Result<u64, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("invalid GUI field {key}"))
}

fn optional_number(value: &Value, key: &str) -> Result<Option<u64>, String> {
    let value = value
        .get(key)
        .ok_or_else(|| format!("missing GUI field {key}"))?;
    if value.is_null() {
        Ok(None)
    } else {
        value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("invalid GUI field {key}"))
    }
}

fn identity(value: &Value) -> Result<MachineIdentity, String> {
    let bytes = |key: &str| {
        let text = value
            .get(key)
            .and_then(Value::as_str)
            .ok_or("invalid GUI identity")?;
        let mut bytes = [0; 16];
        if !aterm_codec::hex::decode_into(text.as_bytes(), &mut bytes) {
            return Err("invalid GUI identity hex".to_owned());
        }
        Ok(bytes)
    };
    Ok(MachineIdentity {
        machine_id: bytes("machine")?,
        incarnation: bytes("incarnation")?,
        label: None,
    })
}

fn service(value: &Value) -> Result<ServiceState, String> {
    let availability = match value.get("availability").and_then(Value::as_str) {
        Some("Unknown") => ServiceAvailability::Unknown,
        Some("Unavailable") => ServiceAvailability::Unavailable,
        Some("Cached") => ServiceAvailability::Cached,
        Some("Current") => ServiceAvailability::Current,
        _ => return Err("invalid GUI service availability".into()),
    };
    Ok(ServiceState {
        availability,
        observed_at_ms: number(value, "observed_at_ms")?,
        last_success_ms: optional_number(value, "last_success_ms")?,
        ..Default::default()
    })
}

pub(super) fn parse(body: &str) -> Result<Projection, String> {
    let mut lines = body.lines();
    let header = lines.next().ok_or("missing GUI inspection envelope")?;
    let mut words = header.split_whitespace();
    if words.next() != Some("app-inspection")
        || words.next() != Some("version=app/v1")
        || !words
            .next()
            .and_then(|word| word.strip_prefix("revision="))
            .is_some_and(|revision| {
                !revision.is_empty()
                    && revision.bytes().all(|b| b.is_ascii_digit())
                    && revision.parse::<u64>().is_ok()
            })
        || words.next() != Some("subject=network")
        || words.next().is_some()
    {
        return Err("invalid GUI network inspection envelope".into());
    }
    let value: Value = aterm_json::from_str(lines.next().ok_or("missing GUI network JSON")?)
        .map_err(|error| format!("invalid GUI network JSON: {error}"))?;
    if lines.next().is_some()
        || value["schema"].as_u64() != Some(1)
        || value["kind"].as_str() != Some("network-projection")
    {
        return Err("unsupported GUI network projection schema".into());
    }
    number(&value, "composition_revision")?;
    let flag = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_bool)
            .ok_or_else(|| format!("invalid GUI flag {key}"))
    };
    let source_present = flag("source_present")?;
    let reader_busy = flag("reader_busy")?;
    let wire = &value["network"];
    let visibility = match wire["visibility"].as_str() {
        Some("Checking") => Visibility::Checking,
        Some("Active") => Visibility::Active,
        Some("Offline") => Visibility::Offline,
        Some("Disabled") => Visibility::Disabled,
        Some("Waiting") => Visibility::Waiting,
        Some("Unavailable") => Visibility::Unavailable,
        _ => return Err("invalid GUI visibility".into()),
    };
    let history = match wire["history"].as_str() {
        Some("Restoring") => HistoryState::Restoring,
        Some("Ready") => HistoryState::Ready,
        Some("Unavailable") => HistoryState::Unavailable,
        _ => return Err("invalid GUI history".into()),
    };
    let observer = match wire.get("observer").ok_or("missing GUI observer")? {
        Value::Null => None,
        value => Some(identity(value)?),
    };
    let mut slots = BTreeSet::new();
    let mut machines = BTreeSet::new();
    let mut peers = Vec::new();
    for row in wire["peers"].as_array().ok_or("invalid GUI peers")? {
        let slot = number(row, "slot")?;
        let identity = identity(&row["identity"])?;
        if !slots.insert(slot) || !machines.insert(identity.machine_id) {
            return Err("ambiguous GUI peer identity or slot".into());
        }
        let reachability = match row["reachability"].as_str() {
            Some("reachable") => Reachability::Reachable,
            Some("last-seen") => Reachability::LastSeen,
            Some("unknown") => Reachability::Unknown,
            _ => return Err("invalid GUI peer reachability".into()),
        };
        peers.push(PeerObservation {
            slot,
            identity,
            first_seen_ms: number(row, "first_seen_ms")?,
            last_seen_ms: number(row, "last_seen_ms")?,
            last_checked_ms: optional_number(row, "last_checked_ms")?,
            reachability,
            last_authenticated_endpoint: None,
            last_repair: None,
            instances: None,
            samples: Vec::new(),
            lease_ms: optional_number(row, "lease_ms")?,
        });
    }
    let via_local_process = optional_number(wire, "via_local_process")?
        .map(u32::try_from)
        .transpose()
        .map_err(|_| "invalid GUI observer PID")?;
    let accounts = service(&value["services"]["accounts"])?;
    let secrets = service(&value["services"]["secrets"])?;
    Ok(Projection {
        source_present,
        reader_busy,
        accounts,
        secrets,
        network: NetworkObservation {
            revision: number(wire, "revision")?,
            observed_at_ms: number(wire, "observed_at_ms")?,
            via_local_process,
            observer,
            visibility,
            history,
            peers,
            ..Default::default()
        },
    })
}

/// A fresh GUI observation from a different process owning the shared vault.
pub(in crate::commons_startup) fn follows_owner(
    body: &str,
    owner: u32,
    expected: &MachineIdentity,
    wall_ms: Option<u64>,
) -> Result<bool, String> {
    let gui = parse(body)?;
    let Some(now) = wall_ms else {
        return Ok(false);
    };
    let recent = |stamp, max| stamp != 0 && now.checked_sub(stamp).is_some_and(|age| age <= max);
    Ok(gui.source_present
        && gui.network.visibility == Visibility::Active
        && gui.network.history == HistoryState::Ready
        && gui.network.via_local_process == Some(owner)
        && gui
            .network
            .observer
            .as_ref()
            .is_some_and(|identity| same_identity(identity, expected))
        && recent(gui.network.observed_at_ms, REACHABLE_MS)
        && gui.accounts.availability == ServiceAvailability::Cached
        && recent(gui.accounts.observed_at_ms, 60_000)
        && gui
            .accounts
            .last_success_ms
            .is_none_or(|stamp| stamp <= gui.accounts.observed_at_ms))
}

pub(super) fn matches(
    gui: &Projection,
    backend: &StageEvidence,
    pid: u32,
    peer: &MachineIdentity,
    wall_ms: Option<u64>,
) -> bool {
    let Some(now) = wall_ms else {
        return false;
    };
    let recent = |stamp, max| stamp != 0 && now.checked_sub(stamp).is_some_and(|age| age <= max);
    let network = &gui.network;
    let accounts = &gui.accounts;
    let secrets = &gui.secrets;
    if !gui.source_present
        || gui.reader_busy
        || network.via_local_process.is_some_and(|via| via != pid)
        || network.visibility != Visibility::Active
        || network.history != HistoryState::Ready
        || network.revision < backend.network.revision
        || network.observed_at_ms < backend.network.observed_at_ms
        || !recent(network.observed_at_ms, REACHABLE_MS)
        || accounts.availability != ServiceAvailability::Cached
        || accounts.observed_at_ms < backend.account_observed_at_ms
        || !recent(accounts.observed_at_ms, 60_000)
        || accounts
            .last_success_ms
            .is_some_and(|stamp| stamp > accounts.observed_at_ms)
        || secrets.availability != ServiceAvailability::Unknown
        || secrets.observed_at_ms < backend.secret_observed_at_ms
        || !recent(secrets.observed_at_ms, 60_000)
        || secrets
            .last_success_ms
            .is_some_and(|stamp| stamp > secrets.observed_at_ms)
        || !network
            .observer
            .as_ref()
            .zip(backend.network.observer.as_ref())
            .is_some_and(|(gui, backend)| same_identity(gui, backend))
    {
        return false;
    }
    let current = network
        .peers
        .iter()
        .find(|p| same_identity(&p.identity, peer));
    let expected = backend
        .network
        .peers
        .iter()
        .find(|p| same_identity(&p.identity, peer));
    let (Some(current), Some(expected)) = (current, expected) else {
        return false;
    };
    current.slot == expected.slot
        && current.first_seen_ms == expected.first_seen_ms
        && current.reachability == expected.reachability
        && if expected.last_checked_ms.is_none() {
            current.last_seen_ms == expected.last_seen_ms
        } else {
            current.last_seen_ms >= expected.last_seen_ms
        }
        && current.last_seen_ms <= network.observed_at_ms
        // A Reachable row is current by the lease its observer measured for that peer.
        && current.reachability_at(Some(now)) == current.reachability
        && match expected.last_checked_ms {
            None => {
                current.last_checked_ms.is_none() && current.reachability != Reachability::Reachable
            }
            Some(expected) => current
                .last_checked_ms
                .is_some_and(|current| current >= expected && current <= network.observed_at_ms),
        }
}

#[cfg(test)]
#[path = "gui_tests.rs"]
mod tests;
