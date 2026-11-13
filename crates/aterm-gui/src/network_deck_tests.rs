// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_types::network_state::{NetworkObservation, PeerObservation};

fn peer(slot: u64, reachability: Reachability) -> PeerObservation {
    PeerObservation {
        slot,
        identity: MachineIdentity {
            machine_id: [slot as u8; 16],
            incarnation: [1; 16],
            label: Some(format!("m{slot}")),
        },
        first_seen_ms: 1,
        last_seen_ms: 1000,
        last_checked_ms: Some(2000),
        last_repair: None,
        last_authenticated_endpoint: None,
        reachability,
        instances: None,
        samples: Vec::new(),
        lease_ms: None,
    }
}

#[test]
fn an_initial_busy_read_invents_no_prior_observation() {
    let unread = std::sync::Arc::new(NetworkStateSnapshot::default());
    let mut busy = false;
    let snapshot = snapshot_after_read(None, &unread, &mut busy, Some(100_000));
    assert!(busy);
    assert_eq!(snapshot.revision, 0);
    assert_eq!(snapshot.network.observed_at_ms, 0);
    assert_eq!(snapshot.network.history, HistoryState::Restoring);
    assert_eq!(snapshot.network.visibility, Visibility::Unavailable);
    assert!(snapshot.network.peers.is_empty());
    assert!(snapshot.network.detail.contains("busy"));
    assert!(!snapshot.network.detail.contains("Last observation"));
    assert_eq!(snapshot.services.accounts.observed_at_ms, 0);
    assert_eq!(snapshot.services.secrets.observed_at_ms, 0);
    assert!(!snapshot.services.secrets.detail.contains("Last observed"));
}

#[test]
fn busy_reads_keep_known_history_and_warnings_without_current_claims() {
    use aterm_types::network_state::{
        PresenceSample, VaultNetworkMaterialization, VaultNetworkRefresh,
        VaultNetworkRefreshOutcome, VaultNetworkSource,
    };

    let mut known = NetworkStateSnapshot {
        revision: 9,
        ..Default::default()
    };
    known.network.revision = 4;
    known.network.observed_at_ms = 100_000;
    known.network.visibility = Visibility::Active;
    known.network.history = HistoryState::Ready;
    known.network.detail = "authenticated observation".into();
    let mut machine = peer(7, Reachability::Reachable);
    machine.last_seen_ms = 100_000;
    machine.last_checked_ms = Some(100_000);
    machine.samples.push(PresenceSample {
        at_ms: 100_000,
        state: Reachability::Reachable,
    });
    known.network.peers.push(machine);
    let service = &mut known.services.secrets;
    service.availability = ServiceAvailability::Cached;
    service.observed_at_ms = 100_000;
    service.last_success_ms = Some(99_000);
    service.revision = Some("signed sequence 7".into());
    service.network_materialization = Some(VaultNetworkMaterialization {
        source: VaultNetworkSource::PeerUrl,
        sequence: 7,
        sampled_at_ms: 99_000,
    });
    service.network_refresh = Some(VaultNetworkRefresh {
        completed_at_ms: 99_500,
        outcome: VaultNetworkRefreshOutcome::Failed,
    });
    service.refresh_failed = true;
    service.detail = "Cache usable; latest refresh failed".into();
    let known = std::sync::Arc::new(known);
    let mut busy = false;
    let retained = snapshot_after_read(None, &known, &mut busy, Some(100_001));
    assert!(busy);
    assert_eq!((retained.revision, retained.network.revision), (9, 4));
    assert_eq!(retained.network.visibility, Visibility::Unavailable);
    assert_eq!(retained.network.history, HistoryState::Ready);
    assert_eq!(retained.network.observed_at_ms, 100_000);
    let machine = &retained.network.peers[0];
    assert_eq!(machine.reachability, Reachability::Unknown);
    assert_eq!(machine.identity, known.network.peers[0].identity);
    assert_eq!(machine.samples, known.network.peers[0].samples);
    assert_eq!(
        (
            machine.first_seen_ms,
            machine.last_seen_ms,
            machine.last_checked_ms
        ),
        (1, 100_000, Some(100_000))
    );
    let service = &retained.services.secrets;
    let prior = &known.services.secrets;
    assert_eq!(service.availability, ServiceAvailability::Unknown);
    assert_eq!(service.observed_at_ms, prior.observed_at_ms);
    assert_eq!(service.last_success_ms, prior.last_success_ms);
    assert_eq!(service.revision, prior.revision);
    assert_eq!(
        service.network_materialization,
        prior.network_materialization
    );
    assert_eq!(service.network_refresh, prior.network_refresh);
    assert!(service.refresh_failed);
    assert!(service.detail.contains(&prior.detail));
    assert!(service.detail.contains("busy"));
    assert!(retained.network.detail.contains("busy"));
    for wall in [Some(100_002), Some(99_999), None] {
        let again = snapshot_after_read(None, &retained, &mut busy, wall);
        assert!(std::sync::Arc::ptr_eq(&retained, &again));
    }
    let recovered = snapshot_after_read(Some(known.clone()), &retained, &mut busy, Some(100_002));
    assert!(!busy);
    assert!(std::sync::Arc::ptr_eq(&known, &recovered));
    assert_eq!(
        recovered.network.peers[0].reachability,
        Reachability::Reachable
    );
    assert_eq!(
        recovered.services.secrets.availability,
        ServiceAvailability::Cached
    );
}

#[test]
fn the_app_keeps_a_known_peer_when_its_network_reader_is_busy() {
    let mut app = crate::App::headless_for_test();
    let mut known = NetworkStateSnapshot {
        revision: 9,
        ..Default::default()
    };
    known.network.history = HistoryState::Ready;
    known.network.peers.push(peer(7, Reachability::Reachable));
    known.services.accounts.refresh_failed = true;
    app.network_snapshot = std::sync::Arc::new(known);
    app.network_source = Some(std::sync::Arc::new(|_| None));
    let now = std::time::Instant::now();
    app.service_network_state(now);
    assert!(app.network_source_busy);
    assert_eq!(app.network_snapshot.revision, 9);
    assert_eq!(app.network_snapshot.network.history, HistoryState::Ready);
    assert_eq!(app.network_snapshot.network.peers[0].slot, 7);
    assert_eq!(
        app.network_snapshot.network.peers[0].reachability,
        Reachability::Unknown
    );
    assert!(app.network_snapshot.services.accounts.refresh_failed);
    let retained = app.network_snapshot.clone();
    app.service_network_state(now + std::time::Duration::from_secs(1));
    assert!(std::sync::Arc::ptr_eq(&retained, &app.network_snapshot));
    assert_eq!(
        app.network_next_poll,
        Some(now + std::time::Duration::from_secs(2))
    );
}

fn cell_prims(prims: &[DrawPrim], hits: &[Hit], target: Target) -> Vec<DrawPrim> {
    let rect = hits
        .iter()
        .find(|hit| hit.kind == HitKind::Network(target))
        .expect("cell hit")
        .rect;
    prims
        .iter()
        .filter(|prim| match prim {
            DrawPrim::Panel { x, y, w, h, .. } | DrawPrim::Stroke { x, y, w, h, .. } => {
                *x >= rect.0 && *y >= rect.1 && x + w <= rect.0 + rect.2 && y + h <= rect.1 + rect.3
            }
            _ => false,
        })
        .cloned()
        .collect()
}

fn local_warning(prims: &[DrawPrim], hits: &[Hit], scale: f32, color: Rgb) -> bool {
    cell_prims(prims, hits, Target::Local).iter().any(|prim| {
        matches!(prim, DrawPrim::Panel { w, h, fill, .. }
            if *w == 2.0 * scale && *h == 2.0 * scale && *fill == rgba(color, 255))
    })
}

fn compact_caption(prims: &[DrawPrim], top: f32, scale: f32) -> Option<&str> {
    prims.iter().find_map(|prim| match prim {
        DrawPrim::Text { x, baseline, s, .. }
            if *x > 100.0 * scale && *baseline == (top + 10.0 * scale).round() =>
        {
            Some(s.as_str())
        }
        _ => None,
    })
}

