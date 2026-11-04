// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Lab's observations through the Settings controller and compiled semantic tree.

use super::*;
use crate::lab_accounts::{LabAccount, LabAccounts};
use crate::lab_screen::LabSession;
use crate::native_app::{AppEffect, AppViewState, NativeApp, NativeRuntime};
use crate::native_ui::CompiledUi;
use aterm_types::network_state::{
    HistoryState, MachineIdentity, NetworkStateSnapshot, PeerObservation, Reachability,
};

fn fixture() -> LabState {
    use aterm_agent::harness::account_progress::{
        AccountProgress, OwnClaimBlocker, OwnClaimFinding,
    };
    let mut progress = AccountProgress::unknown(100);
    progress.account = Some("fixture-account".into());
    progress.phase = aterm_agent::harness::account_progress::ProgressPhase::Waiting;
    progress.own_claim = Some(OwnClaimFinding {
        blocker: OwnClaimBlocker::HeldElsewhere { until: 200 },
        pinned: true,
    });
    let claim_rows = crate::lab_screen::progress_facts_at(Some(&progress), Some(150))
        .into_iter()
        .filter(|(name, _)| {
            matches!(
                name.as_str(),
                "Own account claim" | "Claim recorded until" | "Claim timing" | "Switch blocker"
            )
        });
    let mut network = NetworkStateSnapshot::default();
    network.network.visibility = aterm_types::network_state::Visibility::Active;
    network.network.history = HistoryState::Ready;
    network.network.observed_at_ms = 100_000;
    network.network.detail = "Discovery observations retained".into();
    network.services.accounts.availability = ServiceAvailability::Cached;
    network.services.accounts.observed_at_ms = 100_000;
    network.services.accounts.detail = "Two retained catalog proposals".into();
    network.services.secrets.availability = ServiceAvailability::Cached;
    network.services.secrets.observed_at_ms = 100_000;
    network.services.secrets.detail = "Three authorized manifest entries; no secret values".into();
    network.network.peers.push(PeerObservation {
        slot: 7,
        identity: MachineIdentity {
            machine_id: [1; 16],
            incarnation: [2; 16],
            label: Some("fixture-peer".into()),
        },
        first_seen_ms: 90_000,
        last_seen_ms: 100_000,
        last_checked_ms: Some(100_000),
        last_repair: None,
        last_authenticated_endpoint: None,
        reachability: Reachability::Reachable,
        instances: None,
        samples: Vec::new(),
    });
    LabState {
        network: Arc::new(network),
        accounts: LabAccounts {
            observed_at_ms: Some(100_000),
            status: "One configured Claude account".into(),
            rows: vec![LabAccount {
                label: "fixture-account".into(),
                facts: vec![
                    ("Login".into(), "Signed out".into()),
                    ("5-hour quota".into(), "Unknown; never read".into()),
                    (
                        "Weekly quota".into(),
                        "25% left; retained observation".into(),
                    ),
                    ("Next step".into(), "Browser sign-in needed".into()),
                ],
            }],
        },
        sessions: vec![LabSession {
            local_id: 7,
            title: "fixture Claude session".into(),
            facts: vec![
                ("Recovery".into(), "Waiting".into()),
                ("Blocker".into(), "No eligible alternate account".into()),
                ("Next event".into(), "2026-11-01T00:00:00Z".into()),
            ]
            .into_iter()
            .chain(claim_rows)
            .collect(),
        }],
        collecting: false,
    }
}

fn controller(
    lab: LabState,
) -> (
    NativeRuntime,
    crate::native_app::AppInstanceId,
    crate::native_app::ViewId,
) {
    let app = SettingsApp::new(UpdateState::from_status(1, "0.1.0", None, false));
    let mut runtime = NativeRuntime::new();
    let instance = runtime
        .insert_instance(NativeApp::Settings(Box::new(app)))
        .unwrap();
    let mut store = crate::tab_model::ViewStore::default();
    let view = store.insert_native(instance).unwrap();
    runtime
        .attach_view(
            view,
            instance,
            AppViewState::Settings(Box::new(SettingsViewState::new(&Config::default()))),
        )
        .unwrap();
    publish(&mut runtime, instance, view, lab, 1);
    let outcome = runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: route_action(SettingsRoute::Lab),
                value: None,
            }),
        )
        .unwrap();
    assert!(outcome.effects.iter().any(|effect| matches!(
        effect,
        AppEffect::Harness(HarnessRequest::LabAccountsRefresh)
    )));
    (runtime, instance, view)
}

