// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
#[test]
fn place_only_changes_rekey_and_repaint_the_network_lens_with_or_without_bars() {
    let _attached = crate::stats_deck::tests::attach_stats();
    let _place = crate::net_place::hold_scoped(None);
    for bars in ["deck", "sill", "off"] {
        crate::net_place::hold(Some(aterm_netplace::sample::home(1000, 1)));
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_bars = Some(bars.into());
        let head = crate::stats_deck::reserved_deck_px(
            crate::stats_deck::StatsBars::Deck,
            app.windows[&wid].deck_form,
            app.windows[&wid].scale,
            app.win_cell_size(wid).1,
        ) + 14;
        {
            let metrics = &mut app.windows.get_mut(&wid).unwrap().metrics;
            metrics.head = head;
            metrics.pad = 12;
            metrics.pad_top = 2;
        }
        app.backend.set_head(head);
        app.backend.set_pad(12);
        app.backend.set_pad_top(2);
        assert!(app.open_network_lens());
        assert_eq!(app.deck_frame(wid).is_none(), bars == "off");
        let grid = (app.windows[&wid].cols, app.windows[&wid].rows);
        let network = app.network_snapshot.clone();
        let (geom, lens, before) = app.lens_frame(wid).unwrap();
        assert_eq!(before.form.word(), bars);
        let before_key = app.stats_lens_fp_for(wid, &geom, &lens, &before);
        let before_prims =
            crate::stats_deck::prims_fp(&lens_paint(&geom, &app.lens_input(&before, wid), &lens));
        app.splice_stats_lens(wid);
        let before_pixels = app.windows[&wid].lens_card.as_ref().unwrap().rgba.clone();

        let mut away = aterm_netplace::sample::hotspot(2000, 2000);
        crate::net_place::hold(Some(away.clone()));
        assert_eq!(
            app.stats_lens_fp_for(wid, &geom, &lens, &before),
            before_key
        );
        assert_eq!(
            crate::stats_deck::prims_fp(&lens_paint(&geom, &app.lens_input(&before, wid), &lens)),
            before_prims
        );
        let (after_geom, after_lens, after) = app.lens_frame(wid).unwrap();
        assert_eq!(after.form.word(), bars);
        let after_key = app.stats_lens_fp_for(wid, &after_geom, &after_lens, &after);
        assert_ne!(after_key, before_key);
        app.splice_stats_lens(wid);
        let card = app.windows[&wid].lens_card.as_ref().unwrap();
        assert_eq!(card.fp, after_key);
        assert_ne!(card.rgba, before_pixels);
        assert!(std::sync::Arc::ptr_eq(&app.network_snapshot, &network));
        assert!(app.vitals.snapshot.is_none());
        assert_eq!((app.windows[&wid].cols, app.windows[&wid].rows), grid);

        let after_pixels = card.rgba.clone();
        away.since_ms = Some(9000);
        away.alab = aterm_netplace::Alab::Away {
            since_ms: Some(8000),
        };
        crate::net_place::hold(Some(away));
        assert_eq!(app.stats_lens_fp(wid), after_key);
        app.splice_stats_lens(wid);
        assert_eq!(
            app.windows[&wid].lens_card.as_ref().unwrap().rgba,
            after_pixels
        );
    }
}

