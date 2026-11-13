// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit synthetic account and network scenes rendered by the App capture route.

use std::sync::{Arc, OnceLock};

use aterm_agent::harness::account_progress::{AccountProgress, ProgressPhase};

use super::probe;

fn row_is_visible(ui: &crate::native_ui::CompiledUi, key: &crate::native_ui::UiKey) -> bool {
    ui.semantic(key)
        .is_some_and(|node| !node.offscreen && node.rect.width > 0.0 && node.rect.height > 0.0)
}

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

/// Renders review images without opening a provider or reading account credentials.
#[test]
fn account_progress_gallery() {
    let (dir, _gallery) =
        crate::scripted_redraw_fixture::gallery_directory("ATERM_ACCOUNT_PROGRESS_GALLERY");
    let shared_row = aterm_agent::harness::accounts::Row {
        label: "fixture-alternate".into(),
        email: Some("fixture@example.test".into()),
        ..Default::default()
    };
    let shared_diagnostic =
        aterm_agent::harness::accounts_sync::LoginDiagnostic::from_exit_for_test(
            &shared_row,
            1_793_039_950,
            1,
            Some(aterm_agent::harness::open_url::Met::new(
                aterm_agent::harness::open_url::MetWord::BrowserControl,
                None,
            )),
        );
    let long_label = "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzlabelendingx";
    let mut long_row = shared_row.clone();
    long_row.label = long_label.into();
    let long_diagnostic = aterm_agent::harness::accounts_sync::LoginDiagnostic::from_exit_for_test(
        &long_row,
        1_793_039_950,
        1,
        Some(aterm_agent::harness::open_url::Met::new(
            aterm_agent::harness::open_url::MetWord::BrowserControl,
            None,
        )),
    );
    let mut manifest = String::from(
        "# Synthetic owner-bound account facts and Claude-style screen; no live account state.\nscene\tcolumns\trows\tfile\n",
    );
    for (name, cols, phase, eligible, wall, next_at, detail) in [
        (
            "activity-menu-execution-expired-off",
            120,
            ProgressPhase::Unknown,
            0,
            None,
            None,
            aterm_agent::harness::account_progress::EXECUTION_REFRESH_FINDING,
        ),
        (
            "activity-menu-execution-expired-off-narrow",
            60,
            ProgressPhase::Unknown,
            0,
            None,
            None,
            aterm_agent::harness::account_progress::EXECUTION_REFRESH_FINDING,
        ),
        (
            "activity-menu-execution-expired-probing-off",
            120,
            ProgressPhase::Probing,
            0,
            None,
            None,
            aterm_agent::harness::account_progress::EXECUTION_REFRESH_FINDING,
        ),
        (
            "activity-menu-execution-expired-probing-off-narrow",
            60,
            ProgressPhase::Probing,
            0,
            None,
            None,
            aterm_agent::harness::account_progress::EXECUTION_REFRESH_FINDING,
        ),
        (
            "activity-menu-execution-prepared-off",
            120,
            ProgressPhase::Waiting,
            0,
            None,
            Some(1_793_040_005),
            aterm_agent::harness::account_progress::EXECUTION_PREPARED_FINDING,
        ),
        (
            "activity-menu-execution-prepared-off-narrow",
            60,
            ProgressPhase::Waiting,
            0,
            None,
            Some(1_793_040_005),
            aterm_agent::harness::account_progress::EXECUTION_PREPARED_FINDING,
        ),
        (
            "activity-menu-observer-busy-prepared",
            120,
            ProgressPhase::Waiting,
            0,
            None,
            Some(1_793_040_005),
            aterm_agent::harness::account_progress::EXECUTION_PREPARED_FINDING,
        ),
        (
            "activity-menu-observer-busy-prepared-narrow",
            60,
            ProgressPhase::Waiting,
            0,
            None,
            Some(1_793_040_005),
            aterm_agent::harness::account_progress::EXECUTION_PREPARED_FINDING,
        ),
        (
            "activity-menu-autopilot-off",
            120,
            ProgressPhase::Paused,
            0,
            None,
            None,
            aterm_agent::harness::account_progress::SESSION_AUTOPILOT_OFF_FINDING,
        ),
        (
            "activity-menu-autopilot-off-narrow",
            60,
            ProgressPhase::Paused,
            0,
            None,
            None,
            aterm_agent::harness::account_progress::SESSION_AUTOPILOT_OFF_FINDING,
        ),
        (
            "activity-menu-execution-roster-unread",
            120,
            ProgressPhase::Waiting,
            0,
            None,
            Some(1_793_040_005),
            aterm_agent::harness::account_progress::EXECUTION_ROSTER_UNREAD_FINDING,
        ),
        (
            "activity-menu-execution-roster-unread-narrow",
            60,
            ProgressPhase::Waiting,
            0,
            None,
            Some(1_793_040_005),
            aterm_agent::harness::account_progress::EXECUTION_ROSTER_UNREAD_FINDING,
        ),
        (
            "activity-menu-execution-probing-off",
            120,
            ProgressPhase::Probing,
            0,
            None,
            Some(1_793_040_005),
            aterm_agent::harness::account_progress::EXECUTION_PREPARED_FINDING,
        ),
        (
            "activity-menu-execution-probing-off-narrow",
            60,
            ProgressPhase::Probing,
            0,
            None,
            Some(1_793_040_005),
            aterm_agent::harness::account_progress::EXECUTION_PREPARED_FINDING,
        ),
        (
            "activity-menu-recheck-unavailable",
            120,
            ProgressPhase::Unknown,
            0,
            Some("weekly"),
            None,
            "The account roster could not be read",
        ),
        (
            "activity-menu-recheck-unavailable-narrow",
            60,
            ProgressPhase::Unknown,
            0,
            Some("weekly"),
            None,
            "The account roster could not be read",
        ),
        (
            "activity-menu-bindings-changed",
            120,
            ProgressPhase::Unknown,
            0,
            Some("weekly"),
            None,
            aterm_agent::harness::account_progress::BINDINGS_CHANGED_FINDING,
        ),
        (
            "activity-menu-bindings-changed-narrow",
            60,
            ProgressPhase::Unknown,
            0,
            Some("weekly"),
            None,
            aterm_agent::harness::account_progress::BINDINGS_CHANGED_FINDING,
        ),
        (
            "activity-menu-identity-mismatch",
            120,
            ProgressPhase::Waiting,
            0,
            Some("weekly"),
            None,
            aterm_agent::harness::account_progress::IDENTITY_MISMATCH_FINDING,
        ),
        (
            "activity-menu-identity-mismatch-narrow",
            60,
            ProgressPhase::Waiting,
            0,
            Some("weekly"),
            None,
            aterm_agent::harness::account_progress::IDENTITY_MISMATCH_FINDING,
        ),
        (
            "activity-menu-shared-failure",
            120,
            ProgressPhase::Waiting,
            0,
            Some("weekly"),
            Some(1_793_040_160),
            "No eligible alternate account",
        ),
        (
            "activity-menu-shared-failure-narrow",
            60,
            ProgressPhase::Waiting,
            0,
            Some("weekly"),
            Some(1_793_040_160),
            "No eligible alternate account",
        ),
        (
            "activity-menu-shared-failure-long-label",
            120,
            ProgressPhase::Waiting,
            0,
            Some("weekly"),
            Some(1_793_040_160),
            "No eligible alternate account",
        ),
        (
            "activity-menu-shared-failure-long-label-narrow",
            60,
            ProgressPhase::Waiting,
            0,
            Some("weekly"),
            Some(1_793_040_160),
            "No eligible alternate account",
        ),
        (
            "activity-menu-shared-failure-newest-short",
            40,
            ProgressPhase::Waiting,
            0,
            Some("weekly"),
            Some(1_793_040_160),
            "No eligible alternate account",
        ),
        (
            "activity-menu-shared-failure-newest-short-wide",
            60,
            ProgressPhase::Waiting,
            0,
            Some("weekly"),
            Some(1_793_040_160),
            "No eligible alternate account",
        ),
        (
            "lab-accounts-refresh",
            120,
            ProgressPhase::Unknown,
            0,
            None,
            None,
            aterm_agent::harness::footer::UNOBSERVED_RECOVERY_FINDING,
        ),
        (
            "lab-accounts-refresh-narrow",
            60,
            ProgressPhase::Unknown,
            0,
            None,
            None,
            aterm_agent::harness::footer::UNOBSERVED_RECOVERY_FINDING,
        ),
        (
            "lab-shared-failure",
            120,
            ProgressPhase::Waiting,
            0,
            Some("weekly"),
            Some(1_793_040_160),
            "No eligible alternate account",
        ),
        (
            "lab-shared-failure-narrow",
            60,
            ProgressPhase::Waiting,
            0,
            Some("weekly"),
            Some(1_793_040_160),
            "No eligible alternate account",
        ),
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
            "independent-pool-work",
            120,
            ProgressPhase::Waiting,
            1,
            Some("weekly"),
            Some(1_793_040_300),
            "Recorded account decision",
        ),
        (
            "independent-pool-work-narrow",
            60,
            ProgressPhase::Waiting,
            1,
            Some("weekly"),
            Some(1_793_040_300),
            "Recorded account decision",
        ),
        (
            "activity-menu-independent-pool-work",
            120,
            ProgressPhase::Waiting,
            1,
            Some("weekly"),
            Some(1_793_040_300),
            "Recorded account decision",
        ),
        (
            "activity-menu-independent-pool-work-narrow",
            60,
            ProgressPhase::Waiting,
            1,
            Some("weekly"),
            Some(1_793_040_300),
            "Recorded account decision",
        ),
        (
            "activity-menu-independent-execution-work",
            120,
            ProgressPhase::Waiting,
            0,
            None,
            Some(1_793_040_300),
            aterm_agent::harness::account_progress::EXECUTION_PREPARED_FINDING,
        ),
        (
            "activity-menu-independent-execution-work-narrow",
            60,
            ProgressPhase::Waiting,
            0,
            None,
            Some(1_793_040_300),
            aterm_agent::harness::account_progress::EXECUTION_PREPARED_FINDING,
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
            "unobserved-recovery",
            120,
            ProgressPhase::Unknown,
            0,
            None,
            None,
            aterm_agent::harness::footer::UNOBSERVED_RECOVERY_FINDING,
        ),
        (
            "unobserved-recovery-narrow",
            60,
            ProgressPhase::Unknown,
            0,
            None,
            None,
            aterm_agent::harness::footer::UNOBSERVED_RECOVERY_FINDING,
        ),
        (
            "activity-menu-unobserved-recovery",
            120,
            ProgressPhase::Unknown,
            0,
            None,
            None,
            aterm_agent::harness::footer::UNOBSERVED_RECOVERY_FINDING,
        ),
        (
            "activity-menu-unobserved-recovery-narrow",
            60,
            ProgressPhase::Unknown,
            0,
            None,
            None,
            aterm_agent::harness::footer::UNOBSERVED_RECOVERY_FINDING,
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
            forecasts_paused: false,
            observed_at: 1_793_040_000,
            account: Some("fixture-current".into()),
            known: Some(2),
            eligible: Some(eligible),
            phase,
            wall: wall.map(str::to_owned),
            details: vec![detail.into()],
            login_diagnostics: Vec::new(),
            independent_work: Vec::new(),
            next_at,
            own_claim: None,
            stay: None,
        };
        if name.contains("independent-") {
            progress
                .independent_work
                .push(aterm_agent::harness::account_progress::ActiveSignIn {
                    label: "operations-account-01".into(),
                    route: Some("mailbox".into()),
                    observed_at: progress.observed_at - 10,
                });
        }
        if name.contains("newest-short") {
            use aterm_agent::harness::{
                accounts_sync::LoginDiagnostic,
                open_url::{Met, MetWord},
            };
            let older = LoginDiagnostic::from_exit_for_test(
                &shared_row,
                1_793_039_900,
                1,
                Some(Met::new(MetWord::Stall, None)),
            );
            let mut current = shared_row.clone();
            current.label = "fixture-current".into();
            let newest = LoginDiagnostic::from_exit_for_test(
                &current,
                1_793_039_990,
                1,
                Some(Met::new(MetWord::BrowserControl, None)),
            );
            progress.login_diagnostics.extend([older, newest]);
        } else if name.contains("long-label") {
            progress.login_diagnostics.push(long_diagnostic.clone());
        } else if name.contains("shared-failure") {
            progress.login_diagnostics.push(shared_diagnostic.clone());
        }
        if name.contains("bindings-changed") {
            progress.eligible = None;
        }
        let recheck_unavailable = name.contains("recheck-unavailable");
        if recheck_unavailable {
            progress.account = None;
            progress.known = None;
            progress.eligible = None;
            progress.details.push(format!(
                "{}Selected alternate: fixture-alternate",
                aterm_agent::harness::account_progress::RECOVERY_HISTORY_PREFIX,
            ));
        }
        if name.contains("execution-") {
            progress.known = None;
            progress.eligible = None;
            if name.contains("execution-expired-") {
                progress.details.push(format!(
                    "{}{}",
                    aterm_agent::harness::account_progress::EXECUTION_HISTORY_PREFIX,
                    aterm_agent::harness::account_progress::EXECUTION_PREPARED_FINDING,
                ));
            }
            progress
                .details
                .push("Automatic account rotation is off".into());
            if name.contains("roster-unread") {
                progress.account = None;
            }
        }
        let observer_busy = name.contains("observer-busy");
        let autopilot_off = name.contains("autopilot-off");
        if autopilot_off {
            // Already-derived synthetic reader output, not a policy or actor invocation.
            progress.forecasts_paused = true;
            progress.eligible = None;
            progress.details.push(format!(
                "{}{}",
                aterm_agent::harness::account_progress::RECOVERY_HISTORY_PREFIX,
                "Waiting for the account-switch limit to reset",
            ));
        }
        if name.starts_with("activity-menu")
            && !name.contains("execution-")
            && !recheck_unavailable
            && !observer_busy
            && !autopilot_off
            && !name.contains("independent-")
        {
            progress
                .details
                .push("Local sign-in activity: fixture-current".into());
            progress.details.push(format!(
                "Activity at {}",
                aterm_agent::harness::usage::rfc3339_utc(progress.observed_at - 30),
            ));
        }
        let retained = (observer_busy || autopilot_off).then(|| progress.clone());
        let mut facts = probe::facts();
        facts.account = progress.account.clone();
        if facts.account.is_none() {
            // A recovery that names no account still runs under the pane's own login, which
            // line D's unit names (`this login`): recovery words are never the rule's.
            facts.subscription = Some(aterm_agent::harness::profile::SubscriptionDisplay {
                email: Some("fixture@example.test".into()),
                ..Default::default()
            });
        }
        facts.account_progress = Some(progress);
        facts.account_progress_busy = observer_busy;
        if name == "transcript-wall"
            || name.contains("unobserved-recovery")
            || observer_busy
            || autopilot_off
        {
            facts.usage.get_or_insert_with(Default::default).wall =
                Some(aterm_agent::harness::session_usage::Wall {
                    label: "7d",
                    resets: if name.contains("unobserved-recovery") {
                        "tomorrow at 3pm"
                    } else {
                        "tomorrow"
                    }
                    .into(),
                });
        }
        if name.contains("unobserved-recovery") {
            facts.account_progress = None;
        }
        let menu_progress = facts
            .account_progress_view()
            .map(std::borrow::Cow::into_owned);
        if name.contains("independent-") {
            let observed = menu_progress.as_ref().unwrap();
            assert_eq!(observed.phase, ProgressPhase::Waiting);
            assert_eq!(observed.account.as_deref(), Some("fixture-current"));
            assert_eq!(observed.next_at, Some(1_793_040_300));
            assert_eq!(observed.independent_work.len(), 1);
            let spoken = crate::claude_facts::fact_spoken_of(
                &facts,
                None,
                crate::claude_lights::Fact::Account,
                aterm_agent::harness::footer::FactMask::default(),
            )
            .0;
            assert!(spoken.contains("operations-account-01") && spoken.contains("mailbox"));
            let reply =
                crate::harness_recovery_verb::cmd("recovery", true, || Ok(menu_progress.clone()));
            assert!(
                reply
                    .split_whitespace()
                    .any(|field| field == "phase=waiting")
            );
            assert!(
                reply
                    .split_whitespace()
                    .any(|field| field == "independent_work_count=1")
            );
        }
        if observer_busy {
            let recorded = retained.as_ref().unwrap();
            use aterm_agent::harness::account_progress::{
                EXECUTION_PREPARED_FINDING, OBSERVER_BUSY_FINDING, RECOVERY_HISTORY_PREFIX,
            };
            let unavailable = menu_progress.as_ref().expect("busy recovery projection");
            assert_eq!(facts.account_progress.as_ref(), Some(recorded));
            assert_eq!(unavailable.phase, ProgressPhase::Unknown);
            assert_eq!(unavailable.eligible, None);
            assert_eq!(unavailable.next_at, None);
            assert_eq!(unavailable.observed_at, recorded.observed_at);
            assert_eq!(unavailable.account, recorded.account);
            assert_eq!(unavailable.known, recorded.known);
            assert_eq!(unavailable.own_claim, recorded.own_claim);
            assert_eq!(recorded.wall, None);
            assert!(unavailable.wall.is_some());
            assert_eq!(unavailable.details[0], OBSERVER_BUSY_FINDING);
            assert!(unavailable.details.contains(&format!(
                "{RECOVERY_HISTORY_PREFIX}{EXECUTION_PREPARED_FINDING}"
            )));
        }
        if autopilot_off {
            let recorded = retained.as_ref().unwrap();
            let paused = menu_progress.as_ref().expect("paused recovery projection");
            assert_eq!(paused.phase, ProgressPhase::Paused);
            assert_eq!(paused.eligible, None);
            assert_eq!(paused.next_at, None);
            assert_eq!(paused.observed_at, recorded.observed_at);
            assert_eq!(paused.account, recorded.account);
            assert_eq!(paused.known, recorded.known);
            assert_eq!(paused.details, recorded.details);
            assert_eq!(recorded.wall, None);
            assert!(paused.wall.is_some());
        }
        let live = probe::Live {
            status: if wall.is_some() || observer_busy || autopilot_off {
                "You've hit your limit · synthetic fixture"
            } else {
                probe::DONE
            },
            ..probe::AT_REST
        };
        let (mut app, wid, session) = probe::glass_with(&live, facts);
        if autopilot_off {
            app.pool
                .get(session)
                .unwrap()
                .ctx
                .meta
                .lock()
                .unwrap()
                .set_autopilot(
                    Some(aterm_agent::supervise::Autopilot::Off.word().into()),
                    None,
                );
        }
        app.config.serious_mode = Some(true);
        app.config.cursor_trail = Some(false);
        app.config.cursor_trail_style = Some("off".into());
        app.recompute_sparkle();
        let rows = if name == "activity-menu-no-next-short" || name.contains("newest-short") {
            14
        } else if name.starts_with("lab-") {
            80
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
        if let Some(recorded) = retained.as_ref() {
            let timeline = app.pool.get(session).unwrap().ctx.timeline.lock().unwrap();
            let footer = timeline.claude_footer().expect("published footer");
            assert_eq!(footer.account_progress_busy, observer_busy);
            assert_eq!(footer.account_progress.as_ref(), Some(recorded));
            let observed = crate::harness_recovery_verb::snapshot(&timeline);
            assert_eq!(
                observed, menu_progress,
                "the actual owner-bound recovery and the menu show the same projection"
            );
            if autopilot_off {
                let reply = crate::harness_recovery_verb::cmd("recovery", true, || Ok(observed));
                for field in [
                    "phase=paused",
                    "eligible=-",
                    "next_at=-",
                    "target=-",
                    "route=-",
                ] {
                    assert!(
                        reply.split_whitespace().any(|word| word == field),
                        "{reply}"
                    );
                }
            }
        }
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
            let alternate = if name.contains("long-label") {
                long_label
            } else {
                "fixture-alternate"
            };
            let roster = aterm_agent::harness::accounts::Roster {
                rows: ["fixture-current", alternate]
                    .map(|label| aterm_agent::harness::accounts::Row {
                        label: label.into(),
                        dir: Some(format!("/synthetic/{label}")),
                        ..Default::default()
                    })
                    .into(),
            };
            let mut accounts = crate::fact_menu::account_menu_rows_from(
                &roster,
                &Default::default(),
                &Default::default(),
                true,
                Some("fixture-current"),
                None,
                Default::default(),
                menu_progress
                    .as_ref()
                    .map_or(0, |progress| progress.observed_at),
            );
            if name.contains("unobserved-recovery") || recheck_unavailable {
                accounts.clear();
            }
            let facts = crate::fact_menu::FactMenuFacts {
                account_progress: menu_progress,
                title: if name.contains("newest-short") {
                    "Account · tab 1".into()
                } else {
                    String::new()
                },
                room: if name.contains("newest-short") {
                    crate::autopilot_menu::content_room(cols.into())
                } else {
                    0
                },
                cli: Some(aterm_agent::harness::accounts::Cli::Claude),
                accounts,
                accounts_refused: if recheck_unavailable {
                    Some(detail.into())
                } else {
                    name.contains("unobserved-recovery")
                        .then(|| "Account roster has not been observed".into())
                },
                ..Default::default()
            };
            let menu = app.autopilot_menu.as_mut().unwrap();
            menu.rows = crate::fact_menu::rows(fact, &facts);
            menu.highlight = None;
        } else if name.starts_with("lab-") {
            app.pool
                .get(session)
                .unwrap()
                .ctx
                .timeline
                .lock()
                .unwrap()
                .set_program(4242, Some("claude".into()));
            assert!(app.proxy.is_none(), "the scene starts no account reader");
            if name.starts_with("lab-shared-failure") {
                let snapshot = app.harness_state();
                let published = snapshot
                    .lab
                    .sessions
                    .iter()
                    .find(|entry| entry.local_id == session)
                    .expect("fixture session");
                assert!(published.facts.iter().any(|(key, value)| {
                    key == "Last local sign-in" && value.contains("headless browser")
                }));
            }
            assert!(
                app.open_settings_tab_in_window(wid, crate::native_settings::SettingsRoute::Lab)
            );
            let keys = if name.starts_with("lab-accounts-refresh") {
                ["lab/inventory/row/scope", "lab/accounts/refresh"]
                    .map(crate::native_ui::UiKey::new)
            } else {
                ["last-local-sign-in", "sign-in-finished"].map(|row| {
                    crate::native_ui::UiKey::new(format!("lab/session/{session}/row/{row}"))
                })
            };
            let mut visible = false;
            for _ in 0..512 {
                let compiled = app.compiled_native_ui(wid).unwrap();
                if keys.iter().all(|key| row_is_visible(&compiled, key)) {
                    visible = true;
                    break;
                }
                app.dispatch_native_event(wid, crate::native_app::AppEvent::ScrollLines(1))
                    .unwrap();
            }
            assert!(
                visible,
                "the finite Lab page exposes {name}'s account observations"
            );
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
fn lab_own_claim_gallery() {
    use aterm_agent::harness::{
        account_progress::enrich_inventory,
        accounts::{Logins, Me, Point, Roster, Row, State},
    };
    let (dir, _gallery) =
        crate::scripted_redraw_fixture::gallery_directory("ATERM_LAB_CLAIM_GALLERY");
    let now = i64::try_from(aterm_time::unix_secs().expect("fixture clock")).unwrap();
    let roster = Roster {
        rows: ["fixture-current", "fixture-selected"]
            .map(|label| Row {
                label: label.into(),
                dir: Some(format!("/synthetic/{label}")),
                ..Default::default()
            })
            .into(),
    };
    let enrolled = ["fixture-current".to_owned(), "fixture-selected".to_owned()].into();
    let me = Me {
        account: Some("fixture-current"),
        left: None,
        default_home: true,
        route: aterm_agent::harness::accounts::AuthRoute::Stored,
    };
    let mut manifest = String::from(
        "# Synthetic retained claim decisions in the actual App Lab page; no provider or credential reads.\nscene\tcolumns\trows\tfile\n",
    );
    for (name, observed_at, until, pinned) in [
        ("held-selected-pin", now - 60, now + 3600, true),
        ("expired", now - 3600, now - 60, false),
    ] {
        let mut state = State {
            machine: "0a0b".into(),
            active: Some("fixture-selected".into()),
            proven: true,
            pinned,
            ..Default::default()
        };
        state.note_lease("fixture-current", "fixture-peer", until, observed_at - 1);
        let mut progress = AccountProgress::unknown(observed_at);
        progress.account = Some("fixture-current".into());
        progress.phase = ProgressPhase::Waiting;
        enrich_inventory(
            &mut progress,
            &roster,
            &roster,
            &state,
            &Logins::default(),
            &enrolled,
            &me,
            Some(&Point::TurnEnd),
        );
        assert_eq!(progress.own_claim.unwrap().pinned, pinned);
        let mut facts = probe::facts();
        facts.account_progress = Some(progress);
        let (mut app, wid, session) = probe::glass_with(&probe::AT_REST, facts);
        {
            let entry = app.pool.get(session).unwrap();
            entry
                .ctx
                .timeline
                .lock()
                .unwrap()
                .set_program(4242, Some("claude".into()));
        }
        assert!(
            app.proxy.is_none(),
            "the scene cannot start account collection"
        );
        let snapshot = app.harness_state();
        let recovery = snapshot
            .lab
            .sessions
            .iter()
            .find(|entry| entry.local_id == session)
            .expect("the private App publishes the synthetic session's recovery");
        for name in ["Own account claim", "Claim recorded until", "Claim timing"] {
            assert!(
                recovery.facts.iter().any(|(key, _)| key == name),
                "the source publication contains {name}"
            );
        }
        // The compact/scaled fixture checks rows across pages. This positive
        // image needs the adjacent claim rows on one page of the larger card.
        assert!(app.apply_term_resize(wid, 80, probe::COLS));
        app.config.serious_mode = Some(true);
        assert!(app.open_settings_tab_in_window(wid, crate::native_settings::SettingsRoute::Lab));
        let key =
            |row: &str| crate::native_ui::UiKey::new(format!("lab/session/{session}/row/{row}"));
        let mut visible = None;
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..512 {
            let compiled = app.compiled_native_ui(wid).unwrap();
            for row in [
                "own-account-claim",
                "claim-recorded-until",
                "claim-timing",
                "switch-blocker",
            ] {
                if row_is_visible(&compiled, &key(row)) {
                    seen.insert(row);
                }
            }
            if ["own-account-claim", "claim-recorded-until", "claim-timing"]
                .iter()
                .all(|row| row_is_visible(&compiled, &key(row)))
                && (!pinned || row_is_visible(&compiled, &key("switch-blocker")))
            {
                visible = Some(compiled);
                break;
            }
            app.dispatch_native_event(wid, crate::native_app::AppEvent::ScrollLines(1))
                .unwrap();
        }
        let compiled = visible.unwrap_or_else(|| panic!(
            "the finite Lab page walk reaches the claim rows together: viewport={:?} seen={seen:?}",
            app.native_ui_viewport(wid),
        ));
        let timing = &compiled.semantic(&key("claim-timing")).unwrap().label;
        assert_eq!(timing.contains("expired"), !pinned);
        let file = format!("lab-own-claim-{name}.png");
        std::fs::write(dir.join(&file), capture(&mut app, wid)).expect("Lab fixture PNG");
        manifest.push_str(&format!(
            "{name}\t{}\t{}\t{file}\n",
            app.windows[&wid].cols, app.windows[&wid].rows
        ));
    }
    std::fs::write(dir.join("manifest.tsv"), manifest).expect("Lab fixture manifest");
}

#[test]
fn network_history_gallery() {
    use aterm_types::network_state::{
        AuthenticatedEndpoint, HistoryState, MachineIdentity, NetworkStateSnapshot,
        PeerObservation, PresenceSample, Reachability, RepairObservation, RepairOutcome,
        RepairStage, ServiceAvailability, ServiceState, Visibility,
    };
    let (dir, _gallery) =
        crate::scripted_redraw_fixture::gallery_directory("ATERM_NETWORK_HISTORY_GALLERY");
    let _stats = crate::stats_deck::tests::attach_stats();
    for (name, mut cols, limited, details) in [
        ("baseline", 120, false, false),
        ("away-reader-busy", 120, false, false),
        ("away-reader-busy-narrow", 60, false, false),
        ("away-expired", 120, false, false),
        ("away-expired-narrow", 60, false, false),
        ("away-history-unavailable", 120, false, false),
        ("away-history-unavailable-narrow", 60, false, false),
        ("away-history-restoring", 120, false, false),
        ("away-history-restoring-narrow", 60, false, false),
        ("reader-busy", 120, false, false),
        ("reader-busy-narrow", 60, false, false),
        ("reader-busy-details", 120, false, true),
        ("reader-busy-details-narrow", 60, false, true),
        ("history-unavailable", 120, false, false),
        ("history-unavailable-narrow", 60, false, false),
        ("history-unavailable-relayed", 120, false, false),
        ("history-unavailable-relayed-narrow", 60, false, false),
        ("history-restoring", 120, false, false),
        ("history-restoring-narrow", 60, false, false),
        ("peer-current", 120, false, true),
        ("peer-expired", 120, false, true),
        ("peer-unknown", 120, false, true),
        ("peer-current-endpoint", 120, false, true),
        ("peer-current-endpoint-narrow", 60, false, true),
        ("peer-current-endpoint-short", 60, false, true),
        ("peer-current-repair-failed", 120, false, true),
        ("peer-current-repair-failed-narrow", 60, false, true),
        ("peer-current-repair-failed-short", 60, false, true),
        ("peer-current-repair-deferred", 120, false, true),
        ("peer-current-repair-deferred-narrow", 60, false, true),
        ("peer-current-repair-deferred-short", 60, false, true),
        ("peer-current-repair-completed", 120, false, true),
        ("peer-current-repair-completed-narrow", 60, false, true),
        ("peer-current-repair-completed-short", 60, false, true),
        ("peer-expired-endpoint", 120, false, true),
        ("peer-history-current", 120, false, true),
        ("peer-history-current-short", 60, false, true),
        ("peer-history-current-rollback", 120, false, true),
        ("peer-history-current-rollback-short", 60, false, true),
        ("peer-history-expired", 120, false, true),
        ("peer-history-expired-short", 60, false, true),
        ("peer-history-only", 120, false, true),
        ("peer-history-only-short", 60, false, true),
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
        ("services-refresh-failed", 120, false, true),
        ("services-refresh-failed-narrow", 60, false, true),
        ("services-refresh-failed-reader-busy", 120, false, true),
        (
            "services-refresh-failed-reader-busy-narrow",
            60,
            false,
            true,
        ),
        ("services-refresh-recovered", 120, false, true),
        ("services-refresh-recovered-narrow", 60, false, true),
        ("services-catalog", 120, false, true),
        ("services-catalog-short", 60, false, true),
        ("services-short", 60, false, true),
        ("services-short-wide", 120, false, true),
    ] {
        let _place = crate::net_place::hold_scoped(
            name.starts_with("away-")
                .then(|| aterm_netplace::sample::hotspot(1_793_040_000_000, 1_793_040_000_000)),
        );
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
        let mut history_peers = 1;
        if name == "history-unavailable-relayed-narrow" {
            let scale = (app.windows[&wid].scale as f32).max(0.5);
            let (cw, _) = app.win_cell_size(wid);
            let pad = app.win_pad(wid);
            let width = |text| {
                crate::tray_raster::ui_text_width_for(
                    crate::widget::TextFace::Ui,
                    text,
                    9.5 * scale,
                )
                .ceil()
            };
            // Keep cached service cells visible while retained peer boxes leave exactly
            // enough caption room for the shorter failure cue.
            (cols, history_peers) = (40_u16..=60)
                .rev()
                .find_map(|cols| {
                    let w = f32::from(cols) * cw as f32 + 2.0 * pad as f32;
                    (1..=40).find_map(|peers| {
                        let room = w - (144.0 + 14.0 * peers as f32) * scale;
                        (room > 70.0 * scale
                            && room < width("History unavailable")
                            && room >= width("History error"))
                        .then_some((cols, peers))
                    })
                })
                .expect("constrained history caption fixture");
        }
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
            last_repair: None,
            last_authenticated_endpoint: None,
            reachability: Reachability::Reachable,
            instances: None,
            samples: Vec::new(),
            lease_ms: None,
        });
        let repair_scene = name.contains("-repair-");
        if repair_scene {
            let outcome = if name.contains("failed") {
                RepairOutcome::Failed(aterm_types::network_state::RepairCause::Io)
            } else if name.contains("deferred") {
                RepairOutcome::Deferred(None)
            } else {
                RepairOutcome::Exchanged
            };
            snapshot.network.peers[0].last_repair = Some(RepairObservation {
                completed_at_ms: snapshot.network.observed_at_ms - 10_000,
                outbound: true,
                stage: Some(RepairStage::Application),
                outcome,
                table: Some([7; 32]),
                sent: Some(7),
                received: (!matches!(outcome, RepairOutcome::Failed(_))).then_some(3),
                deferred: Some(if outcome == RepairOutcome::Exchanged {
                    0
                } else {
                    2
                }),
            });
            for service in [
                &mut snapshot.services.accounts,
                &mut snapshot.services.secrets,
            ] {
                *service = ServiceState {
                    availability: ServiceAvailability::Cached,
                    observed_at_ms: snapshot.network.observed_at_ms,
                    detail: "Synthetic local cache; the recorded repair does not establish remote currency or provider sign-in".into(),
                    ..ServiceState::default()
                };
            }
        }
        let recorded_repair = repair_scene.then(|| snapshot.network.peers[0].last_repair.clone());
        if name.starts_with("history-") || name.starts_with("away-history-") {
            snapshot.network.history = if name.contains("history-unavailable") {
                HistoryState::Unavailable
            } else {
                HistoryState::Restoring
            };
            snapshot.network.peers[0].reachability = Reachability::Unknown;
            snapshot.network.detail = if snapshot.network.history == HistoryState::Unavailable {
                "Fixture journal cannot be read or safely written; authenticated contacts are not accepted".into()
            } else {
                "Fixture retained network history is being restored".into()
            };
            for slot in 2..=history_peers {
                let mut peer = snapshot.network.peers[0].clone();
                peer.slot = slot as u64;
                peer.identity = identity(slot as u8 + 1, &format!("fixture-retained-{slot}"));
                snapshot.network.peers.push(peer);
            }
            if name.contains("relayed") {
                snapshot.network.via_local_process = Some(42);
            }
            for service in [
                &mut snapshot.services.accounts,
                &mut snapshot.services.secrets,
            ] {
                *service = ServiceState {
                    availability: ServiceAvailability::Cached,
                    observed_at_ms: snapshot.network.observed_at_ms,
                    detail: "Synthetic locally cached shared metadata; current remote state and provider login are not established".into(),
                    ..ServiceState::default()
                };
            }
        }
        if name.contains("endpoint") {
            let peer = &mut snapshot.network.peers[0];
            peer.identity.label = None;
            peer.last_authenticated_endpoint = Some(AuthenticatedEndpoint {
                address: "[2001:db8::7]:9699".parse().expect("fixture endpoint"),
                observed_at_ms: peer.last_seen_ms - 86_400_000,
            });
        }
        if name.starts_with("peer-history-") {
            let at = snapshot.network.observed_at_ms;
            snapshot.network.peers[0].samples = [
                (at - 120_000, Reachability::Reachable),
                (at - 90_000, Reachability::LastSeen),
                (at - 60_000, Reachability::Reachable),
                (at, Reachability::Reachable),
            ]
            .map(|(at_ms, state)| PresenceSample { at_ms, state })
            .into();
            if name.contains("rollback") {
                snapshot.network.peers[0].first_seen_ms = at + 9_999;
                snapshot.network.peers[0].samples = [
                    (at + 9_999, Reachability::Reachable),
                    (at - 1, Reachability::LastSeen),
                    (at, Reachability::Reachable),
                ]
                .map(|(at_ms, state)| PresenceSample { at_ms, state })
                .into();
            }
            if name.starts_with("peer-history-only") {
                snapshot.network.visibility = Visibility::Unavailable;
                snapshot.network.detail =
                    "Retained history; current reachability has not been checked".into();
            }
        }
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
            if name.starts_with("services-refresh") {
                use aterm_types::network_state::{VaultNetworkRefresh, VaultNetworkRefreshOutcome};
                let failed = name.starts_with("services-refresh-failed");
                snapshot.services.secrets.refresh_failed = failed;
                snapshot.services.secrets.network_refresh = Some(VaultNetworkRefresh {
                    completed_at_ms: snapshot.network.observed_at_ms - 2_000,
                    outcome: if failed {
                        VaultNetworkRefreshOutcome::Failed
                    } else {
                        VaultNetworkRefreshOutcome::Succeeded
                    },
                });
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
        if name.starts_with("away-") {
            // With no peer box to warn, the words must still expose a blocked observer.
            snapshot.network.peers.clear();
            for service in [
                &mut snapshot.services.accounts,
                &mut snapshot.services.secrets,
            ] {
                *service = ServiceState {
                    availability: ServiceAvailability::Cached,
                    observed_at_ms: snapshot.network.observed_at_ms,
                    detail: "Synthetic independently cached shared metadata".into(),
                    ..ServiceState::default()
                };
            }
        }
        app.network_source = None;
        app.network_snapshot = Arc::new(snapshot);
        if name.starts_with("away-expired") {
            let expired_at = app.network_snapshot.network.observed_at_ms + 15_001;
            app.network_snapshot =
                crate::network_deck::fresh_snapshot(app.network_snapshot.clone(), Some(expired_at));
        }
        if name.contains("reader-busy") {
            app.network_snapshot = crate::network_deck::snapshot_after_read(
                None,
                &app.network_snapshot,
                &mut app.network_source_busy,
                None,
            );
        }
        if name.starts_with("peer-") {
            let mut snapshot = app.network_snapshot.as_ref().clone();
            let contact_at = snapshot.network.observed_at_ms;
            if !name.contains("rollback") {
                snapshot.network.observed_at_ms += 8_000;
            }
            if name == "peer-unknown" {
                snapshot.network.peers[0].last_checked_ms = None;
            }
            let age =
                if name.starts_with("peer-current") || name.starts_with("peer-history-current") {
                    15_000
                } else if name.starts_with("peer-history-expired") {
                    30_001
                } else {
                    15_001
                };
            app.network_snapshot =
                crate::network_deck::fresh_snapshot(Arc::new(snapshot), Some(contact_at + age));
        }
        if let Some(recorded_repair) = recorded_repair {
            let snapshot = &app.network_snapshot;
            assert_eq!(snapshot.network.peers[0].last_repair, recorded_repair);
            assert_eq!(
                snapshot.network.peers[0].reachability,
                Reachability::Reachable
            );
            assert_eq!(
                snapshot.services.accounts.availability,
                ServiceAvailability::Cached
            );
            assert_eq!(
                snapshot.services.secrets.availability,
                ServiceAvailability::Cached
            );
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
                    crate::network_deck::Target::Seat(crate::network_deck::SeatKey::Observed(1));
            }
            if name.starts_with("services") {
                app.windows.get_mut(&wid).unwrap().stats_lens.network_target =
                    if name.starts_with("services-failed") || name.contains("catalog") {
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