#[test]
fn away_place_keeps_observer_and_history_failures_visible() {
    let mut active = NetworkStateSnapshot::default();
    active.network.visibility = Visibility::Active;
    active.network.history = HistoryState::Ready;
    active.network.observed_at_ms = 10_000;
    active.services.accounts.availability = ServiceAvailability::Cached;
    active.services.accounts.observed_at_ms = 10_000;
    active.services.secrets = active.services.accounts.clone();
    let active = std::sync::Arc::new(active);
    let mut busy = false;
    let mut cases = vec![
        fresh_snapshot(active.clone(), Some(25_001)),
        snapshot_after_read(None, &active, &mut busy, Some(10_000)),
    ];
    for (visibility, history) in [
        (Visibility::Waiting, HistoryState::Ready),
        (Visibility::Disabled, HistoryState::Ready),
        (Visibility::Active, HistoryState::Restoring),
        (Visibility::Active, HistoryState::Unavailable),
        (Visibility::Offline, HistoryState::Restoring),
        (Visibility::Offline, HistoryState::Unavailable),
    ] {
        let mut snapshot = active.as_ref().clone();
        snapshot.network.visibility = visibility;
        snapshot.network.history = history;
        cases.push(std::sync::Arc::new(snapshot));
    }
    let mut offline = aterm_netplace::sample::hotspot(10_000, 10_000);
    offline.at = aterm_netplace::Where::Offline;
    for place in [aterm_netplace::sample::hotspot(10_000, 10_000), offline] {
        let place = place_caption(Some(&place)).expect("away caption");
        if place.offline {
            let mut confirmed = active.as_ref().clone();
            confirmed.network.visibility = Visibility::Offline;
            assert_eq!(summary_with_caption(&confirmed, Some(&place)), place.long);
        }
        for snapshot in &cases {
            let status = summary_with_caption(snapshot, None);
            let composed = summary_with_caption(snapshot, Some(&place));
            assert!(composed.starts_with(&status), "{status}: {composed}");
            for scale in [1.0, 2.0] {
                for width in [350.0, 700.0, 1100.0] {
                    let g = DeckGeom {
                        w: width * scale,
                        h: 200.0 * scale,
                        pad: 0.0,
                        scale,
                        font_px: 14.0 * scale,
                    };
                    let inks = crate::stats_deck::cached_inks(
                        [20, 20, 20],
                        [230, 230, 230],
                        [100, 180, 200],
                    );
                    let draw = |caption| {
                        paint_with_inks(
                            &g,
                            170.0 * scale,
                            snapshot,
                            &crate::lab_deck::LabDeck::default(),
                            None,
                            caption,
                            &inks,
                            false,
                        )
                    };
                    let original = draw(None);
                    let composed = draw(Some(&place));
                    assert_eq!(original.1, composed.1);
                    for target in [Target::Local, Target::Accounts, Target::Secrets] {
                        assert_eq!(
                            crate::stats_deck::prims_fp(&cell_prims(
                                &original.0,
                                &original.1,
                                target
                            )),
                            crate::stats_deck::prims_fp(&cell_prims(
                                &composed.0,
                                &composed.1,
                                target
                            )),
                        );
                    }
                    let status =
                        compact_caption(&original.0, 170.0 * scale, scale).expect("status fits");
                    let caption = compact_caption(&composed.0, 170.0 * scale, scale)
                        .expect("composed status fits");
                    assert!(caption.starts_with(status), "{status}: {caption}");
                }
            }
        }
    }
}

#[test]
fn waiting_network_keeps_remembered_count_visible_without_claiming_reachability() {
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.visibility = Visibility::Waiting;
    snapshot.network.history = HistoryState::Ready;
    snapshot
        .network
        .peers
        .push(peer(2, Reachability::Reachable));
    let g = DeckGeom {
        w: 700.0,
        h: 200.0,
        pad: 0.0,
        scale: 1.0,
        font_px: 14.0,
    };
    let inks = crate::stats_deck::cached_inks([20, 20, 20], [230, 230, 230], [100, 180, 200]);
    let (prims, _) = paint_with_inks(
        &g,
        170.0,
        &snapshot,
        &crate::lab_deck::LabDeck::default(),
        None,
        None,
        &inks,
        false,
    );
    assert!(prims.iter().any(|prim| matches!(prim,
        DrawPrim::Text { s, .. } if s == "Waiting · 1 known"
    )));
    let summary = summary(&snapshot);
    assert!(summary.contains("1 known"));
    assert!(!summary.contains("reachable"));
}

#[test]
fn reachable_peer_expires_before_a_still_fresh_observer() {
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.visibility = Visibility::Active;
    snapshot.network.history = HistoryState::Ready;
    snapshot.network.observed_at_ms = 10_000;
    let mut remote = peer(2, Reachability::Reachable);
    remote.last_seen_ms = 2_000;
    remote.last_checked_ms = Some(2_000);
    snapshot.network.peers.push(remote);
    let snapshot = std::sync::Arc::new(snapshot);
    let expired = fresh_snapshot(snapshot.clone(), Some(17_001));
    assert_eq!(expired.network.visibility, Visibility::Active);
    assert_eq!(
        expired.network.peers[0].reachability,
        Reachability::LastSeen
    );
    assert_eq!(
        expired.network.observed_at_ms,
        snapshot.network.observed_at_ms
    );
    assert_eq!(expired.network.peers[0].last_seen_ms, 2_000);
    assert_eq!(expired.network.peers[0].last_checked_ms, Some(2_000));
    assert_eq!(
        expired.network.peers[0].samples,
        snapshot.network.peers[0].samples
    );
    assert_eq!(expired.network.revision, snapshot.network.revision);
    assert_eq!(expired.revision, snapshot.revision);
}

#[test]
fn peer_contact_and_check_clocks_each_bound_current_reachability() {
    for (seen, checked, expected) in [
        (2_001, Some(2_001), Reachability::Reachable),
        (2_000, Some(2_001), Reachability::LastSeen),
        (2_001, Some(2_000), Reachability::LastSeen),
        (0, Some(2_001), Reachability::Unknown),
        (2_001, None, Reachability::Unknown),
        (2_001, Some(0), Reachability::Unknown),
        (17_002, Some(2_001), Reachability::Unknown),
        (2_001, Some(17_002), Reachability::Unknown),
    ] {
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.visibility = Visibility::Active;
        snapshot.network.observed_at_ms = 10_000;
        let mut remote = peer(2, Reachability::Reachable);
        remote.last_seen_ms = seen;
        remote.last_checked_ms = checked;
        snapshot.network.peers.push(remote.clone());
        let projected = fresh_snapshot(std::sync::Arc::new(snapshot), Some(17_001));
        remote.reachability = expected;
        assert_eq!(projected.network.peers, vec![remote]);
    }
}

/// A peer its observer reaches every 24 s is Reachable for 48 s after a contact. The deck ages
/// it by that lease, as the observer does, and hands back the same snapshot while it holds.
#[test]
fn a_measured_lease_keeps_a_slow_peer_reachable_in_the_deck() {
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.visibility = Visibility::Active;
    snapshot.network.observed_at_ms = 120_000;
    let mut remote = peer(2, Reachability::Reachable);
    remote.last_seen_ms = 100_000;
    remote.last_checked_ms = Some(100_000);
    remote.lease_ms = Some(48_000);
    snapshot.network.peers.push(remote.clone());
    let snapshot = std::sync::Arc::new(snapshot);
    // Twenty seconds after the contact, past the 15 s of a peer with no measured lease.
    let held = fresh_snapshot(snapshot.clone(), Some(120_000));
    assert!(std::sync::Arc::ptr_eq(&held, &snapshot));
    assert_eq!(held.network.peers[0].reachability, Reachability::Reachable);
    // The source ages (its observer publishes every 2 s); at the lease's last millisecond
    // the peer still holds, and one past it reads LastSeen with no lease left to read by.
    let aged = |observed_at_ms: u64| {
        let mut later = snapshot.as_ref().clone();
        later.network.observed_at_ms = observed_at_ms;
        fresh_snapshot(std::sync::Arc::new(later), Some(observed_at_ms))
    };
    assert_eq!(
        aged(148_000).network.peers[0].reachability,
        Reachability::Reachable
    );
    let expired = aged(148_001);
    remote.reachability = Reachability::LastSeen;
    remote.lease_ms = None;
    assert_eq!(expired.network.peers, vec![remote.clone()]);
    // With no lease the same contact is past the 15 s floor.
    let mut unleased = snapshot.as_ref().clone();
    unleased.network.peers[0].lease_ms = None;
    assert_eq!(
        fresh_snapshot(std::sync::Arc::new(unleased), Some(120_000))
            .network
            .peers,
        vec![remote]
    );
}

