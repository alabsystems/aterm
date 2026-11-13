// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn commons_status_text_shows_the_same_cause_as_json() {
    let value = json!({"state":"ready","ready":true,"repairs":{"visibility":"Active","peers":[
        {"slot":1,"last_repair":{"outcome":"failed","cause":"legacy-past-crossing","needs_update":true}},
        {"slot":2,"last_repair":{"outcome":"failed","cause":"not-ready","needs_update":false}}
    ]}});
    assert_eq!(
        commons_status_text(&value, None),
        "Commons: ready\n\
         peer 1: needs-update (legacy-past-crossing)\n\
         peer 2: failed (not-ready)"
    );
}

/// A Mac whose LAN waits for its broker, with day-old failed attempts (shaped like m22's
/// status: busy custody, 43 failures from about 21 hours before): the text says what the Mac
/// waits for and why Commons is not ready, and each attempt says how old it is, identical ones
/// folded into one line that names their slots.
#[test]
fn commons_status_text_names_what_it_waits_for_and_how_old_each_attempt_is() {
    const NOW: u64 = 1_793_988_193_881;
    const DAY_AGO: u64 = 1_793_912_949_398;
    let failed = |slot: u64, at: u64, cause: &str| {
        json!({"slot":slot,"last_repair":{"completed_at_ms":at,"outbound":true,
            "stage":if cause == "lan-held" {"transport"} else {"application"},
            "outcome":"failed","cause":cause,"needs_update":false,
            "sent":0,"received":0,"deferred":0}})
    };
    let mut peers: Vec<Value> = (1..=43)
        .map(|slot| failed(slot, DAY_AGO, "lan-held"))
        .collect();
    peers[4] = failed(5, DAY_AGO, "not-ready");
    peers.push(failed(45, DAY_AGO - 3 * 86_400_000, "lan-held"));
    peers.push(json!({"slot":44,"last_repair":Value::Null}));
    peers.push(
        json!({"slot":46,"last_repair":{"completed_at_ms":NOW - 90_000,
        "outbound":false,"stage":"application","outcome":"exchanged","cause":null,
        "needs_update":null,"sent":1,"received":2,"deferred":0}}),
    );
    let value = json!({"state":"writer-recovery-pending","ready":false,
        "detail":"Local Commons custody, control journal or vault is busy; membership has not been checked",
        "repairs":{"visibility":"Waiting","observed_at_ms":NOW,
            "detail":"Waits for the LAN broker, which setup's Mac password installs",
            "peers":peers}});
    assert_eq!(
        commons_status_text(&value, Some(NOW)),
        "Commons: writer-recovery-pending\n\
         \x20 Local Commons custody, control journal or vault is busy; membership has not been checked\n\
         Repairs (waiting): Waits for the LAN broker, which setup's Mac password installs\n\
         peers 1-4, 6-43: failed 20h ago (lan-held)\n\
         peer 5: failed 20h ago (not-ready)\n\
         peer 45: failed 3d ago (lan-held)\n\
         peer 46: exchanged 1m ago"
    );
    // A reader whose clock is behind a record, or that has none, prints no age.
    let text = commons_status_text(&value, Some(DAY_AGO - 1));
    assert!(
        text.contains("peers 1-4, 6-43: failed (lan-held)\n"),
        "{text}"
    );
    assert!(text.ends_with("peer 46: exchanged"), "{text}");
    assert!(!commons_status_text(&value, None).contains(" ago"));
}

#[test]
fn commons_status_text_names_an_unreadable_repair_history() {
    let value = json!({"state":"ready","ready":true,
        "repairs":{"visibility":"Unavailable","peers":[],"cause":"io"}});
    assert_eq!(
        commons_status_text(&value, None),
        "Commons: ready\nRepairs (unavailable): repair history unreadable (io)"
    );
}

