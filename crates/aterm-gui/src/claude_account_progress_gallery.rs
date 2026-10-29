// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit synthetic account and network scenes rendered by the App capture route.

use std::sync::{Arc, OnceLock};

use aterm_agent::harness::account_progress::{AccountProgress, ProgressPhase};

use super::probe;

fn capture(app: &mut crate::App, wid: crate::WindowId) -> Vec<u8> {
    app.redraw_window(wid);
    crate::scripted_redraw_fixture::headless(app, |app| {
        let (reply, rx) = std::sync::mpsc::channel();
        app.render_image(crate::control::ImageReq {
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target: crate::control_auth::ConfinedImage::in_memory(),
            clean: false,
            session: None,
            want_bytes: true,
            want_metadata: false,
            frame_metadata: Arc::new(OnceLock::new()),
            cancel: crate::control::CaptureCancellation::new(),
            reply,
        });
        rx.recv_timeout(std::time::Duration::from_secs(60))
            .expect("image reply")
            .expect("fixture image")
            .value
            .2
            .expect("PNG bytes")
    })
}

/// Writes opt-in review images; it never opens a provider or reads account credentials.
#[test]
#[ignore = "writes App-rendered fixture PNGs: set ATERM_ACCOUNT_PROGRESS_GALLERY"]
fn account_progress_gallery() {
    let Some(dir) = std::env::var_os("ATERM_ACCOUNT_PROGRESS_GALLERY") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("gallery output directory");
    let mut manifest = String::from(
        "# Synthetic owner-bound account facts and Claude-style screen; no live account state.\nscene\tcolumns\trows\tfile\n",
    );
    for (name, cols, phase, eligible, wall, next_at, detail) in [
        (
            "activity-menu-no-next-short",
            60,
            ProgressPhase::Waiting,
            0,
            Some("weekly"),
            None,
            "fixture-current: no usable sign-in route",
        ),
        (
            "activity-menu",
            120,
            ProgressPhase::Waiting,
            0,
            Some("weekly"),
            Some(1_793_040_160),
            "fixture-current: no usable sign-in route",
        ),
        (
            "activity-menu-narrow",
            60,
            ProgressPhase::Waiting,
            0,
            Some("weekly"),
            Some(1_793_040_160),
            "fixture-current: no usable sign-in route",
        ),
        (
            "current-login-blocked",
            120,
            ProgressPhase::Waiting,
            0,
            None,
            None,
            "current: CLI sign-in needed",
        ),
        (
            "current-login-blocked-narrow",
            60,
            ProgressPhase::Waiting,
            0,
            None,
            Some(1_793_040_160),
            "current: CLI sign-in needed",
        ),
        (
            "current-login-blocked-tiny",
            40,
            ProgressPhase::Waiting,
            0,
            None,
            Some(1_793_040_160),
            "current: CLI sign-in needed",
        ),
        (
            "quota-login-retry",
            60,
            ProgressPhase::Waiting,
            0,
            Some("weekly"),
            Some(1_793_040_160),
            "current: sign-in needed",
        ),
        (
            "blocked",
            120,
            ProgressPhase::Waiting,
            0,
            Some("weekly"),
            None,
            "No usable sign-in route",
        ),
        (
            "blocked-narrow",
            60,
            ProgressPhase::Waiting,
            0,
            Some("weekly"),
            None,
            "No usable sign-in route",
        ),
        (
            "signing-in",
            120,
            ProgressPhase::SigningIn {
                label: "fixture-alternate".into(),
                route: Some("fixture route".into()),
            },
            0,
            Some("weekly"),
            None,
            "A sign-in attempt is active",
        ),
        (
            "transcript-wall",
            120,
            ProgressPhase::Watching,
            1,
            None,
            None,
            "No recovery action is in flight",
        ),
        (
            "watching",
            120,
            ProgressPhase::Watching,
            1,
            None,
            None,
            "No recovery action is in flight",
        ),
    ] {
        let mut progress = AccountProgress {
            observed_at: 1_793_040_000,
            account: Some("fixture-current".into()),
            known: Some(2),
            eligible: Some(eligible),
            phase,
            wall: wall.map(str::to_owned),
            details: vec![detail.into()],
            next_at,
        };
        if name.starts_with("activity-menu") {
            progress
                .details
                .push("Local sign-in activity: fixture-current".into());
            progress.details.push(format!(
                "Activity at {}",
                aterm_agent::harness::usage::rfc3339_utc(progress.observed_at - 30),
            ));
        }
        let menu_progress = progress.clone();
        let mut facts = probe::facts();
        facts.account = progress.account.clone();
        facts.account_progress = Some(progress);
        if name == "transcript-wall" {
            facts.usage.get_or_insert_with(Default::default).wall =
                Some(aterm_agent::harness::session_usage::Wall {
                    label: "7d",
                    resets: "tomorrow".into(),
                });
        }
        let live = probe::Live {
            status: if wall.is_some() {
                "You've hit your limit · synthetic fixture"
            } else {
                probe::DONE
            },
            ..probe::AT_REST
        };
        let (mut app, wid, session) = probe::glass_with(&live, facts);
        app.config.serious_mode = Some(true);
        app.config.cursor_trail = Some(false);
        app.config.cursor_trail_style = Some("off".into());
        app.recompute_sparkle();
        let rows = if name == "activity-menu-no-next-short" {
            14
        } else {
            probe::ROWS
        };
        if app.windows[&wid].cols != cols || app.windows[&wid].rows != rows {
            assert!(app.apply_term_resize(wid, rows, cols));
        }
        let mut screen = probe::screen_at(&live, rows, cols);
        screen[0] = format!("SYNTHETIC FIXTURE · {name} · no live account");
        probe::feed(&app, session, &screen);
        probe::refill(&mut app, wid);
        let _ = probe::pass(&mut app, wid);
        if name.starts_with("activity-menu") {
            let fact = crate::claude_lights::Fact::Account;
            let anchor = app.fact_anchor(wid, session, fact).expect("account anchor");
            assert!(
                app.proxy.is_none(),
                "the gallery must not start an account reader"
            );
            app.open_fact_menu(wid, session, fact, anchor, true);
            // Use the production menu projection with a synthetic roster, so
            // the gallery performs no account-file or provider reads.
            let roster = aterm_agent::harness::accounts::Roster {
                rows: ["fixture-current", "fixture-alternate"]
                    .map(|label| aterm_agent::harness::accounts::Row {
                        label: label.into(),
                        dir: Some(format!("/synthetic/{label}")),
                        ..Default::default()
                    })
                    .into(),
            };
            let (accounts, _) = crate::fact_menu::account_menu_rows_from(
                &roster,
                &Default::default(),
                &Default::default(),
                true,
                Some("fixture-current"),
                None,
                Default::default(),
                menu_progress.observed_at,
            );
            let facts = crate::fact_menu::FactMenuFacts {
                account_progress: Some(menu_progress),
                cli: Some(aterm_agent::harness::accounts::Cli::Claude),
                auto_rotate: true,
                accounts,
                ..Default::default()
            };
            let menu = app.autopilot_menu.as_mut().unwrap();
            menu.rows = crate::fact_menu::rows(fact, &facts);
            menu.highlight = None;
        }
        let before = (app.windows[&wid].rows, app.windows[&wid].cols);
        let png = capture(&mut app, wid);
        assert_eq!((app.windows[&wid].rows, app.windows[&wid].cols), before);
        let file = format!("fixture-{name}.png");
        std::fs::write(dir.join(&file), png).expect("fixture PNG");
        manifest.push_str(&format!("{name}\t{cols}\t{rows}\t{file}\n"));
    }
    std::fs::write(dir.join("manifest.tsv"), manifest).expect("fixture manifest");
}