#[test]
fn observer_refresh_does_not_refresh_retained_peer_evidence() {
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.visibility = Visibility::Active;
    snapshot.network.observed_at_ms = 10_000;
    let mut remote = peer(2, Reachability::Reachable);
    remote.last_seen_ms = 2_000;
    remote.last_checked_ms = Some(2_000);
    snapshot.network.peers.push(remote);
    let first = fresh_snapshot(std::sync::Arc::new(snapshot.clone()), Some(17_001));
    assert_eq!(first.network.peers[0].reachability, Reachability::LastSeen);
    snapshot.network.observed_at_ms = 18_000;
    let refreshed = fresh_snapshot(std::sync::Arc::new(snapshot.clone()), Some(18_000));
    assert_eq!(
        refreshed.network.peers[0].reachability,
        Reachability::LastSeen
    );
    snapshot.network.peers[0].last_seen_ms = 18_000;
    snapshot.network.peers[0].last_checked_ms = Some(18_000);
    let contacted = fresh_snapshot(std::sync::Arc::new(snapshot), Some(18_000));
    assert_eq!(
        contacted.network.peers[0].reachability,
        Reachability::Reachable
    );
    let mut retained = first.as_ref().clone();
    retained.network.observed_at_ms = 18_000;
    retained.network.peers[0].last_seen_ms = 18_000;
    retained.network.peers[0].last_checked_ms = Some(18_000);
    assert_eq!(
        fresh_snapshot(std::sync::Arc::new(retained), Some(18_000))
            .network
            .peers[0]
            .reachability,
        Reachability::LastSeen
    );
}

#[test]
fn peer_expiry_invalidates_an_open_network_lens_without_a_source_revision() {
    let mut app = crate::App::headless_for_test();
    let wid = *app.windows.keys().next().expect("window");
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.visibility = Visibility::Active;
    snapshot.network.observed_at_ms = 10_000;
    let mut remote = peer(2, Reachability::Reachable);
    remote.last_seen_ms = 2_000;
    remote.last_checked_ms = Some(2_000);
    snapshot.network.peers.push(remote);
    app.network_snapshot = std::sync::Arc::new(snapshot);
    assert!(app.open_network_lens());
    let before = app.stats_lens_fp(wid);
    assert_ne!(before, 0);
    app.network_snapshot = fresh_snapshot(app.network_snapshot.clone(), Some(17_001));
    assert_eq!(app.network_snapshot.revision, 0);
    assert_eq!(app.network_snapshot.network.revision, 0);
    assert_eq!(app.network_snapshot.network.visibility, Visibility::Active);
    assert_ne!(app.stats_lens_fp(wid), before);
}

#[test]
fn observation_time_changes_the_lens_fingerprint() {
    let mut app = crate::App::headless_for_test();
    let wid = *app.windows.keys().next().expect("window");
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.visibility = Visibility::Waiting;
    snapshot.network.observed_at_ms = 1_000;
    app.network_snapshot = std::sync::Arc::new(snapshot);
    assert!(app.open_network_lens());
    let before = app.stats_lens_fp(wid);
    let mut updated = app.network_snapshot.as_ref().clone();
    updated.network.observed_at_ms = 2_000;
    app.network_snapshot = std::sync::Arc::new(updated);
    assert_eq!(app.network_snapshot.revision, 0);
    assert_ne!(app.stats_lens_fp(wid), before);
}

#[test]
fn retention_limit_changes_visible_coverage_without_moving_cells() {
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.visibility = Visibility::Active;
    snapshot.network.history = HistoryState::Ready;
    snapshot.network.observer = Some(peer(1, Reachability::Reachable).identity);
    snapshot
        .network
        .peers
        .push(peer(2, Reachability::Reachable));
    let g = DeckGeom {
        w: 700.0,
        h: 200.0,
        pad: 0.0,
        scale: 1.0,
        font_px: 14.0,
    };
    let inks = crate::stats_deck::cached_inks([20, 20, 20], [230, 230, 230], [100, 180, 200]);
    let before = paint_with_inks(
        &g,
        170.0,
        &snapshot,
        &crate::lab_deck::LabDeck::default(),
        None,
        None,
        &inks,
        false,
    );
    let before_summary = summary(&snapshot);
    let before_details = details(&snapshot, Target::Overview);
    snapshot.network.retention_limited = true;
    let limited = paint_with_inks(
        &g,
        170.0,
        &snapshot,
        &crate::lab_deck::LabDeck::default(),
        None,
        None,
        &inks,
        false,
    );
    assert_ne!(summary(&snapshot), before_summary);
    assert_ne!(
        crate::stats_deck::prims_fp(&limited.0),
        crate::stats_deck::prims_fp(&before.0)
    );
    assert_eq!(limited.1, before.1);
    assert!(details(&snapshot, Target::Overview).len() > before_details.len());
    assert_eq!(snapshot.network.peers.len(), 1);
    assert_eq!(
        snapshot.network.peers[0].reachability,
        Reachability::Reachable
    );
}

#[test]
fn retention_limit_invalidates_network_details_without_a_revision_change() {
    let mut app = crate::App::headless_for_test();
    let wid = *app.windows.keys().next().expect("window");
    assert!(app.open_network_lens());
    let before = app.stats_lens_fp(wid);
    let mut snapshot = app.network_snapshot.as_ref().clone();
    snapshot.network.retention_limited = true;
    app.network_snapshot = std::sync::Arc::new(snapshot);
    assert_ne!(app.stats_lens_fp(wid), before);
}

#[test]
fn peer_slots_survive_status_and_input_order_changes() {
    let mut snapshot = NetworkStateSnapshot {
        network: NetworkObservation {
            visibility: Visibility::Active,
            history: HistoryState::Ready,
            peers: vec![
                peer(7, Reachability::Reachable),
                peer(2, Reachability::LastSeen),
            ],
            ..Default::default()
        },
        ..Default::default()
    };
    let g = DeckGeom {
        w: 700.0,
        h: 200.0,
        pad: 0.0,
        scale: 1.0,
        font_px: 14.0,
    };
    let inks = crate::stats_deck::cached_inks([20, 20, 20], [230, 230, 230], [100, 180, 200]);
    let first = paint_with_inks(
        &g,
        170.0,
        &snapshot,
        &crate::lab_deck::LabDeck::default(),
        None,
        None,
        &inks,
        false,
    )
    .1;
    snapshot.network.peers.reverse();
    snapshot.network.peers[0].reachability = Reachability::Unknown;
    let second = paint_with_inks(
        &g,
        170.0,
        &snapshot,
        &crate::lab_deck::LabDeck::default(),
        None,
        None,
        &inks,
        false,
    )
    .1;
    assert_eq!(first, second);
    snapshot.network.via_local_process = Some(42);
    let (relayed, relayed_hits) = paint_with_inks(
        &g,
        170.0,
        &snapshot,
        &crate::lab_deck::LabDeck::default(),
        None,
        None,
        &inks,
        false,
    );
    assert_eq!(second, relayed_hits);
    assert!(relayed.iter().any(|prim| {
        matches!(prim, DrawPrim::Text { s, face, .. }
            if s.contains("via local aterm") && *face != crate::widget::TextFace::Mono)
    }));
    assert_eq!(
        step(
            &snapshot,
            &crate::lab_deck::LabDeck::default(),
            None,
            (700.0, 1.0),
            Target::Local,
            true
        ),
        Target::Seat(SeatKey::Observed(2))
    );
}