#[test]
fn ages_are_one_coarse_unit_and_slots_fold_into_runs() {
    assert_eq!(
        [
            0,
            59_999,
            60_000,
            3_599_999,
            3_600_000,
            172_799_999,
            172_800_000
        ]
        .map(age),
        ["0s", "59s", "1m", "59m", "1h", "47h", "2d"]
    );
    assert_eq!(slot_ranges(&[1, 2, 3, 5, 7, 8]), "1-3, 5, 7-8");
    assert_eq!(slot_ranges(&[4]), "4");
}
use aterm_types::network_state::{
    HistoryState, NetworkObservation, ServiceAvailability, ServiceState, ServiceStates, Visibility,
};
use std::{
    cell::Cell,
    time::{Duration, Instant},
};

fn network(at: Instant, wall: u64) -> crate::network_state::Publication {
    crate::network_state::Publication::new(
        NetworkObservation {
            observed_at_ms: wall,
            visibility: Visibility::Active,
            history: HistoryState::Ready,
            via_local_process: Some(7),
            revision: 23,
            ..NetworkObservation::default()
        },
        at,
    )
}

fn services(at: Instant, wall: u64) -> crate::service_state::Publication {
    let state = ServiceState {
        availability: ServiceAvailability::Cached,
        observed_at_ms: wall,
        revision: Some("retained revision".into()),
        ..ServiceState::default()
    };
    crate::service_state::Publication::new(
        ServiceStates {
            accounts: state.clone(),
            secrets: state,
        },
        at,
    )
}

#[test]
fn unavailable_network_place_is_reported_explicitly() {
    let at = Instant::now();
    let value = network_state_value_with(
        network(at, 100_000),
        None,
        services(at, 100_000),
        || None,
        || (at, Some(100_000)),
    );
    assert_eq!(value.get("place"), Some(&json!({"why":"not_sampled"})));
    assert_eq!(value["live_network"], json!(true));
}

#[test]
fn network_place_uses_the_common_fields_without_rewriting_access() {
    let at = Instant::now();
    let value = network_state_value_with(
        network(at, 100_000),
        None,
        services(at, 100_000),
        || Some(aterm_netplace::sample::hotspot(100_000, 90_000)),
        || (at, Some(100_000)),
    );
    let place = &value["place"];
    assert_eq!(place["state"], json!("remote"));
    assert_eq!(place["kind"], json!("hotspot"));
    assert_eq!(place["address"], json!("172.20.10.2"));
    assert_eq!(place["network_strong"], json!(true));
    assert_eq!(place["expensive"], json!(true));
    assert_eq!(place["constrained"], json!(false));
    assert_eq!(place["internet"]["state"], json!("path-up"));
    assert_eq!(place["internet"]["source"], json!("path"));
    assert_eq!(
        place["internet"]["checked_ms"],
        json!({"why":"not_sampled"})
    );
    assert_eq!(place["alab"]["state"], json!("away"));
    assert_eq!(place["alab"]["known"], Value::Null, "no member count");
    assert_eq!(place["alab"]["reachable"], Value::Null, "no member count");
    assert_eq!(place["alab"]["since_ms"], json!(90_000));
    assert_eq!(value["live_network"], json!(true));
    assert_eq!(
        value["services"]["accounts"]["availability"],
        json!("Cached")
    );
}

#[test]
fn a_known_network_place_does_not_grant_observer_or_service_access() {
    let at = Instant::now();
    let value = network_state_value_with(
        crate::network_state::Publication::new(
            NetworkObservation {
                observed_at_ms: 100_000,
                history: HistoryState::Ready,
                visibility: Visibility::Unavailable,
                ..NetworkObservation::default()
            },
            at,
        ),
        None,
        crate::service_state::Publication::new(ServiceStates::default(), at),
        || Some(aterm_netplace::sample::home(100_000, 12)),
        || (at, Some(100_000)),
    );
    assert_eq!(value["place"]["state"], json!("alab-lan"));
    assert_eq!(value["place"]["alab"]["state"], json!("here"));
    assert_eq!(value["live_observation"], json!(false));
    assert_eq!(value["live_network"], json!(false));
    for service in ["accounts", "secrets"] {
        assert_eq!(value["services"][service]["availability"], json!("Unknown"));
    }
}