#[test]
#[ignore = "writes App-rendered PNGs: set ATERM_NETWORK_HISTORY_GALLERY and ATERM_DEV_STATS_DECK_HEADLESS=1"]
fn network_history_gallery() {
    use aterm_types::network_state::{
        HistoryState, MachineIdentity, NetworkStateSnapshot, PeerObservation, Reachability,
        ServiceAvailability, ServiceState, Visibility,
    };
    let Some(dir) = std::env::var_os("ATERM_NETWORK_HISTORY_GALLERY") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("gallery output directory");
    assert!(
        crate::stats_deck::deck_on_headless(),
        "enable resource-panel capture"
    );
    for (name, cols, limited, details) in [
        ("baseline", 120, false, false),
        ("peer-current", 120, false, true),
        ("peer-expired", 120, false, true),
        ("peer-unknown", 120, false, true),
        ("waiting", 120, true, true),
        ("waiting-narrow", 60, true, true),
        ("waiting-short", 60, true, true),
        ("waiting-short-wide", 120, true, true),
        ("waiting-long-short", 60, true, true),
        ("limited", 120, true, false),
        ("limited-details", 120, true, true),
        ("limited-narrow", 60, true, false),
        ("services", 120, false, true),
        ("services-narrow", 60, false, true),
        ("services-failed", 120, false, true),
        ("services-failed-narrow", 60, false, true),
        ("services-failed-short", 60, false, true),
        ("services-catalog", 120, false, true),
        ("services-catalog-short", 60, false, true),
        ("services-short", 60, false, true),
        ("services-short-wide", 120, false, true),
    ] {
        let (mut app, wid, session) = probe::probe_glass();
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_bars = Some("deck".into());
        app.config.serious_mode = Some(true);
        app.config.cursor_trail = Some(false);
        app.config.cursor_trail_style = Some("off".into());
        app.recompute_sparkle();
        let rows = if name.contains("short") { 14 } else { 26 };
        assert!(app.apply_term_resize(wid, rows, cols));
        // A headless test window has no native attach to reserve chrome space.
        let ws = &app.windows[&wid];
        let head = ws.metrics.head.max(crate::stats_deck::reserved_deck_px(
            crate::stats_deck::StatsBars::Deck,
            ws.deck_form,
            ws.scale,
            app.win_cell_size(wid).1,
        ));
        app.windows.get_mut(&wid).unwrap().metrics.head = head;
        app.backend.set_head(head);
        assert!(app.deck_frame(wid).is_some());
        let identity = |id, label: &str| MachineIdentity {
            machine_id: [id; 16],
            incarnation: [1; 16],
            label: Some(label.into()),
        };
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.visibility = Visibility::Active;
        snapshot.network.history = HistoryState::Ready;
        snapshot.network.observer = Some(identity(1, "fixture-local"));
        snapshot.network.observed_at_ms = 1_793_040_000_000;
        snapshot.network.retention_limited = limited;
        snapshot.network.peers.push(PeerObservation {
            slot: 1,
            identity: identity(2, "fixture-peer"),
            first_seen_ms: 1_793_030_000_000,
            last_seen_ms: 1_793_040_000_000,
            last_checked_ms: Some(1_793_040_000_000),
            reachability: Reachability::Reachable,
            instances: None,
            samples: Vec::new(),
        });
        if name.starts_with("services") || name.starts_with("waiting") {
            snapshot.services.accounts = ServiceState {
                availability: ServiceAvailability::Unknown,
                observed_at_ms: snapshot.network.observed_at_ms,
                detail: "Shared catalog metadata could not be read".into(),
                refresh_failed: true,
                ..ServiceState::default()
            };
            snapshot.services.secrets = ServiceState {
                availability: ServiceAvailability::Cached,
                observed_at_ms: snapshot.network.observed_at_ms,
                revision: Some("vault 71".into()),
                detail: "12 authorized manifest entries; file delivery, login readiness and remote currency are not established by this read".into(),
                ..ServiceState::default()
            };
            if name.contains("catalog") {
                snapshot.services.accounts.availability = ServiceAvailability::Cached;
                snapshot.services.accounts.refresh_failed = false;
                snapshot.services.accounts.detail = "2 account proposals retained; catalog access does not prove login readiness or remote currency".into();
            }
        }
        if name.starts_with("waiting") {
            snapshot.network.visibility = Visibility::Waiting;
            snapshot.network.detail = "Another local process owns Commons repair".into();
            if name.contains("long") {
                snapshot.network.detail.push_str("; ");
                snapshot
                    .network
                    .detail
                    .push_str(&"retained diagnostic context ".repeat(40));
            }
            snapshot.services.accounts.availability = ServiceAvailability::Unavailable;
            snapshot.services.accounts.refresh_failed = false;
            snapshot.services.accounts.detail =
                "Account proposals not retained locally; harness rosters are separate".into();
            snapshot.services.secrets.last_success_ms = Some(snapshot.network.observed_at_ms);
        }
        app.network_source = None;
        app.network_snapshot = Arc::new(snapshot);
        if name.starts_with("peer-") {
            let mut snapshot = app.network_snapshot.as_ref().clone();
            let contact_at = snapshot.network.observed_at_ms;
            snapshot.network.observed_at_ms += 8_000;
            if name == "peer-unknown" {
                snapshot.network.peers[0].last_checked_ms = None;
            }
            let age = if name == "peer-current" {
                15_000
            } else {
                15_001
            };
            app.network_snapshot =
                crate::network_deck::fresh_snapshot(Arc::new(snapshot), Some(contact_at + age));
        }
        let mut screen = probe::screen_at(&probe::AT_REST, rows, cols);
        screen[0] = format!("SYNTHETIC NETWORK HISTORY · {name}");
        probe::feed(&app, session, &screen);
        probe::refill(&mut app, wid);
        let _ = probe::pass(&mut app, wid);
        if details {
            assert!(app.open_network_lens());
            if name.starts_with("peer-") {
                app.windows.get_mut(&wid).unwrap().stats_lens.network_target =
                    crate::network_deck::Target::Peer(1);
            }
            if name.starts_with("services") {
                app.windows.get_mut(&wid).unwrap().stats_lens.network_target =
                    if name.contains("failed") || name.contains("catalog") {
                        crate::network_deck::Target::Accounts
                    } else {
                        crate::network_deck::Target::Secrets
                    };
            }
        }
        let before = (app.windows[&wid].rows, app.windows[&wid].cols);
        let png = capture(&mut app, wid);
        assert!(app.windows[&wid].deck_card.is_some());
        assert_eq!((app.windows[&wid].rows, app.windows[&wid].cols), before);
        std::fs::write(dir.join(format!("{name}.png")), png).expect("fixture PNG");
    }
}