#[test]
fn captions_use_resource_faces_and_service_cells_clear_their_labels() {
    for scale in [1.0, 2.0] {
        let g = DeckGeom {
            w: 700.0 * scale,
            h: 200.0 * scale,
            pad: 0.0,
            scale,
            font_px: 14.0 * scale,
        };
        let inks = crate::stats_deck::cached_inks([20, 20, 20], [230, 230, 230], [100, 180, 200]);
        let (prims, hits) = paint_with_inks(
            &g,
            170.0 * scale,
            &inactive_snapshot(),
            &crate::lab_deck::LabDeck::default(),
            None,
            None,
            &inks,
            false,
        );
        for prim in &prims {
            if let DrawPrim::Text { face, px, .. } = prim {
                assert_ne!(*face, crate::widget::TextFace::Mono);
                assert_eq!(*px, 9.5 * scale);
            }
        }
        for (label, target) in [("Accounts", Target::Accounts), ("Secrets", Target::Secrets)] {
            let end = prims
                .iter()
                .find_map(|prim| match prim {
                    DrawPrim::Text { x, s, px, face, .. } if s == label => {
                        Some(x + crate::tray_raster::ui_text_width_for(*face, s, *px).ceil())
                    }
                    _ => None,
                })
                .expect("service label");
            let hit = hits
                .iter()
                .find(|hit| hit.kind == HitKind::Network(target))
                .expect("service cell");
            assert!(hit.rect.0 >= end + 4.0 * scale);
        }
    }
}
#[test]
fn retained_contact_and_service_times_do_not_depend_on_the_observer_clock() {
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.visibility = Visibility::Unavailable;
    snapshot.network.peers.push(peer(2, Reachability::LastSeen));
    snapshot.services.secrets.observed_at_ms = 3000;
    snapshot.services.secrets.last_success_ms = Some(2000);
    let contact = details(&snapshot, Target::Seat(SeatKey::Observed(2)));
    let service = details(&snapshot, Target::Secrets);
    assert!(
        contact
            .iter()
            .any(|(_, value)| value.contains("1970-01-01T00:00:01Z"))
    );
    assert!(
        service
            .iter()
            .any(|(_, value)| value == "1970-01-01T00:00:03Z")
    );
    assert!(
        service
            .iter()
            .any(|(_, value)| value == "1970-01-01T00:00:02Z")
    );
    for observer_time in [1000, 10_000, 1_000_000] {
        snapshot.network.observed_at_ms = observer_time;
        assert_eq!(
            details(&snapshot, Target::Seat(SeatKey::Observed(2))),
            contact
        );
        assert_eq!(details(&snapshot, Target::Secrets), service);
    }
    assert_eq!(observation_time(0), "Unknown");
    assert_eq!(observation_time(u64::MAX), "Invalid timestamp");
    assert_eq!(
        observation_time(253_402_300_799_000),
        "9999-12-31T23:59:59Z"
    );
}

#[test]
fn selected_peer_keeps_verified_identity_and_original_outbound_evidence() {
    use aterm_types::network_state::AuthenticatedEndpoint;

    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.history = HistoryState::Ready;
    let mut remote = peer(2, Reachability::LastSeen);
    remote.identity.label = None;
    remote.identity.machine_id = [0x23; 16];
    remote.identity.incarnation = [0x42; 16];
    remote.last_authenticated_endpoint = Some(AuthenticatedEndpoint {
        address: "[fd00::12]:7414".parse().unwrap(),
        observed_at_ms: 2000,
    });
    snapshot.network.peers.push(remote);

    for (visibility, observer_time) in [
        (Visibility::Active, 3000),
        (Visibility::Offline, 1_000_000),
        (Visibility::Unavailable, 0),
    ] {
        snapshot.network.visibility = visibility;
        snapshot.network.observed_at_ms = observer_time;
        let rows = details(&snapshot, Target::Seat(SeatKey::Observed(2)));
        assert_eq!(rows[0].0, "Machine");
        let at = |key| rows.iter().position(|(name, _)| name == key).unwrap();
        assert!(at("Observation") < at("Last repair"));
        assert!(at("Last repair") < at("Last outbound"));
        assert!(at("Last outbound") < at("Machine UUID"));
        let value = |key| {
            rows.iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
                .unwrap()
        };
        assert_eq!(value("Machine UUID"), "23232323232323232323232323232323");
        assert_eq!(value("Incarnation"), "42424242424242424242424242424242");
        assert!(value("Last outbound").contains("[fd00::12]:7414"));
        assert!(value("Last outbound").contains("1970-01-01T00:00:02Z"));
        assert!(value("Last outbound").contains("historical"));
        assert!(value("Observation").contains("1970-01-01T00:00:01Z"));
    }
}

#[test]
fn selected_peer_does_not_infer_an_endpoint_for_a_replaced_incarnation() {
    use aterm_types::network_state::AuthenticatedEndpoint;

    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.visibility = Visibility::Active;
    snapshot
        .network
        .peers
        .push(peer(2, Reachability::Reachable));
    let absent = details(&snapshot, Target::Seat(SeatKey::Observed(2)))
        .into_iter()
        .find(|(name, _)| name == "Last outbound")
        .unwrap();
    snapshot.network.peers[0].last_authenticated_endpoint = Some(AuthenticatedEndpoint {
        address: "192.0.2.12:7414".parse().unwrap(),
        observed_at_ms: 2000,
    });
    assert!(
        details(&snapshot, Target::Seat(SeatKey::Observed(2)))
            .iter()
            .any(|(_, value)| value.contains("192.0.2.12:7414"))
    );

    // The observer clears endpoint evidence when a new verified incarnation replaces it.
    snapshot.network.peers[0].identity.incarnation = [0x3a; 16];
    snapshot.network.peers[0].last_authenticated_endpoint = None;
    let replaced = details(&snapshot, Target::Seat(SeatKey::Observed(2)));
    assert!(replaced.contains(&absent));
    assert!(
        replaced
            .iter()
            .any(|(name, value)| { name == "Incarnation" && value == &"3a".repeat(16) })
    );
    assert!(
        !replaced
            .iter()
            .any(|(_, value)| value.contains("192.0.2.12:7414"))
    );
}