#[test]
fn network_place_io_cannot_renew_observer_or_service_leases() {
    let at = Instant::now();
    let now = Cell::new((at, Some(100_000)));
    let value = network_state_value_with(
        network(at, 100_000),
        None,
        services(at, 100_000),
        || {
            now.set((at + Duration::from_secs(61), Some(161_000)));
            Some(aterm_netplace::sample::hotspot(100_000, 90_000))
        },
        || now.get(),
    );
    assert_eq!(value["place"]["state"], json!("remote"));
    assert_eq!(value["live_network"], json!(false));
    assert_eq!(value["network"]["visibility"], json!("Unavailable"));
    assert_eq!(value["network"]["observed_at_ms"], json!(100_000));
    for service in ["accounts", "secrets"] {
        assert_eq!(value["services"][service]["availability"], json!("Unknown"));
        assert_eq!(value["services"][service]["observed_at_ms"], json!(100_000));
    }
}

#[test]
fn vault_network_receipt_json_remains_historical_after_local_observation_expires() {
    use aterm_types::network_state::{
        VaultNetworkMaterialization, VaultNetworkRefresh, VaultNetworkRefreshOutcome,
        VaultNetworkSource,
    };
    let at = Instant::now();
    let mut states = ServiceStates::default();
    states.secrets.availability = ServiceAvailability::Cached;
    states.secrets.observed_at_ms = 100_000;
    states.secrets.network_materialization = Some(VaultNetworkMaterialization {
        source: VaultNetworkSource::PeerUrl,
        sequence: 17,
        sampled_at_ms: 80_000,
    });
    states.secrets.refresh_failed = true;
    states.secrets.network_refresh = Some(VaultNetworkRefresh {
        completed_at_ms: 90_000,
        outcome: VaultNetworkRefreshOutcome::Failed,
    });
    let value = network_state_value_with(
        network(at, 100_000),
        None,
        crate::service_state::Publication::new(states, at),
        || None,
        || (at + Duration::from_secs(61), Some(161_000)),
    );
    assert_eq!(
        value["services"]["secrets"]["availability"],
        json!("Unknown")
    );
    assert_eq!(value["services"]["secrets"]["last_success_ms"], Value::Null);
    assert_eq!(value["services"]["secrets"]["refresh_failed"], json!(true));
    assert_eq!(
        value["services"]["secrets"]["network_refresh"],
        json!({"outcome":"Failed", "completed_at_ms":90_000})
    );
    assert_eq!(
        value["services"]["secrets"]["network_materialization"],
        json!({
            "source":"peer_url", "sequence":17, "sampled_at_ms":80_000,
        })
    );
    assert_eq!(
        value["services"]["accounts"]["network_materialization"],
        Value::Null
    );
    assert_eq!(
        value["services"]["accounts"]["network_refresh"],
        Value::Null
    );
}

#[test]
fn a_retained_204_json_does_not_establish_access_or_signed_materialization() {
    use aterm_types::network_state::{
        VaultHttp204Response, VaultNetworkRefresh, VaultNetworkRefreshOutcome, VaultNetworkSource,
    };
    let at = Instant::now();
    let mut states = ServiceStates::default();
    states.secrets.availability = ServiceAvailability::Unavailable;
    states.secrets.observed_at_ms = 100_000;
    states.secrets.http204_response = Some(VaultHttp204Response {
        source: VaultNetworkSource::ConfiguredUrl,
        responded_at_ms: 80_000,
    });
    states.secrets.network_refresh = Some(VaultNetworkRefresh {
        completed_at_ms: 90_000,
        outcome: VaultNetworkRefreshOutcome::Succeeded,
    });
    let value = network_state_value_with(
        network(at, 100_000),
        None,
        crate::service_state::Publication::new(states, at),
        || None,
        || (at + Duration::from_secs(61), Some(161_000)),
    );
    let secrets = &value["services"]["secrets"];
    assert_eq!(secrets["availability"], json!("Unknown"));
    assert_eq!(secrets["last_success_ms"], Value::Null);
    assert_eq!(secrets["network_materialization"], Value::Null);
    assert_eq!(secrets["refresh_failed"], json!(false));
    assert_eq!(
        secrets["network_refresh"],
        json!({"outcome":"Succeeded", "completed_at_ms":90_000})
    );
    assert_eq!(
        secrets["http204_response"],
        json!({
            "source":"configured_url", "responded_at_ms":80_000,
        })
    );
    assert_eq!(
        value["services"]["accounts"]["http204_response"],
        Value::Null
    );
}