fn publish(
    runtime: &mut NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    lab: LabState,
    revision: u64,
) {
    let mut harness = HarnessState::unobserved();
    harness.lab = lab;
    assert!(runtime.replace_settings_harness(harness, revision));
    runtime
        .dispatch(instance, view, AppEvent::HarnessChanged { revision })
        .unwrap();
}

fn pages(
    runtime: &mut NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    width: f32,
    height: f32,
) -> Vec<CompiledUi> {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let cx = ViewCx {
        viewport: LogicalRect::new(0.0, 0.0, width, height),
        animation_phase_ms: 720,
        motion: crate::native_app::ViewMotionCx::default(),
        terminal_font_px: 12.0,
        terminal_scale: 1.0,
        semantic_font: None,
        document: None,
    };
    let mut out = Vec::new();
    for page in 0..=512 {
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            panic!("Settings view")
        };
        state.scroll_page_to(page);
        state.page_reveal = None;
        out.push(
            runtime
                .render(instance, view, &cx)
                .expect("Settings renders")
                .compile(cx.viewport)
                .expect("Settings compiles"),
        );
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view")
        };
        let limit = state
            .result_page_limit
            .get()
            .expect("the page records its bound");
        if page >= limit {
            return out;
        }
    }
    panic!("fixture pagination did not finish");
}

fn label<'a>(pages: &'a [CompiledUi], key: &str) -> &'a str {
    &pages
        .iter()
        .find_map(|page| page.semantic(&UiKey::new(key)))
        .unwrap_or_else(|| panic!("missing {key}"))
        .label
}

#[test]
fn lab_route_shows_independent_cache_quota_and_recovery_observations() {
    let (mut runtime, instance, view) = controller(fixture());
    assert_eq!(SettingsRoute::from_path("/lab"), Some(SettingsRoute::Lab));
    assert!(
        navigation_groups()
            .iter()
            .any(|(name, routes)| *name == "System" && routes == &[SettingsRoute::Lab])
    );
    let drawn = pages(&mut runtime, instance, view, 1200.0, 820.0);
    assert!(label(&drawn, "lab/network/row/network").contains("1 reachable"));
    let observer = UiKey::new("lab/network/row/observer");
    assert_eq!(
        drawn[0]
            .semantics
            .iter()
            .filter(|node| node.key == observer)
            .count(),
        1,
        "the native network projection owns the observer explanation once"
    );
    assert!(label(&drawn, "lab/network/row/observer").contains("Discovery observations retained"));
    assert!(
        label(&drawn, "lab/service/secrets/row/currency")
            .contains("remote currency not established")
    );
    assert!(label(&drawn, "lab/service/accounts/row/commons-account-catalog").contains("Cached"));
    assert!(label(&drawn, "lab/account/0/row/5-hour-quota").contains("Unknown; never read"));
    assert!(label(&drawn, "lab/account/0/row/weekly-quota").contains("25% left"));
    assert!(label(&drawn, "lab/session/7/row/blocker").contains("No eligible alternate account"));
    assert!(label(&drawn, "lab/session/7/row/next-event").contains("2026-11-01T00:00:00Z"));
    assert!(label(&drawn, "lab/peer/7/row/machine").contains("fixture-peer"));

    let refresh_button = drawn
        .iter()
        .find_map(|page| page.semantic(&UiKey::new(LAB_ACCOUNTS_REFRESH)))
        .expect("the local Claude inventory offers its refresh");
    assert_eq!(refresh_button.parent, Some(UiKey::new("lab/inventory")));
    let refresh = runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: refresh_button
                    .action
                    .clone()
                    .expect("refresh is actionable"),
                value: None,
            }),
        )
        .unwrap();
    assert!(refresh.effects.iter().any(|effect| matches!(
        effect,
        AppEffect::Harness(HarnessRequest::LabAccountsRefresh)
    )));
    let show = runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("harness/session/7/show"),
                value: None,
            }),
        )
        .unwrap();
    assert!(show.effects.iter().any(|effect| matches!(
        effect,
        AppEffect::Harness(HarnessRequest::Show { session: 7 })
    )));
}

