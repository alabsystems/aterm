// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed retained-GUI observations, independent of the live backend reader.

use super::{StageEvidence, same_identity};
use aterm_json::Value;
use aterm_types::network_state::{
    HistoryState, MachineIdentity, NetworkObservation, PeerObservation, Reachability,
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
        && recent(gui.network.observed_at_ms, 15_000)
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
        || !recent(network.observed_at_ms, 15_000)
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
        && (current.reachability != Reachability::Reachable
            || (recent(current.last_seen_ms, 15_000)
                && current.last_checked_ms.is_some_and(|at| recent(at, 15_000))))
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
mod tests {
    use super::*;
    use aterm_json::json;

    fn at_mut<'a>(value: &'a mut Value, path: &[&str]) -> &'a mut Value {
        let Some((key, rest)) = path.split_first() else {
            return value;
        };
        let next = match value {
            Value::Object(values) => values.get_mut(*key).expect("fixture object field"),
            Value::Array(values) => values
                .get_mut(key.parse::<usize>().expect("fixture array index"))
                .expect("fixture array entry"),
            _ => panic!("fixture path crosses a scalar"),
        };
        at_mut(next, rest)
    }

    fn backend() -> StageEvidence {
        StageEvidence {
            network: super::super::tests::observation(),
            account_observed_at_ms: 99_000,
            secret_observed_at_ms: 99_000,
        }
    }

    fn fixture(backend: &StageEvidence) -> Value {
        let identity = |identity: &MachineIdentity| {
            json!({
                "machine":aterm_codec::hex::encode_lower(&identity.machine_id),
                "incarnation":aterm_codec::hex::encode_lower(&identity.incarnation),
            })
        };
        let network = &backend.network;
        let peer = &network.peers[0];
        json!({"schema":1,"kind":"network-projection","composition_revision":500,
            "source_present":true,"reader_busy":false,
            "network":{"revision":network.revision,"observed_at_ms":network.observed_at_ms,
                "via_local_process":null,"observer":identity(network.observer.as_ref().unwrap()),
                "visibility":"Active","history":"Ready","peers":[{
                    "slot":peer.slot,"identity":identity(&peer.identity),
                    "first_seen_ms":peer.first_seen_ms,"last_seen_ms":peer.last_seen_ms,
                    "last_checked_ms":peer.last_checked_ms,"reachability":"reachable",
                }]},
            "services":{"accounts":{"availability":"Cached","observed_at_ms":99_500,"last_success_ms":99_500},
                "secrets":{"availability":"Unknown","observed_at_ms":99_500,"last_success_ms":null}},
        })
    }

    fn body(value: &Value) -> String {
        format!(
            "app-inspection version=app/v1 revision=700 subject=network\n{}\n",
            aterm_json::to_string(value).unwrap()
        )
    }

    #[test]
    fn follower_requires_the_current_owner_and_fresh_retained_samples() {
        let backend = backend();
        let expected = backend.network.observer.as_ref().unwrap();
        let mut value = fixture(&backend);
        *at_mut(&mut value, &["network", "via_local_process"]) = json!(42);
        // A producer reading its next sample does not invalidate the retained one.
        *at_mut(&mut value, &["reader_busy"]) = json!(true);
        assert!(follows_owner(&body(&value), 42, expected, Some(100_100)).unwrap());
        assert!(!follows_owner(&body(&value), 43, expected, Some(100_100)).unwrap());
        assert!(!follows_owner(&body(&value), 42, expected, None).unwrap());
        for (path, replacement) in [
            (vec!["source_present"], json!(false)),
            (vec!["network", "visibility"], json!("Waiting")),
            (vec!["network", "history"], json!("Unavailable")),
            (vec!["network", "observed_at_ms"], json!(1)),
            (vec!["network", "observed_at_ms"], json!(100_101)),
            (
                vec!["network", "observer", "incarnation"],
                json!("03".repeat(16)),
            ),
            (
                vec!["services", "accounts", "availability"],
                json!("Unknown"),
            ),
            (vec!["services", "accounts", "observed_at_ms"], json!(1)),
            (
                vec!["services", "accounts", "last_success_ms"],
                json!(100_101),
            ),
        ] {
            let mut refused = value.clone();
            *at_mut(&mut refused, &path) = replacement;
            assert!(!follows_owner(&body(&refused), 42, expected, Some(100_100)).unwrap());
        }
    }

    #[test]
    fn gui_wait_uses_producer_and_service_samples_instead_of_the_ui_revision() {
        let backend = backend();
        let peer = &backend.network.peers[0].identity;
        let value = fixture(&backend);
        let valid = |value: &Value| {
            matches(
                &parse(&body(value)).unwrap(),
                &backend,
                42,
                peer,
                Some(100_100),
            )
        };
        assert!(valid(&value), "direct GUI source needs no relay PID");
        let mut relayed = value.clone();
        *at_mut(&mut relayed, &["network", "via_local_process"]) = json!(42);
        assert!(valid(&relayed));
        for field in 0..19 {
            let mut stale = value.clone();
            *at_mut(&mut stale, &["composition_revision"]) = json!(999_999);
            match field {
                0 => *at_mut(&mut stale, &["network", "revision"]) = json!(9),
                1 => *at_mut(&mut stale, &["network", "observed_at_ms"]) = json!(99_999),
                2 => {
                    *at_mut(&mut stale, &["services", "accounts", "observed_at_ms"]) = json!(98_999)
                }
                3 => *at_mut(&mut stale, &["source_present"]) = json!(false),
                4 => *at_mut(&mut stale, &["reader_busy"]) = json!(true),
                5 => *at_mut(&mut stale, &["network", "via_local_process"]) = json!(43),
                6 => {
                    *at_mut(&mut stale, &["network", "observer", "incarnation"]) =
                        json!("03".repeat(16))
                }
                7 => {
                    *at_mut(&mut stale, &["network", "peers", "0", "reachability"]) =
                        json!("last-seen")
                }
                8 => *at_mut(&mut stale, &["network", "peers", "0", "slot"]) = json!(8),
                9 => {
                    *at_mut(&mut stale, &["network", "peers", "0", "first_seen_ms"]) = json!(90_001)
                }
                10 => {
                    *at_mut(&mut stale, &["network", "peers", "0", "last_seen_ms"]) = json!(98_000)
                }
                11 => {
                    *at_mut(&mut stale, &["network", "peers", "0", "last_checked_ms"]) = Value::Null
                }
                12 => *at_mut(&mut stale, &["network", "observed_at_ms"]) = json!(100_101),
                13 => {
                    *at_mut(&mut stale, &["services", "accounts", "observed_at_ms"]) =
                        json!(100_101)
                }
                14 => {
                    *at_mut(&mut stale, &["services", "accounts", "last_success_ms"]) =
                        json!(100_101)
                }
                15 => {
                    *at_mut(&mut stale, &["services", "accounts", "availability"]) =
                        json!("Unknown")
                }
                16 => {
                    *at_mut(&mut stale, &["services", "secrets", "availability"]) = json!("Cached")
                }
                17 => {
                    *at_mut(&mut stale, &["services", "secrets", "observed_at_ms"]) = json!(98_999)
                }
                _ => {
                    *at_mut(&mut stale, &["services", "secrets", "observed_at_ms"]) = json!(100_101)
                }
            }
            assert!(!valid(&stale), "invalid observation field {field}");
        }
        let projection = parse(&body(&value)).unwrap();
        assert!(!matches(&projection, &backend, 42, peer, None));
        assert!(!matches(&projection, &backend, 42, peer, Some(115_001)));
        let mut old_contact = value.clone();
        *at_mut(&mut old_contact, &["network", "observed_at_ms"]) = json!(116_000);
        assert!(
            !matches(
                &parse(&body(&old_contact)).unwrap(),
                &backend,
                42,
                peer,
                Some(116_001),
            ),
            "a fresh heartbeat cannot renew old peer contact"
        );
        let old_service_backend = StageEvidence {
            network: backend.network.clone(),
            account_observed_at_ms: 1,
            secret_observed_at_ms: backend.secret_observed_at_ms,
        };
        let mut old_service = value;
        *at_mut(
            &mut old_service,
            &["services", "accounts", "observed_at_ms"],
        ) = json!(1);
        *at_mut(
            &mut old_service,
            &["services", "accounts", "last_success_ms"],
        ) = json!(1);
        assert!(!matches(
            &parse(&body(&old_service)).unwrap(),
            &old_service_backend,
            42,
            peer,
            Some(100_100)
        ));
    }

    #[test]
    fn gui_projection_rejects_malformed_versions_types_and_ambiguous_peers() {
        let value = fixture(&backend());
        let good = body(&value);
        for bad in [
            good.replace("app/v1", "app/v2"),
            good.replace("subject=network", "subject=tabs"),
            good.replace("revision=700", "revision=unknown"),
            format!("{good}extra\n"),
        ] {
            assert!(parse(&bad).is_err());
        }
        for field in 0..8 {
            let mut bad = value.clone();
            match field {
                0 => *at_mut(&mut bad, &["schema"]) = json!(2),
                1 => *at_mut(&mut bad, &["kind"]) = json!("other"),
                2 => *at_mut(&mut bad, &["reader_busy"]) = json!("false"),
                3 => *at_mut(&mut bad, &["network", "observer", "machine"]) = json!("ab"),
                4 => *at_mut(&mut bad, &["network", "visibility"]) = json!("Maybe"),
                5 => {
                    *at_mut(&mut bad, &["services", "accounts", "observed_at_ms"]) = json!("99500")
                }
                6 => {
                    let mut peer = bad["network"]["peers"][0].clone();
                    *at_mut(&mut peer, &["identity", "machine"]) = json!("03".repeat(16));
                    at_mut(&mut bad, &["network", "peers"])
                        .as_array_mut()
                        .unwrap()
                        .push(peer);
                }
                _ => {
                    let mut peer = bad["network"]["peers"][0].clone();
                    *at_mut(&mut peer, &["slot"]) = json!(8);
                    at_mut(&mut bad, &["network", "peers"])
                        .as_array_mut()
                        .unwrap()
                        .push(peer);
                }
            }
            assert!(
                parse(&body(&bad)).is_err(),
                "malformed observation field {field}"
            );
        }
    }

    #[test]
    fn a_valid_pre_capture_projection_cannot_authorize_a_different_post_capture_state() {
        let backend = backend();
        let peer = &backend.network.peers[0].identity;
        let before = fixture(&backend);
        let mut after = before.clone();
        *at_mut(&mut after, &["network", "revision"]) = json!(11);
        *at_mut(&mut after, &["network", "observed_at_ms"]) = json!(100_001);
        *at_mut(&mut after, &["network", "peers", "0", "reachability"]) = json!("unknown");
        assert!(matches(
            &parse(&body(&before)).unwrap(),
            &backend,
            42,
            peer,
            Some(100_100)
        ));
        assert!(!matches(
            &parse(&body(&after)).unwrap(),
            &backend,
            42,
            peer,
            Some(100_100)
        ));
    }

    #[test]
    fn restored_gui_requires_retained_contact_and_absent_lifetime_check() {
        let mut backend = backend();
        backend.network.via_local_process = Some(43);
        backend.network.peers[0].reachability = Reachability::LastSeen;
        backend.network.peers[0].last_checked_ms = None;
        let peer = &backend.network.peers[0].identity;
        let mut value = fixture(&backend);
        *at_mut(&mut value, &["network", "peers", "0", "reachability"]) = json!("last-seen");
        let valid = |value: &Value| {
            matches(
                &parse(&body(value)).unwrap(),
                &backend,
                43,
                peer,
                Some(100_100),
            )
        };
        assert!(valid(&value));
        for field in 0..6 {
            let mut wrong = value.clone();
            match field {
                0 => {
                    *at_mut(&mut wrong, &["network", "peers", "0", "last_seen_ms"]) = json!(99_001)
                }
                1 => {
                    *at_mut(&mut wrong, &["network", "peers", "0", "last_checked_ms"]) =
                        json!(99_000)
                }
                2 => {
                    *at_mut(&mut wrong, &["network", "peers", "0", "reachability"]) =
                        json!("reachable")
                }
                3 => *at_mut(&mut wrong, &["network", "via_local_process"]) = json!(42),
                4 => *at_mut(&mut wrong, &["network", "peers", "0", "slot"]) = json!(8),
                _ => {
                    *at_mut(&mut wrong, &["network", "peers", "0", "first_seen_ms"]) = json!(90_001)
                }
            }
            assert!(!valid(&wrong), "field {field}");
        }
    }
}