#[test]
fn failed_refresh_preserves_available_cache_and_does_not_claim_peers() {
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.history = HistoryState::Ready;
    snapshot.network.unverified_endpoints = 3;
    snapshot.services.secrets.availability = ServiceAvailability::Cached;
    snapshot.services.secrets.refresh_failed = true;
    let failed_details = details(&snapshot, Target::Secrets);
    snapshot.services.secrets.refresh_failed = false;
    assert_ne!(failed_details, details(&snapshot, Target::Secrets));
    assert_eq!(
        snapshot.services.secrets.availability,
        ServiceAvailability::Cached
    );
    assert!(
        !targets_with(
            &snapshot,
            &crate::lab_deck::LabDeck::default(),
            None,
            (700.0, 1.0)
        )
        .iter()
        .any(|t| matches!(t, Target::Seat(_)))
    );
}
#[test]
fn history_failure_is_visible_on_active_and_relayed_compact_strips() {
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.visibility = Visibility::Active;
    snapshot.network.history = HistoryState::Ready;
    snapshot.network.observer = Some(peer(1, Reachability::Reachable).identity);
    // A failed journal retains known peers but the observer cannot accept contacts.
    snapshot.network.peers.push(peer(2, Reachability::Unknown));
    snapshot.network.unverified_endpoints = 3;
    snapshot.services.accounts.availability = ServiceAvailability::Cached;
    snapshot.services.secrets.availability = ServiceAvailability::Cached;
    let inks = crate::stats_deck::cached_inks([20, 20, 20], [230, 230, 230], [100, 180, 200]);
    for scale in [1.0, 2.0] {
        let painter = Painter::new(&inks, scale, 14.0 * scale, false);
        let full_width = painter.ui_text_w("History unavailable", false);
        let short_width = painter.ui_text_w("History error", false);
        // The words start 12 pt after the one retained peer's column and end 12 pt short
        // of the edge.
        let caption_x = (field::SEATS_X_PT + field::PITCH_PT + field::GAP_PT) * scale;
        let constrained_room = short_width.max(70.0 * scale + 1.0);
        assert!(constrained_room < full_width);
        let constrained_width = caption_x + constrained_room + field::GAP_PT * scale;
        for (width, expect_short) in [
            (constrained_width, true),
            (360.0 * scale, false),
            (700.0 * scale, false),
        ] {
            let g = DeckGeom {
                w: width,
                h: 200.0 * scale,
                pad: 0.0,
                scale,
                font_px: 14.0 * scale,
            };
            let top = 170.0 * scale;
            for relay in [None, Some(42)] {
                snapshot.network.via_local_process = relay;
                snapshot.network.history = HistoryState::Ready;
                let ready = paint_with_inks(
                    &g,
                    top,
                    &snapshot,
                    &crate::lab_deck::LabDeck::default(),
                    None,
                    None,
                    &inks,
                    false,
                );
                let ready_summary = summary(&snapshot);
                assert!(!local_warning(&ready.0, &ready.1, scale, inks.over));
                snapshot.network.history = HistoryState::Unavailable;
                let failed = paint_with_inks(
                    &g,
                    top,
                    &snapshot,
                    &crate::lab_deck::LabDeck::default(),
                    None,
                    None,
                    &inks,
                    false,
                );
                assert!(local_warning(&failed.0, &failed.1, scale, inks.over));
                assert_eq!(failed.1, ready.1);
                assert_ne!(summary(&snapshot), ready_summary);
                for target in [
                    Target::Seat(SeatKey::Observed(2)),
                    Target::Accounts,
                    Target::Secrets,
                ] {
                    assert_eq!(
                        crate::stats_deck::prims_fp(&cell_prims(&failed.0, &failed.1, target)),
                        crate::stats_deck::prims_fp(&cell_prims(&ready.0, &ready.1, target)),
                    );
                }
                // No draw-or-drop rule: the status is always drawn, shortened to fit.
                let caption = compact_caption(&failed.0, top, scale).expect("history caption");
                assert!(caption.contains("History"));
                assert!(!caption.contains("reachable") && !caption.contains("via"));
                if expect_short {
                    assert!(painter.ui_text_w(caption, false) < full_width);
                    assert!(painter.ui_text_w(caption, false) <= constrained_room);
                }
                for prim in &failed.0 {
                    if let DrawPrim::Text {
                        x,
                        baseline,
                        s,
                        px,
                        face,
                        ..
                    } = prim
                        && *x > 100.0 * scale
                        && *baseline == (top + 10.0 * scale).round()
                    {
                        assert!(
                            x + crate::tray_raster::ui_text_width_for(*face, s, *px).ceil() <= g.w
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn history_health_and_partial_coverage_compose_without_changing_cells() {
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.observer = Some(peer(1, Reachability::Reachable).identity);
    snapshot.network.peers.push(peer(2, Reachability::Unknown));
    snapshot.network.via_local_process = Some(42);
    snapshot.services.accounts.availability = ServiceAvailability::Cached;
    snapshot.services.secrets.availability = ServiceAvailability::Cached;
    let g = DeckGeom {
        w: 700.0,
        h: 200.0,
        pad: 0.0,
        scale: 1.0,
        font_px: 14.0,
    };
    let inks = crate::stats_deck::cached_inks([20, 20, 20], [230, 230, 230], [100, 180, 200]);
    for visibility in [
        Visibility::Active,
        Visibility::Checking,
        Visibility::Offline,
        Visibility::Disabled,
        Visibility::Waiting,
        Visibility::Unavailable,
    ] {
        snapshot.network.visibility = visibility;
        snapshot.network.history = HistoryState::Ready;
        snapshot.network.retention_limited = false;
        let ready = paint_with_inks(
            &g,
            170.0,
            &snapshot,
            &crate::lab_deck::LabDeck::default(),
            None,
            None,
            &inks,
            false,
        );
        for history in [HistoryState::Restoring, HistoryState::Unavailable] {
            snapshot.network.history = history;
            for limited in [false, true] {
                snapshot.network.retention_limited = limited;
                let painted = paint_with_inks(
                    &g,
                    170.0,
                    &snapshot,
                    &crate::lab_deck::LabDeck::default(),
                    None,
                    None,
                    &inks,
                    false,
                );
                assert_eq!(painted.1, ready.1);
                assert_eq!(
                    local_warning(&painted.0, &painted.1, 1.0, inks.over),
                    limited || history == HistoryState::Unavailable,
                );
                let caption = compact_caption(&painted.0, 170.0, 1.0).expect("status caption");
                assert!(caption.contains("1 known"));
                assert_eq!(caption.contains("partial history"), limited);
                assert!(!caption.contains("reachable") && !caption.contains("via"));
                if visibility == Visibility::Unavailable {
                    assert_eq!(
                        compact_caption(&ready.0, 170.0, 1.0),
                        Some(caption.split(" · partial history").next().unwrap()),
                    );
                } else {
                    assert_ne!(Some(caption), compact_caption(&ready.0, 170.0, 1.0));
                    assert_eq!(
                        caption.contains("Restoring"),
                        history == HistoryState::Restoring
                    );
                }
                for target in [
                    Target::Seat(SeatKey::Observed(2)),
                    Target::Accounts,
                    Target::Secrets,
                ] {
                    assert_eq!(
                        crate::stats_deck::prims_fp(&cell_prims(&painted.0, &painted.1, target)),
                        crate::stats_deck::prims_fp(&cell_prims(&ready.0, &ready.1, target)),
                    );
                }
            }
        }
    }
}

#[test]
fn stale_reachable_peer_requires_an_active_observer() {
    for visibility in [
        Visibility::Checking,
        Visibility::Offline,
        Visibility::Disabled,
        Visibility::Waiting,
        Visibility::Unavailable,
    ] {
        assert_eq!(
            observed_reachability(visibility, Reachability::Reachable),
            Reachability::Unknown
        );
    }
    assert_eq!(
        observed_reachability(Visibility::Active, Reachability::Reachable),
        Reachability::Reachable
    );
}

#[test]
fn stalled_or_unusable_clock_expires_current_evidence_but_keeps_history() {
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.visibility = Visibility::Active;
    snapshot.network.observed_at_ms = 10_000;
    let mut remote = peer(2, Reachability::Reachable);
    remote.last_seen_ms = 10_000;
    remote.last_checked_ms = Some(10_000);
    snapshot.network.peers.push(remote);
    let snapshot = std::sync::Arc::new(snapshot);
    assert!(std::sync::Arc::ptr_eq(
        &fresh_snapshot(snapshot.clone(), Some(25_000)),
        &snapshot
    ));
    for now in [Some(25_001), Some(9_999), None] {
        let expired = fresh_snapshot(snapshot.clone(), now);
        assert_eq!(expired.network.visibility, Visibility::Unavailable);
        assert_eq!(expired.network.peers, snapshot.network.peers);
        assert_eq!(expired.network.observed_at_ms, 10_000);
    }
}

#[test]
fn direct_visibility_claims_expire_without_losing_history_or_repeated_expiry() {
    for visibility in [
        Visibility::Offline,
        Visibility::Disabled,
        Visibility::Waiting,
        Visibility::Checking,
    ] {
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.visibility = visibility;
        snapshot.network.observed_at_ms = 100_000;
        snapshot.network.history = HistoryState::Ready;
        snapshot.network.peers.push(peer(2, Reachability::LastSeen));
        let snapshot = std::sync::Arc::new(snapshot);
        assert!(snapshot.network.via_local_process.is_none());
        assert!(std::sync::Arc::ptr_eq(
            &fresh_snapshot(snapshot.clone(), Some(115_000)),
            &snapshot,
        ));
        for now in [Some(115_001), Some(99_999), None] {
            let expired = fresh_snapshot(snapshot.clone(), now);
            assert_eq!(expired.network.visibility, Visibility::Unavailable);
            assert_eq!(
                expired.network.observed_at_ms,
                snapshot.network.observed_at_ms
            );
            assert_eq!(expired.network.history, snapshot.network.history);
            assert_eq!(expired.network.peers, snapshot.network.peers);
            assert!(std::sync::Arc::ptr_eq(
                &fresh_snapshot(expired.clone(), now),
                &expired
            ));
        }
    }
}

#[test]
fn initial_checking_and_unavailable_history_do_not_expire_again() {
    for (visibility, observed_at_ms, source) in [
        (Visibility::Checking, 0, None),
        (Visibility::Unavailable, 100_000, None),
        (Visibility::Unavailable, 100_000, Some(41)),
    ] {
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.visibility = visibility;
        snapshot.network.observed_at_ms = observed_at_ms;
        snapshot.network.via_local_process = source;
        let snapshot = std::sync::Arc::new(snapshot);
        for now in [Some(115_001), None] {
            assert!(std::sync::Arc::ptr_eq(
                &fresh_snapshot(snapshot.clone(), now),
                &snapshot
            ));
        }
    }
}

#[test]
fn service_freshness_is_independent_of_network_and_other_services() {
    for visibility in [Visibility::Active, Visibility::Offline, Visibility::Waiting] {
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.visibility = visibility;
        snapshot.network.observed_at_ms = 100_000;
        snapshot.services.accounts = ServiceState {
            availability: ServiceAvailability::Cached,
            observed_at_ms: 40_000,
            last_success_ms: Some(35_000),
            revision: Some("retained revision".into()),
            refresh_failed: true,
            ..Default::default()
        };
        snapshot.services.secrets = ServiceState {
            availability: ServiceAvailability::Cached,
            observed_at_ms: 100_000,
            ..Default::default()
        };
        let snapshot = std::sync::Arc::new(snapshot);
        assert!(std::sync::Arc::ptr_eq(
            &fresh_snapshot(snapshot.clone(), Some(100_000)),
            &snapshot,
        ));
        let expired = fresh_snapshot(snapshot.clone(), Some(100_001));
        assert_eq!(expired.network, snapshot.network);
        assert_eq!(expired.services.secrets, snapshot.services.secrets);
        assert_eq!(
            expired.services.accounts.availability,
            ServiceAvailability::Unknown
        );
        assert_eq!(expired.services.accounts.observed_at_ms, 40_000);
        assert_eq!(expired.services.accounts.last_success_ms, Some(35_000));
        assert_eq!(
            expired.services.accounts.revision,
            snapshot.services.accounts.revision
        );
        assert!(expired.services.accounts.refresh_failed);
        assert_eq!(
            snapshot.services.accounts.availability,
            ServiceAvailability::Cached
        );
        assert!(std::sync::Arc::ptr_eq(
            &fresh_snapshot(expired.clone(), Some(100_001)),
            &expired,
        ));
    }
}

/// The shipped wake with no network source: the lab's account rows alone own the band's
/// poll, only while the band or its lens is drawn, and the feed is asked only then.
/// Negative controls: asking the feed unconditionally takes the held rows while hidden (the
/// first `lab_deck` assertion); keeping the old `network_source?` gate leaves the drawn
/// deadline `None`; polling every second after the first projection breaks the `EVERY` step.
#[test]
fn with_no_network_source_the_lab_rows_wake_the_band_only_while_drawn() {
    let mut app = crate::App::headless_for_test();
    let wid = *app.windows.keys().next().expect("window");
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("off".into());
    app.network_source = None;
    let rows = crate::lab_deck::LabDeck {
        more: 3,
        ..Default::default()
    };
    let now = std::time::Instant::now();
    assert_eq!(
        app.network_state_deadline(),
        None,
        "test feeds are not live"
    );
    app.lab_deck_feed.live = true;
    assert_eq!(app.network_state_deadline(), None, "nothing draws the band");
    app.service_network_state(now);
    assert_eq!(app.network_next_poll, None);
    app.lab_deck_feed.hold(rows.clone());
    app.service_network_state(now);
    assert!(
        app.lab_deck.is_empty() && app.lab_deck.more == 0,
        "a hidden band asks nothing"
    );
    assert!(app.open_network_lens());
    assert!(app.network_state_deadline().is_some());
    app.service_network_state(now);
    assert_eq!(app.lab_deck.as_ref(), &rows);
    assert_eq!(
        app.network_next_poll,
        Some(now + crate::lab_deck::Feed::EVERY),
        "a projection is held: once a minute"
    );
    assert_eq!(
        app.network_state_deadline(),
        Some(now + crate::lab_deck::Feed::EVERY)
    );
    app.close_stats_lens(wid);
    assert_eq!(app.network_state_deadline(), None);
    app.service_network_state(now + crate::lab_deck::Feed::EVERY);
    assert_eq!(app.network_next_poll, None, "hidden again: no poll owed");
    app.lab_deck_feed.live = false;
    assert!(app.open_network_lens());
    assert_eq!(app.network_state_deadline(), None);
}

#[test]
fn visible_network_lens_keeps_its_poll_deadline_with_resource_bars_hidden() {
    let mut app = crate::App::headless_for_test();
    let wid = *app.windows.keys().next().expect("window");
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("off".into());
    let snapshot = inactive_snapshot();
    app.network_source = Some(std::sync::Arc::new(move |_| Some(snapshot.clone())));
    let due = std::time::Instant::now() + std::time::Duration::from_secs(1);
    app.network_next_poll = Some(due);
    assert_eq!(app.network_state_deadline(), None);
    assert!(app.open_network_lens());
    assert_eq!(app.network_state_deadline(), Some(due));
    app.windows.get_mut(&wid).unwrap().occluded = true;
    assert_eq!(app.network_state_deadline(), None);
    app.windows.get_mut(&wid).unwrap().occluded = false;
    assert_eq!(app.network_state_deadline(), Some(due));
    app.close_stats_lens(wid);
    assert_eq!(app.network_state_deadline(), None);
    assert!(app.open_network_lens());
    app.network_source = None;
    assert_eq!(app.network_state_deadline(), None);
}

/// The window asks for the services only while their cells are drawn, on the band's lens or
/// the Lab page, or for a minute after an inspection of the projection; with none of these,
/// or the lens occluded, it reads the network alone.
/// NEGATIVE CONTROLS: a read that always asks for the services records `WithServices` with
/// nothing drawn; one that leaves out the Lab page, or the inspection, records `Network`
/// there; an inspection that never lapses records `WithServices` a minute on.
#[test]
fn the_window_reads_the_services_only_while_their_cells_are_drawn_or_inspected() {
    use crate::NetworkRead::{Network, WithServices};
    let mut app = crate::App::headless_for_test();
    let wid = *app.windows.keys().next().expect("window");
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("off".into());
    let snapshot = inactive_snapshot();
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let reads = std::sync::Arc::clone(&asked);
    app.network_source = Some(std::sync::Arc::new(move |read| {
        reads.lock().unwrap().push(read);
        Some(snapshot.clone())
    }));
    let now = std::cell::Cell::new(std::time::Instant::now());
    let read = |app: &mut crate::App, after: std::time::Duration| {
        now.set(now.get() + after);
        app.service_network_state(now.get());
        std::mem::take(&mut *asked.lock().unwrap())
    };
    let second = std::time::Duration::from_secs(1);
    assert_eq!(
        read(&mut app, second),
        [Network],
        "nothing draws the services"
    );
    assert!(app.open_network_lens());
    assert_eq!(
        read(&mut app, second),
        [WithServices],
        "the lens draws them"
    );
    app.windows.get_mut(&wid).unwrap().occluded = true;
    assert_eq!(
        read(&mut app, second),
        [Network],
        "an occluded lens draws nothing"
    );
    app.windows.get_mut(&wid).unwrap().occluded = false;
    app.close_stats_lens(wid);
    assert_eq!(read(&mut app, second), [Network], "the lens closed");
    assert!(app.open_settings_tab_in_window(wid, crate::native_settings::SettingsRoute::Lab));
    assert_eq!(
        read(&mut app, second),
        [WithServices],
        "the Lab page draws them"
    );
    assert!(app.open_settings_tab_in_window(wid, crate::native_settings::SettingsRoute::Home));
    assert_eq!(read(&mut app, second), [Network], "the Lab page left");
    app.note_inspection(&crate::app_control::InspectRequest::Tabs, now.get());
    assert_eq!(
        read(&mut app, second),
        [Network],
        "the tabs' inspection reads none"
    );
    app.note_inspection(&crate::app_control::InspectRequest::Network, now.get());
    let at_once = std::time::Duration::ZERO;
    assert_eq!(
        read(&mut app, at_once),
        [WithServices],
        "an inspection reads them at once"
    );
    let minute = crate::app_control::INSPECTED_FOR;
    assert_eq!(read(&mut app, minute), [WithServices], "a minute on");
    assert_eq!(read(&mut app, second), [Network], "past a minute");
}

/// An inspection owes the window a read each second for a minute, drawn or not: the next
/// turn's at once, then the ordinary poll's; one within that minute leaves the poll alone.
/// NEGATIVE CONTROLS: a deadline that leaves out the inspection is `None` with nothing drawn;
/// an inspection that keeps the poll waits for it; one that clears it each time reads at each.
#[test]
fn an_inspection_owes_the_window_a_read_each_second_for_a_minute() {
    let mut app = crate::App::headless_for_test();
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("off".into());
    let snapshot = inactive_snapshot();
    app.network_source = Some(std::sync::Arc::new(move |_| Some(snapshot.clone())));
    let second = std::time::Duration::from_secs(1);
    let now = std::time::Instant::now();
    app.service_network_state(now);
    assert_eq!(app.network_next_poll, Some(now + second));
    assert_eq!(
        app.network_state_deadline(),
        None,
        "nothing shows the services"
    );
    app.note_inspection(&crate::app_control::InspectRequest::Network, now);
    assert_eq!(app.network_next_poll, None, "the next turn reads at once");
    assert!(app.network_state_deadline().is_some());
    app.service_network_state(now);
    assert_eq!(
        app.network_state_deadline(),
        Some(now + second),
        "then each second"
    );
    app.note_inspection(&crate::app_control::InspectRequest::Network, now);
    assert_eq!(
        app.network_next_poll,
        Some(now + second),
        "a repeat leaves the poll"
    );
    app.network_inspected =
        std::time::Instant::now().checked_sub(crate::app_control::INSPECTED_FOR + second);
    assert!(app.network_inspected.is_some());
    assert_eq!(app.network_state_deadline(), None, "past a minute");
}

#[test]
fn service_expiry_invalidates_an_open_network_lens() {
    let mut app = crate::App::headless_for_test();
    let wid = *app.windows.keys().next().expect("window");
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.visibility = Visibility::Offline;
    snapshot.network.observed_at_ms = 61_001;
    snapshot.services.secrets.availability = ServiceAvailability::Cached;
    snapshot.services.secrets.observed_at_ms = 1000;
    app.network_snapshot = std::sync::Arc::new(snapshot);
    assert!(app.open_network_lens());
    let before = app.stats_lens_fp(wid);
    assert_ne!(before, 0);
    app.network_snapshot = fresh_snapshot(app.network_snapshot.clone(), Some(61_001));
    assert_eq!(app.network_snapshot.revision, 0);
    assert_eq!(app.network_snapshot.network.visibility, Visibility::Offline);
    assert_ne!(app.stats_lens_fp(wid), before);
}

#[test]
fn invalid_service_clocks_never_establish_current_access() {
    for availability in [
        ServiceAvailability::Cached,
        ServiceAvailability::Current,
        ServiceAvailability::Unavailable,
    ] {
        for (at, now) in [
            (0, Some(100_000)),
            (100_001, Some(100_000)),
            (100_000, None),
        ] {
            let mut snapshot = NetworkStateSnapshot::default();
            snapshot.services.secrets.availability = availability;
            snapshot.services.secrets.observed_at_ms = at;
            let snapshot = std::sync::Arc::new(snapshot);
            let expired = fresh_snapshot(snapshot.clone(), now);
            assert_eq!(
                expired.services.secrets.availability,
                ServiceAvailability::Unknown
            );
            assert_eq!(expired.services.secrets.observed_at_ms, at);
            assert_eq!(snapshot.services.secrets.availability, availability);
        }
    }
}

#[test]
fn relayed_observations_name_their_source_without_refreshing_expired_evidence() {
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.visibility = Visibility::Active;
    snapshot.network.history = HistoryState::Ready;
    snapshot.network.observed_at_ms = 100_000;
    snapshot
        .network
        .peers
        .push(peer(2, Reachability::Reachable));
    let direct = summary(&snapshot);
    snapshot.network.via_local_process = Some(41);
    assert_ne!(summary(&snapshot), direct);
    assert!(
        details(&snapshot, Target::Overview)
            .iter()
            .any(|(key, value)| { key == "Observation source" && value.contains("41") })
    );
    let expired = fresh_snapshot(std::sync::Arc::new(snapshot), Some(115_001));
    assert_eq!(expired.network.visibility, Visibility::Unavailable);
    assert_eq!(expired.network.observed_at_ms, 100_000);
    assert_eq!(expired.network.via_local_process, Some(41));
    assert_eq!(
        observed_reachability(
            expired.network.visibility,
            expired.network.peers[0].reachability
        ),
        Reachability::Unknown
    );
    let mut offline = expired.as_ref().clone();
    offline.network.visibility = Visibility::Offline;
    let expired_offline = fresh_snapshot(std::sync::Arc::new(offline), Some(115_001));
    assert_eq!(expired_offline.network.visibility, Visibility::Unavailable);
    assert_eq!(expired_offline.network.observed_at_ms, 100_000);
}

#[test]
fn absent_observer_does_not_promise_a_pending_read() {
    let snapshot = inactive_snapshot();
    assert_eq!(snapshot.network.visibility, Visibility::Unavailable);
    assert_eq!(snapshot.network.history, HistoryState::Unavailable);
    assert!(!snapshot.network.detail.is_empty());
}

#[test]
fn changing_the_relay_source_invalidates_network_details() {
    let mut app = crate::App::headless_for_test();
    let wid = *app.windows.keys().next().expect("window");
    assert!(app.open_network_lens());
    let before = app.stats_lens_fp(wid);
    let mut snapshot = app.network_snapshot.as_ref().clone();
    snapshot.network.via_local_process = Some(42);
    app.network_snapshot = std::sync::Arc::new(snapshot);
    assert_ne!(app.stats_lens_fp(wid), before);
}

#[test]
fn account_inventory_history_survives_expiry_without_promoting_service_access() {
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.visibility = Visibility::Active;
    snapshot.network.observed_at_ms = 100_000;
    snapshot.services.accounts.availability = ServiceAvailability::Cached;
    snapshot.services.accounts.observed_at_ms = 100_000;
    let absent = details(&snapshot, Target::Accounts);
    assert!(absent.iter().any(|(key, value)| {
        key == "Peer inventory" && value.contains("No retained observation")
    }));
    snapshot.network.accounts_exchange =
        Some(aterm_types::network_state::AccountsExchangeObservation {
            scope_digest: [3; 32],
            local_actor: [4; 48],
            control_observation: [5; 64],
            inventory_count: 2,
            inventory_fingerprint: [6; 32],
            peer: MachineIdentity {
                machine_id: [7; 16],
                incarnation: [8; 16],
                label: Some("fixture-peer".into()),
            },
            sampled_at_ms: 90_000,
        });
    let original = snapshot.network.accounts_exchange.clone();
    let expired = fresh_snapshot(std::sync::Arc::new(snapshot), Some(160_001));
    assert_eq!(expired.network.accounts_exchange, original);
    assert_eq!(
        expired.services.accounts.availability,
        ServiceAvailability::Unknown
    );
    assert_eq!(
        expired.services.secrets.availability,
        ServiceAvailability::Unknown
    );
    let rows = details(&expired, Target::Accounts);
    assert!(rows.iter().any(|(key, value)| {
        key == "Peer inventory"
            && value.contains("2 retained operation IDs")
            && value.contains("fixture-peer")
    }));
    assert!(rows.iter().any(|(key, value)| {
        key == "Peer inventory sampled" && value == &observation_time(90_000)
    }));
    assert!(
        rows.iter()
            .any(|(key, value)| { key == "Inventory scope" && value.contains("Historical") })
    );
    assert!(
        !details(&expired, Target::Secrets)
            .iter()
            .any(|(key, _)| key == "Peer inventory")
    );
}

#[test]
fn vault_network_history_keeps_original_time_and_scope_after_local_observation_expires() {
    use aterm_types::network_state::{VaultNetworkMaterialization, VaultNetworkSource};
    for source in [
        VaultNetworkSource::ConfiguredUrl,
        VaultNetworkSource::PeerUrl,
    ] {
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.services.secrets.availability = ServiceAvailability::Cached;
        snapshot.services.secrets.observed_at_ms = 100_000;
        let absent = details(&snapshot, Target::Secrets);
        assert!(
            absent.iter().any(|(key, value)| key == "Network sync"
                && value.contains("No verified network receipt"))
        );
        snapshot.services.secrets.network_materialization = Some(VaultNetworkMaterialization {
            source,
            sequence: 17,
            sampled_at_ms: 80_000,
        });
        let expired = fresh_snapshot(std::sync::Arc::new(snapshot), Some(160_001));
        assert_eq!(
            expired.services.secrets.availability,
            ServiceAvailability::Unknown
        );
        let receipt = expired.services.secrets.network_materialization.unwrap();
        assert_eq!(receipt.sampled_at_ms, 80_000);
        let rows = details(&expired, Target::Secrets);
        let route = match source {
            VaultNetworkSource::ConfiguredUrl => "configured route",
            VaultNetworkSource::PeerUrl => "peer route",
        };
        assert!(rows.iter().any(|(key, value)| key == "Network sync"
            && value.contains("Signed vault version 17")
            && value.contains(route)));
        assert!(rows.iter().any(
            |(key, value)| key == "Network fetch started" && value == &observation_time(80_000)
        ));
        assert!(rows.iter().any(
            |(key, value)| key == "Network sync scope" && value.contains("Historical receipt")
        ));
        assert!(
            !details(&expired, Target::Accounts)
                .iter()
                .any(|(key, _)| key == "Network sync")
        );
    }
}

#[test]
fn retained_204_details_do_not_turn_transport_contact_into_local_access() {
    use aterm_types::network_state::{VaultHttp204Response, VaultNetworkSource};
    for availability in [
        ServiceAvailability::Unknown,
        ServiceAvailability::Unavailable,
        ServiceAvailability::Cached,
    ] {
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.services.secrets.availability = availability;
        snapshot.services.secrets.observed_at_ms = 100_000;
        let absent = details(&snapshot, Target::Secrets);
        assert!(
            absent
                .iter()
                .any(|(key, value)| key == "Retained HTTP 204" && !value.is_empty())
        );
        let response = VaultHttp204Response {
            source: VaultNetworkSource::ConfiguredUrl,
            responded_at_ms: 80_000,
        };
        snapshot.services.secrets.http204_response = Some(response);
        let rows = details(&snapshot, Target::Secrets);
        assert_eq!(snapshot.services.secrets.availability, availability);
        assert_eq!(snapshot.services.secrets.network_materialization, None);
        assert_eq!(snapshot.services.secrets.last_success_ms, None);
        assert!(rows.iter().any(|(key, value)| key == "Retained HTTP 204"
            && value.contains("Configured route answered HTTP 204")));
        assert!(rows.iter().any(|(key, value)| key == "Response received"
            && value == &observation_time(response.responded_at_ms)));
        assert!(
            !details(&snapshot, Target::Accounts)
                .iter()
                .any(|(key, _)| key == "Retained HTTP 204")
        );
        let expired = fresh_snapshot(std::sync::Arc::new(snapshot), Some(160_001));
        assert_eq!(expired.services.secrets.http204_response, Some(response));
        assert_eq!(
            expired.services.secrets.availability,
            ServiceAvailability::Unknown
        );
    }
}

#[test]
fn network_refresh_failure_keeps_cache_and_success_receipt_separate_in_the_lens() {
    use aterm_types::network_state::{
        VaultNetworkMaterialization, VaultNetworkRefresh, VaultNetworkRefreshOutcome,
        VaultNetworkSource,
    };
    let mut snapshot = NetworkStateSnapshot::default();
    let service = &mut snapshot.services.secrets;
    service.availability = ServiceAvailability::Cached;
    service.observed_at_ms = 100_000;
    service.refresh_failed = true;
    let receipt = VaultNetworkMaterialization {
        source: VaultNetworkSource::ConfiguredUrl,
        sequence: 17,
        sampled_at_ms: 80_000,
    };
    service.network_materialization = Some(receipt);
    for outcome in [
        VaultNetworkRefreshOutcome::Failed,
        VaultNetworkRefreshOutcome::Succeeded,
    ] {
        let refresh = VaultNetworkRefresh {
            completed_at_ms: 90_000,
            outcome,
        };
        snapshot.services.secrets.network_refresh = Some(refresh);
        snapshot.services.secrets.refresh_failed = outcome == VaultNetworkRefreshOutcome::Failed;
        let rows = details(&snapshot, Target::Secrets);
        let expected = if outcome == VaultNetworkRefreshOutcome::Failed {
            "Failed"
        } else {
            "Completed"
        };
        assert!(
            rows.iter()
                .any(|(key, value)| key == "Recorded network refresh" && value == expected)
        );
        assert!(
            rows.iter().any(
                |(key, value)| key == "Refresh completed" && value == &observation_time(90_000)
            )
        );
        assert!(rows.iter().any(
            |(key, value)| key == "Network fetch started" && value == &observation_time(80_000)
        ));
        assert_eq!(
            snapshot.services.secrets.availability,
            ServiceAvailability::Cached
        );
        assert_eq!(
            snapshot.services.secrets.network_materialization,
            Some(receipt)
        );
        assert_eq!(snapshot.services.secrets.last_success_ms, None);
        assert!(
            !details(&snapshot, Target::Accounts)
                .iter()
                .any(|(key, _)| key == "Recorded network refresh")
        );
        let expired = fresh_snapshot(std::sync::Arc::new(snapshot.clone()), Some(160_001));
        assert_eq!(
            expired.services.secrets.availability,
            ServiceAvailability::Unknown
        );
        assert_eq!(expired.services.secrets.network_refresh, Some(refresh));
        assert_eq!(
            expired.services.secrets.network_materialization,
            Some(receipt)
        );
    }
}

#[test]
fn an_inventory_observation_invalidates_an_open_network_lens_without_revision_change() {
    let mut app = crate::App::headless_for_test();
    let wid = *app.windows.keys().next().expect("window");
    assert!(app.open_network_lens());
    let before = app.stats_lens_fp(wid);
    let mut snapshot = app.network_snapshot.as_ref().clone();
    snapshot.network.accounts_exchange =
        Some(aterm_types::network_state::AccountsExchangeObservation {
            scope_digest: [1; 32],
            local_actor: [2; 48],
            control_observation: [3; 64],
            inventory_count: 0,
            inventory_fingerprint: [4; 32],
            peer: MachineIdentity {
                machine_id: [5; 16],
                incarnation: [6; 16],
                label: None,
            },
            sampled_at_ms: 1,
        });
    app.network_snapshot = std::sync::Arc::new(snapshot);
    assert_ne!(app.stats_lens_fp(wid), before);
}

#[test]
fn explicit_network_closeup_keeps_hidden_deck_and_terminal_geometry() {
    let mut app = crate::App::headless_for_test();
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("off".into());
    let wid = *app.windows.keys().next().expect("window");
    let before = (
        app.windows[&wid].rows,
        app.windows[&wid].cols,
        app.windows[&wid].metrics.head,
    );
    assert!(app.open_network_lens());
    assert!(app.lens_frame(wid).is_some());
    assert_eq!(app.stats_bars_form(), crate::stats_deck::StatsBars::Off);
    assert_eq!(
        (
            app.windows[&wid].rows,
            app.windows[&wid].cols,
            app.windows[&wid].metrics.head
        ),
        before
    );
}