#[test]
fn historical_peer_inventory_is_readable_at_both_widths_without_claiming_current_sync() {
    for (width, height) in [(1200.0, 820.0), (600.0, 620.0)] {
        let mut lab = fixture();
        let network = Arc::make_mut(&mut lab.network);
        network.network.accounts_exchange =
            Some(aterm_types::network_state::AccountsExchangeObservation {
                scope_digest: [3; 32],
                local_actor: [4; 48],
                control_observation: [5; 64],
                inventory_count: 2,
                inventory_fingerprint: [6; 32],
                peer: network.network.peers[0].identity.clone(),
                sampled_at_ms: 90_000,
            });
        network.network.visibility = aterm_types::network_state::Visibility::Unavailable;
        let (mut runtime, instance, view) = controller(lab);
        let drawn = pages(&mut runtime, instance, view, width, height);
        assert!(
            label(&drawn, "lab/service/accounts/row/peer-inventory")
                .contains("2 retained operation IDs reported matching by fixture-peer")
        );
        assert!(
            label(&drawn, "lab/service/accounts/row/peer-inventory-sampled").contains("1970-01-01")
        );
        assert!(
            label(&drawn, "lab/service/accounts/row/inventory-scope")
                .contains("Historical account-proposal IDs only")
        );
        assert!(
            label(&drawn, "lab/service/accounts/row/currency")
                .contains("remote currency not established")
        );
        assert!(
            label(&drawn, "lab/service/accounts/row/commons-account-catalog").contains("Cached")
        );
        assert!(!drawn.iter().any(|page| {
            page.semantic(&UiKey::new("lab/service/secrets/row/peer-inventory"))
                .is_some()
        }));
    }
}

#[test]
fn retained_peer_repairs_survive_expiry_and_remain_readable_at_both_widths() {
    use aterm_types::network_state::{RepairObservation, RepairOutcome, RepairStage};

    for (width, height) in [(1200.0, 820.0), (600.0, 620.0)] {
        for (outcome, word) in [
            (RepairOutcome::Failed, "Failed"),
            (RepairOutcome::Exchanged, "One application exchange"),
        ] {
            let mut lab = fixture();
            Arc::make_mut(&mut lab.network).network.peers[0].last_repair =
                Some(RepairObservation {
                    completed_at_ms: 90_000,
                    outbound: true,
                    stage: Some(RepairStage::Application),
                    outcome,
                    table: Some([3; 32]),
                    sent: Some(7),
                    received: None,
                    deferred: Some(2),
                });
            lab.network = crate::network_deck::fresh_snapshot(lab.network, Some(200_000));
            let (mut runtime, instance, view) = controller(lab);
            let drawn = pages(&mut runtime, instance, view, width, height);
            assert!(label(&drawn, "lab/peer/7/row/last-repair").contains(word));
            assert!(label(&drawn, "lab/peer/7/row/repair-time").contains("00:01:30Z"));
            let counts = label(&drawn, "lab/peer/7/row/record-receipts");
            assert!(counts.contains('7') && counts.contains('2') && counts.contains("unknown"));
            assert!(label(&drawn, "lab/peer/7/row/repair-table").contains(&"03".repeat(32)));
            assert!(label(&drawn, "lab/peer/7/row/repair-scope").contains("Historical"));
            assert!(label(&drawn, "lab/peer/7/row/repair-scope").contains("not established"));
        }
    }
}

#[test]
fn an_older_observer_does_not_invent_a_repair_or_zero_receipts() {
    let (mut runtime, instance, view) = controller(fixture());
    let drawn = pages(&mut runtime, instance, view, 600.0, 620.0);
    assert!(label(&drawn, "lab/peer/7/row/last-repair").contains("Not reported"));
    for row in ["repair-time", "record-receipts", "repair-table"] {
        let key = UiKey::new(format!("lab/peer/7/row/{row}"));
        assert!(drawn.iter().all(|page| page.semantic(&key).is_none()));
    }
}