/// A relayed publication is as old as its owner's sample, so reading it after the services
/// renews nothing.
#[test]
fn delayed_service_metadata_cannot_keep_an_expired_network_current() {
    let at = Instant::now();
    let sampled = at + Duration::from_secs(14);
    let now = Cell::new((sampled, Some(114_000)));
    let value = read_network_state_with(
        || Ok(network(at, 100_000)),
        || panic!("a live read must not open retained history"),
        || {
            let publication = services(sampled, 114_000);
            now.set((at + Duration::from_secs(16), Some(116_000)));
            publication
        },
        || None,
        || now.get(),
    )
    .unwrap();
    assert_eq!(value["live_network"], json!(false));
    assert_eq!(value["network"]["visibility"], json!("Unavailable"));
    assert_eq!(value["network"]["observed_at_ms"], json!(100_000));
    assert_eq!(value["network"]["revision"], json!(23));
    assert_eq!(
        value["services"]["secrets"]["availability"],
        json!("Cached")
    );
}

/// The network's short lease starts at its owner's latest periodic sample, whose age a relayed
/// read keeps, so a healthy owner stays healthy however long the services took to read before
/// the read of the network.
#[test]
fn a_slow_service_read_does_not_expire_a_healthy_network() {
    let at = Instant::now();
    let now = Cell::new((at, 100_000));
    let value = read_network_state_with(
        || {
            let (at, wall) = now.get();
            Ok(network(at, wall))
        },
        || panic!("a live read must not open retained history"),
        || {
            let publication = services(at, 100_000);
            now.set((at + Duration::from_secs(70), 170_000));
            publication
        },
        || None,
        || {
            let (at, wall) = now.get();
            (at, Some(wall))
        },
    )
    .unwrap();
    assert_eq!(value["live_network"], json!(true));
    assert_eq!(value["network"]["visibility"], json!("Active"));
    assert_eq!(value["network"]["observed_at_ms"], json!(170_000));
    // The services are as old as their read, and say so.
    assert_eq!(
        value["services"]["accounts"]["availability"],
        json!("Unknown")
    );
}

#[test]
fn current_network_and_retained_history_keep_their_distinct_evidence() {
    let at = Instant::now();
    let now = at + Duration::from_secs(14);
    let current = network_state_value_with(
        network(at, 100_000),
        None,
        crate::service_state::Publication::new(
            ServiceStates {
                accounts: ServiceState {
                    availability: ServiceAvailability::Cached,
                    observed_at_ms: 50_000,
                    ..ServiceState::default()
                },
                secrets: ServiceState {
                    availability: ServiceAvailability::Cached,
                    observed_at_ms: 114_000,
                    ..ServiceState::default()
                },
            },
            now,
        ),
        || None,
        || (now, Some(114_000)),
    );
    assert_eq!(current["live_network"], json!(true));
    assert_eq!(
        current["services"]["accounts"]["availability"],
        json!("Unknown")
    );
    assert_eq!(
        current["services"]["secrets"]["availability"],
        json!("Cached")
    );
    let retained = crate::network_state::Publication::new(
        NetworkObservation {
            visibility: Visibility::Unavailable,
            history: HistoryState::Ready,
            observed_at_ms: 100_000,
            revision: 23,
            ..NetworkObservation::default()
        },
        now,
    );
    let history = network_state_value_with(
        retained,
        None,
        services(now, 114_000),
        || None,
        || (now, Some(114_000)),
    );
    assert_eq!(history["live_observation"], json!(false));
    assert_eq!(history["live_network"], json!(false));
    assert_eq!(history["network"]["revision"], json!(23));
    assert_eq!(history["network"]["observed_at_ms"], json!(100_000));
}