fn render_presence(
    samples: &[aterm_types::network_state::PresenceSample],
    observed: u64,
    metrics: &FaceMetrics,
    width: f64,
    scale: f32,
) -> (Vec<DrawPrim>, Vec<String>, Vec<TextBox>) {
    use aterm_types::network_state::{
        HistoryState, MachineIdentity, NetworkStateSnapshot, PeerObservation, Reachability,
        Visibility,
    };

    let resource = Snapshot::default();
    let input = crate::stats_deck::tests::input(&resource);
    let inks = input.inks;
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.visibility = Visibility::Active;
    snapshot.network.history = HistoryState::Ready;
    snapshot.network.observed_at_ms = observed;
    snapshot.network.peers.push(PeerObservation {
        slot: 1,
        identity: MachineIdentity {
            machine_id: [2; 16],
            incarnation: [3; 16],
            label: None,
        },
        first_seen_ms: samples.first().map_or(observed, |sample| sample.at_ms),
        last_seen_ms: observed,
        last_checked_ms: Some(observed),
        last_repair: None,
        last_authenticated_endpoint: None,
        reachability: Reachability::Reachable,
        instances: None,
        samples: samples.to_vec(),
        lease_ms: None,
    });
    let doc = lab_network_doc(
        Some(&snapshot),
        None,
        None,
        crate::network_deck::Target::Seat(crate::network_deck::SeatKey::Observed(1)),
        None,
        metrics,
        width,
    );
    let section = doc
        .sections
        .iter()
        .find(|section| section.name == "Observed presence")
        .expect("selected peer history");
    assert_eq!(section.forms[0].h, 42.0);
    let mut pen = Pen {
        p: Painter::new(&inks, scale, 14.0 * scale, false),
        m: metrics,
        inks,
        ic: false,
        s: f64::from(scale),
        ox: 0.0,
        oy: 0.0,
        draw: true,
        line_tab: 0.0,
        boxes: Vec::new(),
    };
    (section.forms[0].draw)(
        &mut pen,
        Place {
            x: 0.0,
            y: 0.0,
            w: width,
            h: 42.0,
        },
    );
    (pen.p.prims, section.words.clone(), pen.boxes)
}

fn distinct_presence_marks(xs: &[f32], scale: f32) -> bool {
    xs.windows(2).all(|pair| pair[1] - pair[0] >= 4.0 * scale)
}

#[test]
fn rollback_and_duplicate_samples_keep_distinct_states_in_the_actual_painter() {
    use aterm_types::network_state::{PresenceSample, Reachability};
    let resource = Snapshot::default();
    let inks = crate::stats_deck::tests::input(&resource).inks;
    for (times, observed) in [
        (vec![100_000, 90_000, 90_001], 90_001),
        // A frozen wall clock can record contact and its later monotonic expiry equally.
        (vec![100_000, 100_000], 100_000),
    ] {
        let samples: Vec<_> = times
            .iter()
            .enumerate()
            .map(|(index, &at_ms)| PresenceSample {
                at_ms,
                state: if index == 1 {
                    Reachability::LastSeen
                } else {
                    Reachability::Reachable
                },
            })
            .collect();
        for metrics in [FaceMetrics::helvetica_neue(), FaceMetrics::test_stack()] {
            for width in [220.0, 420.0] {
                for scale in [1.0, 2.0] {
                    let (prims, words, boxes) =
                        render_presence(&samples, observed, &metrics, width, scale);
                    let marks: Vec<_> = prims
                        .iter()
                        .filter_map(|prim| match prim {
                            DrawPrim::Panel { x, w, h, fill, .. }
                                if *w == 4.0 * scale && *h == 7.0 * scale =>
                            {
                                Some((*x, *fill))
                            }
                            _ => None,
                        })
                        .collect();
                    let xs: Vec<_> = marks.iter().map(|(x, _)| *x).collect();
                    assert_eq!(xs.len(), samples.len());
                    assert!(distinct_presence_marks(&xs, scale));
                    assert_eq!(xs[0], 0.0);
                    assert_eq!(
                        *xs.last().unwrap(),
                        ((width - 8.0) * f64::from(scale)) as f32
                    );
                    assert!(xs.iter().all(|x| x.is_finite()
                        && *x >= 0.0
                        && *x + 4.0 * scale <= width as f32 * scale));
                    for ((_, fill), sample) in marks.iter().zip(&samples) {
                        let ink = if sample.state == Reachability::Reachable {
                            inks.machine
                        } else {
                            inks.hollow
                        };
                        assert_eq!(*fill, rgba(ink, 255));
                    }
                    assert_eq!(
                        prims
                            .iter()
                            .filter(|prim| matches!(prim,
                                DrawPrim::Panel { w, h, .. }
                                    if *w == 2.0 * scale && *h == 5.0 * scale
                            ))
                            .count(),
                        1
                    );
                    assert!(boxes.iter().any(|text| text.text == "Recorded order"));
                    assert!(
                        boxes
                            .iter()
                            .all(|text| text.r[0] >= 0.0 && text.r[2] <= width + EPS)
                    );
                    assert!(words.iter().any(|words| words.contains("recorded order")
                        && words.contains("does not represent elapsed time")));
                    // The previous formula collapses these same producer histories.
                    let start = samples[0].at_ms;
                    let span = observed.saturating_sub(start).max(1);
                    let former: Vec<_> = samples
                        .iter()
                        .map(|sample| {
                            (sample.at_ms.saturating_sub(start).min(span) as f64 / span as f64
                                * (width - 8.0)
                                * f64::from(scale)) as f32
                        })
                        .collect();
                    assert!(!distinct_presence_marks(&former, scale));
                }
            }
        }
    }
}