#[test]
fn a_vault_network_receipt_remains_historical_in_wide_and_compact_lab_pages() {
    use aterm_types::network_state::{VaultNetworkMaterialization, VaultNetworkSource};
    for (width, height) in [(1200.0, 820.0), (600.0, 620.0)] {
        let mut lab = fixture();
        Arc::make_mut(&mut lab.network)
            .services
            .secrets
            .network_materialization = Some(VaultNetworkMaterialization {
            source: VaultNetworkSource::ConfiguredUrl,
            sequence: 17,
            sampled_at_ms: 80_000,
        });
        let (mut runtime, instance, view) = controller(lab);
        let drawn = pages(&mut runtime, instance, view, width, height);
        assert!(
            label(&drawn, "lab/service/secrets/row/network-sync")
                .contains("Signed vault version 17 received via configured route")
        );
        assert!(
            label(&drawn, "lab/service/secrets/row/network-fetch-started").contains("1970-01-01")
        );
        assert!(
            label(&drawn, "lab/service/secrets/row/network-sync-scope")
                .contains("Historical receipt")
        );
        assert!(
            label(&drawn, "lab/service/secrets/row/currency")
                .contains("remote currency not established")
        );
        assert!(!drawn.iter().any(|page| {
            page.semantic(&UiKey::new("lab/service/accounts/row/network-sync"))
                .is_some()
        }));
    }
}

#[test]
fn a_retained_204_is_separate_from_access_in_wide_and_compact_lab_pages() {
    use aterm_types::network_state::{VaultHttp204Response, VaultNetworkSource};
    for (width, height) in [(1200.0, 820.0), (600.0, 620.0)] {
        let mut lab = fixture();
        let service = &mut Arc::make_mut(&mut lab.network).services.secrets;
        service.availability = ServiceAvailability::Unavailable;
        service.detail = "Vault admission has not completed".into();
        service.http204_response = Some(VaultHttp204Response {
            source: VaultNetworkSource::ConfiguredUrl,
            responded_at_ms: 80_000,
        });
        let (mut runtime, instance, view) = controller(lab);
        let drawn = pages(&mut runtime, instance, view, width, height);
        assert!(
            label(&drawn, "lab/service/secrets/row/retained-http-204")
                .contains("Configured route answered HTTP 204")
        );
        assert!(label(&drawn, "lab/service/secrets/row/response-received").contains("1970-01-01"));
        assert!(
            label(&drawn, "lab/service/secrets/row/response-scope")
                .contains("Historical unsigned HTTP status only")
        );
        assert!(label(&drawn, "lab/service/secrets/row/secrets").contains("Unavailable"));
        assert!(
            label(&drawn, "lab/service/secrets/row/network-sync")
                .contains("No verified network receipt")
        );
        assert!(!drawn.iter().any(|page| {
            page.semantic(&UiKey::new("lab/service/accounts/row/retained-http-204"))
                .is_some()
        }));
        for page in &drawn {
            let audit = page.paint_audit_lines();
            let header = &audit[0];
            assert!(
                header.contains("overflow=0 ")
                    && header.contains("clipped=0 ")
                    && header.contains("overlaps=0 "),
                "{width}x{height}: {header}"
            );
        }
    }
}

#[test]
fn a_failed_network_refresh_is_visible_beside_usable_cache_in_both_lab_widths() {
    use aterm_types::network_state::{VaultNetworkRefresh, VaultNetworkRefreshOutcome};
    for (width, height) in [(1200.0, 820.0), (600.0, 620.0)] {
        for outcome in [
            VaultNetworkRefreshOutcome::Failed,
            VaultNetworkRefreshOutcome::Succeeded,
        ] {
            let mut lab = fixture();
            let service = &mut Arc::make_mut(&mut lab.network).services.secrets;
            service.refresh_failed = outcome == VaultNetworkRefreshOutcome::Failed;
            service.network_refresh = Some(VaultNetworkRefresh {
                completed_at_ms: 90_000,
                outcome,
            });
            let (mut runtime, instance, view) = controller(lab);
            let drawn = pages(&mut runtime, instance, view, width, height);
            let expected = if outcome == VaultNetworkRefreshOutcome::Failed {
                "Failed"
            } else {
                "Completed"
            };
            assert!(
                label(&drawn, "lab/service/secrets/row/recorded-network-refresh")
                    .ends_with(expected)
            );
            assert!(
                label(&drawn, "lab/service/secrets/row/refresh-completed").contains("1970-01-01")
            );
            assert!(label(&drawn, "lab/service/secrets/row/secrets").contains("Cached"));
            assert!(!drawn.iter().any(|page| {
                page.semantic(&UiKey::new(
                    "lab/service/accounts/row/recorded-network-refresh",
                ))
                .is_some()
            }));
            for page in &drawn {
                let audit = page.paint_audit_lines();
                let header = &audit[0];
                assert!(
                    header.contains("overflow=0 ")
                        && header.contains("clipped=0 ")
                        && header.contains("overlaps=0 "),
                    "{header}"
                );
            }
        }
    }
}