#[test]
fn failed_live_read_reports_its_kind_without_exposing_private_errors() {
    let at = Instant::now();
    let private = "snapshot-json: private diagnostic payload";
    for (kind, code) in [
        (std::io::ErrorKind::InvalidData, "InvalidData"),
        (std::io::ErrorKind::NotFound, "NotFound"),
    ] {
        let value = read_network_state_with(
            || Err(std::io::Error::new(kind, private)),
            || {
                Ok(crate::network_state::Publication::new(
                    NetworkObservation {
                        visibility: Visibility::Unavailable,
                        history: HistoryState::Ready,
                        observed_at_ms: 100_000,
                        revision: 23,
                        ..NetworkObservation::default()
                    },
                    at,
                ))
            },
            || services(at, 100_000),
            || None,
            || (at, Some(100_000)),
        )
        .unwrap();
        assert_eq!(value["live_observation_error"], json!(code));
        assert_eq!(
            value.get("live_observation_error_stage"),
            Some(&Value::Null)
        );
        assert_eq!(value["live_observation"], json!(false));
        assert_eq!(value["live_network"], json!(false));
        assert_eq!(value["network"]["history"], json!("Ready"));
        assert_eq!(value["network"]["revision"], json!(23));
        assert_eq!(value["network"]["observed_at_ms"], json!(100_000));
        assert!(!aterm_json::to_string(&value).unwrap().contains(private));
    }
    let live = read_network_state_with(
        || Ok(network(at, 100_000)),
        || panic!("a live read must not open retained history"),
        || services(at, 100_000),
        || None,
        || (at, Some(100_000)),
    )
    .unwrap();
    assert_eq!(live.get("live_observation_error"), Some(&Value::Null));
    assert_eq!(live.get("live_observation_error_stage"), Some(&Value::Null));
    assert_eq!(live["live_network"], json!(true));
}

#[test]
fn failed_native_endpoint_reports_safe_stage_and_retains_history() {
    let root = aterm_tempfile::tempdir().unwrap();
    let private = "private-observer-diagnostic";
    let config = root.path().join(private).join("aterm.toml");
    let error = crate::network_state::read_live_publication(&config).unwrap_err();
    assert!(error.to_string().contains(private));
    let at = Instant::now();
    let value = read_network_state_with(
        || Err(error),
        || {
            Ok(crate::network_state::Publication::new(
                NetworkObservation {
                    visibility: Visibility::Unavailable,
                    history: HistoryState::Ready,
                    observed_at_ms: 100_000,
                    revision: 23,
                    ..NetworkObservation::default()
                },
                at,
            ))
        },
        || services(at, 100_000),
        || None,
        || (at, Some(100_000)),
    )
    .unwrap();
    assert_eq!(value["live_observation_error"], json!("NotFound"));
    assert_eq!(value["live_observation_error_stage"], json!("endpoint"));
    assert_eq!(value["live_observation"], json!(false));
    assert_eq!(value["live_network"], json!(false));
    assert_eq!(value["network"]["revision"], json!(23));
    assert_eq!(value["network"]["observed_at_ms"], json!(100_000));
    assert!(!aterm_json::to_string(&value).unwrap().contains(private));
}

#[test]
fn a_failed_live_read_cannot_hide_a_failed_history_read() {
    let result = read_network_state_with(
        || Err(std::io::ErrorKind::NotFound.into()),
        || Err(Error::new("history unavailable")),
        || services(Instant::now(), 100_000),
        || panic!("failed history must be returned before place reads"),
        || panic!("failed history supplies no publication"),
    );
    assert!(result.is_err());
}