#[test]
fn presence_axis_keeps_elapsed_spacing_only_with_strictly_increasing_clock_evidence() {
    use aterm_types::network_state::{PresenceSample, Reachability};
    let samples = |times: &[u64]| {
        times
            .iter()
            .map(|&at_ms| PresenceSample {
                at_ms,
                state: Reachability::Reachable,
            })
            .collect::<Vec<_>>()
    };
    let ordinary = samples(&[90_000, 95_000, 110_000]);
    let axis = PresenceAxis::for_samples(&ordinary, 130_000);
    assert!(matches!(axis, PresenceAxis::Elapsed { .. }));
    assert_eq!(
        [
            axis.fraction(0, 90_000),
            axis.fraction(1, 95_000),
            axis.fraction(2, 110_000)
        ],
        [0.0, 0.125, 0.5]
    );
    for (times, observed) in [
        (vec![], 90_000),
        (vec![90_000], 90_000),
        (vec![100_000], 90_000),
        (vec![90_000, 100_000], 95_000),
        (vec![90_000, 80_000, 100_000], 110_000),
        (vec![u64::MAX, 0, 1], 1),
        (vec![0, u64::MAX], u64::MAX),
    ] {
        let samples = samples(&times);
        let axis = PresenceAxis::for_samples(&samples, observed);
        let positions: Vec<_> = samples
            .iter()
            .enumerate()
            .map(|(index, sample)| axis.fraction(index, sample.at_ms))
            .collect();
        assert!(
            positions
                .iter()
                .all(|x| x.is_finite() && (0.0..=1.0).contains(x))
        );
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        if times.last().is_some_and(|last| *last > observed)
            || times.windows(2).any(|pair| pair[0] >= pair[1])
        {
            assert!(matches!(axis, PresenceAxis::Recorded { .. }));
        }
    }
    // The ordinary painter must retain its unequal elapsed gaps and original caption.
    let (prims, words, boxes) =
        render_presence(&ordinary, 130_000, &FaceMetrics::test_stack(), 420.0, 1.0);
    let xs: Vec<_> = prims
        .iter()
        .filter_map(|prim| match prim {
            DrawPrim::Panel { x, w, h, .. } if *w == 4.0 && *h == 7.0 => Some(*x),
            _ => None,
        })
        .collect();
    assert_eq!(xs, [0.0, 52.0, 206.0]);
    assert!(boxes.iter().any(|text| text.text == "Observed presence"));
    assert!(words.iter().all(|words| !words.contains("spacing")));
}

fn retained_peer() -> aterm_types::network_state::NetworkStateSnapshot {
    use aterm_types::network_state::{
        AuthenticatedEndpoint, HistoryState, MachineIdentity, NetworkStateSnapshot,
        PeerObservation, Reachability, RepairCause, RepairObservation, RepairOutcome, RepairStage,
        Visibility,
    };

    let mut network = NetworkStateSnapshot::default();
    network.network.visibility = Visibility::Offline;
    network.network.history = HistoryState::Ready;
    network.network.observed_at_ms = 1_793_040_000_000;
    network.network.peers.push(PeerObservation {
        slot: 7,
        identity: MachineIdentity {
            machine_id: [0x23; 16],
            incarnation: [0x42; 16],
            label: None,
        },
        first_seen_ms: 1_793_030_000_000,
        last_seen_ms: 1_793_040_000_000,
        last_checked_ms: None,
        last_repair: Some(RepairObservation {
            completed_at_ms: 1_793_000_000_000,
            outbound: true,
            stage: Some(RepairStage::Application),
            outcome: RepairOutcome::Failed(RepairCause::Io),
            table: Some([7; 32]),
            sent: Some(7),
            received: None,
            deferred: Some(2),
        }),
        last_authenticated_endpoint: Some(AuthenticatedEndpoint {
            address: "[2001:db8::7]:9699".parse().unwrap(),
            observed_at_ms: 1_793_000_000_000,
        }),
        reachability: Reachability::LastSeen,
        instances: None,
        samples: Vec::new(),
        lease_ms: None,
    });
    network
}