#[test]
fn a_lab_publication_replaces_unknown_facts_and_busy_refresh_never_starts_again() {
    let unknown = LabState {
        network: Arc::new(NetworkStateSnapshot::default()),
        accounts: LabAccounts::default(),
        sessions: Vec::new(),
        collecting: true,
    };
    let (mut runtime, instance, view) = controller(unknown);
    let drawn = pages(&mut runtime, instance, view, 1200.0, 820.0);
    assert!(label(&drawn, "lab/service/secrets/row/secrets").contains("Unknown"));
    let refresh_button = drawn
        .iter()
        .find_map(|page| page.semantic(&UiKey::new(LAB_ACCOUNTS_REFRESH)))
        .expect("the account reader's progress stays in the Claude inventory");
    assert!(!refresh_button.state.as_ref().expect("button state").enabled);
    let busy = runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new(LAB_ACCOUNTS_REFRESH),
                value: None,
            }),
        )
        .unwrap();
    assert!(!busy.effects.iter().any(|effect| matches!(
        effect,
        AppEffect::Harness(HarnessRequest::LabAccountsRefresh)
    )));

    publish(&mut runtime, instance, view, fixture(), 2);
    let drawn = pages(&mut runtime, instance, view, 1200.0, 820.0);
    assert!(label(&drawn, "lab/service/secrets/row/secrets").contains("Cached"));
    assert!(label(&drawn, "lab/account/0/row/login").contains("Signed out"));
    assert!(label(&drawn, "lab/account/0/row/next-step").contains("Browser sign-in needed"));
    let mut changed = fixture();
    changed.sessions.clear();
    publish(&mut runtime, instance, view, changed, 3);
    let show = runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("harness/session/7/show"),
                value: None,
            }),
        )
        .unwrap();
    assert!(
        !show
            .effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::Harness(HarnessRequest::Show { .. })))
    );
}

#[test]
fn lab_facts_remain_whole_in_wide_compact_and_scaled_settings() {
    struct RestoreScale;
    impl Drop for RestoreScale {
        fn drop(&mut self) {
            crate::native_appearance::inject_text_scale(None);
        }
    }
    let _restore = RestoreScale;
    for scale in [1.0, 1.25, 2.0] {
        crate::native_appearance::inject_text_scale(Some(scale));
        let (mut runtime, instance, view) = controller(fixture());
        for (width, height) in [(1200.0, 820.0), (584.0, 700.0), (320.0, 568.0)] {
            let drawn = pages(&mut runtime, instance, view, width, height);
            for (page, compiled) in drawn.iter().enumerate() {
                compiled.validate_parity().unwrap_or_else(|error| {
                    panic!("{scale} {width}x{height} page {page}: {error:?}")
                });
                let failures: Vec<_> = compiled
                    .fit_failures()
                    .into_iter()
                    .filter(|failure| !failure.contains("key=\"settings/nav/"))
                    .collect();
                assert!(
                    failures.is_empty(),
                    "{scale} {width}x{height} page {page}: {failures:?}"
                );
            }
            assert!(label(&drawn, "lab/account/0/row/login").contains("Signed out"));
            assert!(label(&drawn, "lab/session/7/row/next-event").contains("2026-11-01T00:00:00Z"));
            assert!(
                label(&drawn, "lab/session/7/row/own-account-claim").contains("at the decision")
            );
            assert!(
                label(&drawn, "lab/session/7/row/claim-recorded-until")
                    .contains("1970-01-01T00:03:20Z")
            );
            assert!(label(&drawn, "lab/session/7/row/claim-timing").contains("not rechecked"));
            assert!(label(&drawn, "lab/session/7/row/switch-blocker").contains("pinned"));
            assert!(drawn.iter().any(|page| {
                page.semantic(&UiKey::new(LAB_ACCOUNTS_REFRESH))
                    .is_some_and(|button| !button.offscreen && button.action.is_some())
            }));
        }
    }
}