#[test]
fn short_peer_lenses_keep_the_original_repair_time_with_every_recorded_outcome() {
    use aterm_types::network_state::{RepairCause, RepairOutcome};

    let mut app = crate::App::headless_for_test();
    let wid = *app.windows.keys().next().unwrap();
    app.network_source = None;
    assert!(app.open_network_lens());
    let target = crate::network_deck::Target::Seat(crate::network_deck::SeatKey::Observed(7));
    app.windows.get_mut(&wid).unwrap().stats_lens.network_target = target;
    let resource = Snapshot::default();
    let lens = Lens {
        focus: Instrument::NetworkState,
        bar: Bar::Global,
        how: Opened::Keyboard,
    };
    for outcome in [
        Some(RepairOutcome::Failed(RepairCause::Io)),
        Some(RepairOutcome::Deferred(None)),
        Some(RepairOutcome::Exchanged),
        Some(RepairOutcome::Cancelled),
        Some(RepairOutcome::NotObserved),
        Some(RepairOutcome::NoCommonTable),
        None,
    ] {
        let mut network = retained_peer();
        let label = "fixture-peer".to_owned();
        network.network.peers[0].identity.label = Some(label.clone());
        // A visible endpoint or last-seen time cannot satisfy the repair-time check.
        network.network.peers[0]
            .last_authenticated_endpoint
            .as_mut()
            .unwrap()
            .observed_at_ms -= 60_000;
        network.network.peers[0].last_repair = outcome.map(|outcome| {
            let mut repair = network.network.peers[0].last_repair.clone().unwrap();
            repair.outcome = outcome;
            repair
        });
        let expected = crate::network_deck::details(&network, target);
        let value = |name| {
            expected
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value)
        };
        let result = value("Last repair").unwrap().split(" · ").next().unwrap();
        let original_time = network.network.peers[0]
            .last_repair
            .as_ref()
            .map(|repair| aterm_types::rfc3339::format_rfc3339(repair.completed_at_ms / 1000));
        for metrics in [FaceMetrics::helvetica_neue(), FaceMetrics::test_stack()] {
            for scale in [1.0_f32, 2.0] {
                let mut input = crate::stats_deck::tests::input(&resource);
                input.network = Some(&network);
                input.network_target = target;
                let geometry = lens_geom(
                    StatsBars::Deck,
                    0,
                    80.0 * scale,
                    0.0,
                    80.0 * scale,
                    420.0 * scale,
                    196.0 * scale,
                    scale,
                    14.0 * scale,
                )
                .unwrap();
                let laid = lay_lens(&geometry, &input, &lens, &metrics, true);
                let drawn = laid
                    .prims
                    .iter()
                    .filter_map(|prim| match prim {
                        DrawPrim::Text { s, .. } => Some(s.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                assert!(drawn.contains("LastSeen"), "contact omitted: {drawn}");
                assert!(drawn.contains(result), "repair outcome omitted: {drawn}");
                if let Some(time) = &original_time {
                    assert!(
                        drawn.contains(time),
                        "original repair time omitted: {drawn}"
                    );
                } else {
                    assert!(laid.wells.iter().all(|(name, _)| name != "Repair time"));
                }
            }
        }
        app.network_snapshot = std::sync::Arc::new(network);
        let copied = app.stats_lens_sentence(wid).unwrap();
        assert!(copied.contains(&label));
        assert!(copied.contains("23232323232323232323232323232323"));
        assert!(copied.contains("42424242424242424242424242424242"));
        for (_, value) in &expected {
            assert!(
                copied.contains(value),
                "retained field omitted from copy: {value}"
            );
        }
    }
}

#[test]
fn selected_peer_prioritizes_contact_evidence_and_copies_complete_identity() {
    let resource = Snapshot::default();
    let network = retained_peer();
    let mut input = crate::stats_deck::tests::input(&resource);
    input.network = Some(&network);
    input.network_target =
        crate::network_deck::Target::Seat(crate::network_deck::SeatKey::Observed(7));
    let lens = Lens {
        focus: Instrument::NetworkState,
        bar: Bar::Global,
        how: Opened::Keyboard,
    };
    for (metrics, width, height) in [
        (FaceMetrics::helvetica_neue(), 420.0, 196.0),
        (FaceMetrics::helvetica_neue(), 840.0, 600.0),
        (FaceMetrics::test_stack(), 420.0, 196.0),
        (FaceMetrics::test_stack(), 840.0, 600.0),
    ] {
        let geometry = lens_geom(
            StatsBars::Deck,
            0,
            80.0,
            0.0,
            80.0,
            width,
            height,
            1.0,
            14.0,
        )
        .unwrap();
        let laid = lay_lens(&geometry, &input, &lens, &metrics, true);
        let drawn = laid
            .prims
            .iter()
            .filter_map(|prim| match prim {
                DrawPrim::Text { s, .. } => Some(s.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(drawn.contains("LastSeen"), "contact omitted: {drawn}");
        assert!(drawn.contains("Failed"), "repair outcome omitted: {drawn}");
        assert!(
            laid.wells
                .iter()
                .any(|(name, words)| { name == "Observation" && words.contains("LastSeen") })
        );
        if height > 196.0 {
            assert!(
                drawn.contains("[2001:db8::7]:9699"),
                "endpoint omitted: {drawn}"
            );
            assert!(
                drawn.contains("historical"),
                "historical scope omitted: {drawn}"
            );
            assert!(
                drawn.contains("23232323232323232323232323232323"),
                "machine identity omitted: {drawn}"
            );
            assert!(
                drawn.contains("42424242424242424242424242424242"),
                "incarnation omitted: {drawn}"
            );
        }
    }

    let mut app = crate::App::headless_for_test();
    let wid = *app.windows.keys().next().unwrap();
    app.network_source = None;
    app.network_snapshot = std::sync::Arc::new(network);
    assert!(app.open_network_lens());
    app.windows.get_mut(&wid).unwrap().stats_lens.network_target =
        crate::network_deck::Target::Seat(crate::network_deck::SeatKey::Observed(7));
    let copied = app.stats_lens_sentence(wid).unwrap();
    assert!(copied.contains("23232323232323232323232323232323"));
    assert!(copied.contains("42424242424242424242424242424242"));
    assert!(copied.contains("[2001:db8::7]:9699"));
    assert!(copied.contains("2026-10-26T07:33:20Z"));
    assert!(copied.contains("historical"));
}

#[test]
fn overview_observation_time_is_visible() {
    use aterm_types::network_state::{HistoryState, Visibility};
    let resource = Snapshot::default();
    let mut network = aterm_types::network_state::NetworkStateSnapshot::default();
    network.network.visibility = Visibility::Waiting;
    network.network.history = HistoryState::Ready;
    network.network.detail = "Another process owns repair".into();
    network.network.observed_at_ms = 1_793_040_000_000;
    let mut input = crate::stats_deck::tests::input(&resource);
    input.network = Some(&network);
    input.network_target = crate::network_deck::Target::Overview;
    let lens = Lens {
        focus: Instrument::NetworkState,
        bar: Bar::Global,
        how: Opened::Keyboard,
    };
    let geometry = lens_geom(StatsBars::Deck, 0, 80.0, 0.0, 80.0, 840.0, 300.0, 1.0, 14.0).unwrap();
    let laid = lay_lens(
        &geometry,
        &input,
        &lens,
        &FaceMetrics::helvetica_neue(),
        true,
    );
    let drawn = laid
        .prims
        .iter()
        .filter_map(|prim| match prim {
            DrawPrim::Text { s, .. } => Some(s.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let time = aterm_types::rfc3339::format_rfc3339(network.network.observed_at_ms / 1000);
    assert!(drawn.contains(&time), "observation time missing: {drawn}");
    assert!(drawn.contains(&network.network.detail));
}

#[test]
fn blocked_overview_keeps_the_observer_reason_in_a_short_lens() {
    use aterm_types::network_state::{HistoryState, Visibility};
    let resource = Snapshot::default();
    let mut network = aterm_types::network_state::NetworkStateSnapshot::default();
    network.network.visibility = Visibility::Waiting;
    network.network.history = HistoryState::Ready;
    network.network.detail =
        "Another observer holds the repair lease; waiting for current network evidence".into();
    network.services.accounts.detail =
        "Account metadata is retained separately from provider sign-in".into();
    network.services.accounts.revision = Some("account revision".into());
    network.services.secrets.detail =
        "Local vault metadata does not establish remote currency".into();
    network.services.secrets.revision = Some("vault revision".into());
    let mut input = crate::stats_deck::tests::input(&resource);
    input.network = Some(&network);
    input.network_target = crate::network_deck::Target::Overview;
    let lens = Lens {
        focus: Instrument::NetworkState,
        bar: Bar::Global,
        how: Opened::Keyboard,
    };
    for (width, height) in [(420.0, 196.0), (840.0, 300.0)] {
        let geometry = lens_geom(
            StatsBars::Deck,
            0,
            80.0,
            0.0,
            80.0,
            width,
            height,
            1.0,
            14.0,
        )
        .unwrap();
        let laid = lay_lens(
            &geometry,
            &input,
            &lens,
            &FaceMetrics::helvetica_neue(),
            true,
        );
        let drawn = laid
            .prims
            .iter()
            .filter_map(|prim| match prim {
                DrawPrim::Text { s, .. } => Some(s.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            drawn.contains(&network.network.detail),
            "observer reason was omitted at {width} by {height}: {drawn}"
        );
    }
}

#[test]
fn long_observer_reason_keeps_a_visible_prefix_and_complete_report() {
    use aterm_types::network_state::{HistoryState, Visibility};
    let resource = Snapshot::default();
    let mut network = aterm_types::network_state::NetworkStateSnapshot::default();
    network.network.visibility = Visibility::Waiting;
    network.network.history = HistoryState::Ready;
    network.network.detail = format!(
        "Observer holds repair lease; {}",
        "retained evidence ".repeat(80)
    );
    let mut input = crate::stats_deck::tests::input(&resource);
    input.network = Some(&network);
    input.network_target = crate::network_deck::Target::Overview;
    let lens = Lens {
        focus: Instrument::NetworkState,
        bar: Bar::Global,
        how: Opened::Keyboard,
    };
    let geometry = lens_geom(StatsBars::Deck, 0, 80.0, 0.0, 80.0, 420.0, 196.0, 1.0, 14.0).unwrap();
    let laid = lay_lens(
        &geometry,
        &input,
        &lens,
        &FaceMetrics::helvetica_neue(),
        true,
    );
    let drawn = laid
        .prims
        .iter()
        .filter_map(|prim| match prim {
            DrawPrim::Text { s, .. } => Some(s.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        drawn.contains("Observer holds repair lease"),
        "reason prefix is missing: {drawn}"
    );
    assert!(
        laid.wells
            .iter()
            .any(|(_, words)| words == &network.network.detail)
    );
}

#[test]
fn network_service_explanations_are_drawn_in_full_at_both_widths() {
    let resource = Snapshot::default();
    let mut input = crate::stats_deck::tests::input(&resource);
    let mut network = aterm_types::network_state::NetworkStateSnapshot::default();
    let detail = (0..60)
        .map(|n| format!("detail{n}"))
        .collect::<Vec<_>>()
        .join(" ");
    network.services.secrets.detail = detail.clone();
    input.network = Some(&network);
    input.network_target = crate::network_deck::Target::Secrets;
    let lens = Lens {
        focus: Instrument::NetworkState,
        bar: Bar::Global,
        how: Opened::Keyboard,
    };
    for (width, height) in [(420.0, 260.0), (840.0, 600.0)] {
        let geometry = lens_geom(
            StatsBars::Deck,
            0,
            80.0,
            0.0,
            80.0,
            width,
            height,
            1.0,
            14.0,
        )
        .unwrap();
        let laid = lay_lens(
            &geometry,
            &input,
            &lens,
            &FaceMetrics::helvetica_neue(),
            true,
        );
        assert_eq!(
            laid.stacks.len(),
            1,
            "Network State uses the full panel width"
        );
        let drawn = laid
            .prims
            .iter()
            .filter_map(|prim| match prim {
                DrawPrim::Text { s, .. } => Some(s.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            drawn.contains(&detail),
            "service explanation was cut for a {width} by {height} grid"
        );
    }
}

#[test]
fn account_catalog_status_and_scope_remain_visible_in_a_short_lens() {
    use aterm_types::network_state::ServiceAvailability;
    let resource = Snapshot::default();
    let lens = Lens {
        focus: Instrument::NetworkState,
        bar: Bar::Global,
        how: Opened::Keyboard,
    };
    for (availability, detail) in [
        (
            ServiceAvailability::Unavailable,
            "Shared account catalog has not been received or initialized",
        ),
        (
            ServiceAvailability::Cached,
            "2 account proposals retained; catalog access does not prove login readiness or remote currency",
        ),
    ] {
        let mut network = aterm_types::network_state::NetworkStateSnapshot::default();
        network.services.accounts.availability = availability;
        network.services.accounts.detail = detail.into();
        network.services.accounts.observed_at_ms = 1_793_040_000_000;
        network.services.accounts.last_success_ms = Some(1_793_040_000_000);
        network.services.accounts.revision = Some("retained catalog revision".into());
        let mut input = crate::stats_deck::tests::input(&resource);
        input.network = Some(&network);
        input.network_target = crate::network_deck::Target::Accounts;
        let expected = crate::network_deck::details(&network, input.network_target);
        assert_eq!(
            expected[1].0, "Scope",
            "scope must follow the selected service status"
        );
        for (width, height) in [(420.0, 196.0), (840.0, 600.0)] {
            let geometry = lens_geom(
                StatsBars::Deck,
                0,
                80.0,
                0.0,
                80.0,
                width,
                height,
                1.0,
                14.0,
            )
            .unwrap();
            let laid = lay_lens(
                &geometry,
                &input,
                &lens,
                &FaceMetrics::helvetica_neue(),
                true,
            );
            let drawn = laid
                .prims
                .iter()
                .filter_map(|prim| match prim {
                    DrawPrim::Text { s, .. } => Some(s.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            for (_, value) in &expected[..2] {
                assert!(
                    drawn.contains(value),
                    "selected catalog status or scope was cut at {width} by {height}"
                );
            }
        }
    }
}

#[test]
fn network_details_do_not_require_a_resource_reading() {
    let resource = Snapshot::default();
    let mut input = crate::stats_deck::tests::input(&resource);
    input.snap = None;
    let mut network = aterm_types::network_state::NetworkStateSnapshot::default();
    network.services.secrets.availability = aterm_types::network_state::ServiceAvailability::Cached;
    network.services.secrets.refresh_failed = true;
    input.network = Some(&network);
    input.network_target = crate::network_deck::Target::Secrets;
    let lens = Lens {
        focus: Instrument::NetworkState,
        bar: Bar::Global,
        how: Opened::Keyboard,
    };
    let geometry = LensGeom {
        dx: 0,
        dy: 0,
        w: 800.0,
        h: 400.0,
        margin: 8.0,
        scale: 1.0,
        font_px: 14.0,
    };
    let laid = lay_lens(
        &geometry,
        &input,
        &lens,
        &FaceMetrics::helvetica_neue(),
        false,
    );
    let expected = crate::network_deck::details(&network, input.network_target);
    assert!(expected.iter().all(|row| laid.wells.contains(row)));
    assert!(!laid.wells.is_empty());
}

#[test]
fn network_copy_is_available_without_resource_reading() {
    let mut app = crate::App::headless_for_test();
    app.vitals.snapshot = None;
    let wid = *app.windows.keys().next().expect("window");
    assert!(app.open_network_lens());
    assert_eq!(app.stats_lens_copy_ready(wid), Some(true));
    assert!(app.stats_lens_sentence(wid).is_some());
    app.windows
        .get_mut(&wid)
        .unwrap()
        .stats_lens
        .open
        .as_mut()
        .unwrap()
        .focus = Instrument::Cpu;
    assert_eq!(app.stats_lens_copy_ready(wid), Some(false));
}
