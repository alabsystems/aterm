// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    ApplyLedgerVerdict, ApplyPostureEnv, UPDATE_DIDNT_INSTALL, UPDATE_WAITS_FOR_YOU,
    apply_ledger_verdict, debug_seamless_reexec_armed,
};
use crate::App;
use crate::WindowId;
use crate::native_app::{AppViewState, UpdateOutcome};
use crate::native_updater_service::CheckStart;

/// Verify `debug_seamless_reexec_armed` does not deadlock on first initialization.
#[test]
fn the_reexec_seam_reads_the_environment_and_never_itself() {
    let armed = debug_seamless_reexec_armed();
    assert_eq!(
        armed,
        aterm_types::control_socket::env_flag_engaged(
            std::env::var_os("ATERM_DEBUG_SEAMLESS_REEXEC")
                .map(|v| v.to_string_lossy().into_owned())
                .as_deref(),
        ),
        "the seam is armed exactly when its variable is ENGAGED (non-empty and not \
         \"0\") — the one flag rule every ATERM_NO_* / QA-seam reader shares, so an \
         inherited empty variable can never arm it"
    );
}

/// Only a ready `Staged` row applies on press; other rows and un-staged clicks open `/updates`.
#[test]
fn the_update_row_and_menu_apply_only_when_staged_and_ready() {
    let on_software_update = |app: &App| {
        let (_, view) = app
            .active_native_view(WindowId(0))
            .expect("native Settings tab");
        matches!(
            app.native_runtime.view_state(view),
            Some(AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::SoftwareUpdate
        )
    };
    let mut app = App::headless_for_test();
    let id = app.post_update_row(
        crate::update_words::progress(
            &aterm_update::Progress::Staged {
                version: "9.9.9".into(),
                build: 1,
            },
            None,
            "",
        ),
        "9.9.9",
        crate::messages_host::FlowPhase::Staged { build: 1 },
    );
    assert_eq!(app.staged_update_row(), Some(id));
    app.perform_intent(
        WindowId(0),
        id,
        aterm_messages::Intent::ApplyUpdate { build: 1 },
    );
    assert!(on_software_update(&app), "the row press");

    let mut app = App::headless_for_test();
    assert!(
        !app.staged_update_ready(),
        "test env must not have a strictly-newer staged build"
    );
    app.apply_update_or_details();
    assert!(on_software_update(&app), "the menu item");
}

/// Enabling serious mode mid-update zeroes the upgrade rim's motion while keeping the rim active.
#[test]
fn serious_mode_mid_update_stills_the_rim_and_keeps_it() {
    let mut app = App::headless_for_test();
    app.system_reduce_motion = false;
    app.serious_mode = false;
    app.spawn_upgrade_surge(41);
    let at = std::time::Instant::now() + std::time::Duration::from_secs(1);
    let frame = std::time::Duration::from_millis(100);
    let moving = |app: &App| {
        let rim = app.level_up.as_ref().expect("the rim is up");
        rim.fingerprint(at) != rim.fingerprint(at + frame)
    };
    assert!(moving(&app), "PRECONDITION: the rim animates");
    assert!(app.set_serious_mode(true));
    assert!(!moving(&app), "serious mode keeps the rim, still");
}

#[test]
fn selectable_update_menu_click_surfaces_dirty_native_preflight_block() {
    let dir = std::env::temp_dir().join(format!("aterm-update-menu-dirty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("draft.md");
    std::fs::write(&path, "draft\n").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();

    let mut app = App::headless_for_test();
    app.open_document_tab(crate::native_app::AppKind::Editor, &uri)
        .unwrap();
    let wid = WindowId(0);
    app.dispatch_native_event(
        wid,
        crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::Commit(
            "unsaved ".to_string(),
        )),
    )
    .unwrap();

    let outcome = app
        .apply_update_or_details_with_facts(true, true)
        .expect("staged click produces an apply outcome");
    assert!(matches!(
        &outcome,
        UpdateOutcome::Blocked { reasons }
            if reasons.iter().any(|reason| reason.contains("Checkpoint Drafts"))
    ));
    let (_, view) = app
        .active_native_view(wid)
        .expect("Software Update details tab");
    assert!(matches!(
        app.native_runtime.view_state(view),
        Some(AppViewState::Settings(state))
            if state.route == crate::native_settings::SettingsRoute::SoftwareUpdate
    ));
    let _ = std::fs::remove_dir_all(dir);
}

/// Stage `build` through the check reducer along with the current ledger's `apply_lane` report.
#[cfg(target_os = "macos")]
fn stage_build_with_ledger(app: &mut App, build: u64) {
    let current_build = app.native_updater_service.snapshot().current_build;
    let base = aterm_update::status(current_build)
        .map(crate::app_native::durable_update_status)
        .expect("the scratch staging root yields a status under test");
    let status = crate::native_updater_service::DurableUpdateStatus {
        linux_host: false,
        linux: None,
        enabled: true,
        staged_build: Some(build),
        staged_version: Some(format!("1.0.{build}")),
        staged_commit: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
        staged_dmg_sha256: Some("ab".repeat(32)),
        changelog: None,
        outcome: "staged".to_string(),
        installable: Some(true),
        channel_unreadable: false,
        ..base
    };
    let CheckStart::Start(ticket) = app.native_updater_service.request_check() else {
        panic!("a service between checks must start exactly one");
    };
    assert_eq!(
        app.native_updater_service.finish_check(ticket, status),
        crate::native_updater_service::CheckCompletion::Reduced,
    );
    app.publish_native_update_state();
}

/// An `AutoApplyIntent` for `build`, i.e. the state in which the automatic lane
/// still means to try this artifact by itself.
#[cfg(target_os = "macos")]
fn arm_intent(app: &mut App, build: u64) {
    app.auto_apply_manual_only = None;
    app.auto_apply_intent = Some(crate::AutoApplyIntent {
        build,
        dmg_sha256: [0xab; 32],
        retry_at: std::time::Instant::now() + std::time::Duration::from_secs(600),
        attempts: 0,
        deadline_owner: crate::AutoApplyDeadlineOwner::Ordinary,
        held: crate::native_update_lane_hold::NamedHold::default(),
    });
}

/// When a sibling stages a replacement build while an apply attempt runs, a returned
/// failure is attributed to the attempted build rather than the replacement stage.
#[cfg(target_os = "macos")]
#[test]
fn a_returned_failure_is_not_worded_for_the_stage_that_replaced_it() {
    use crate::messages_host::FlowPhase;
    use crate::native_updater_service::{
        ApplyDecision, ApplyPreflightStart, ClosePreflight, ReturnedApplyDisposition,
        ReturnedApplyFacts,
    };
    use crate::update_words::ApplyPosture;
    let _ledger = super::hold_update_ledger_for_test();
    let message = "overlap handoff failed safely: candidate exited before its proof; the \
                   download on disk has since changed";
    let failed = || UpdateOutcome::Failed {
        message: message.to_string(),
    };
    for decision in [false, true] {
        for source in ["manual handoff", "automatic handoff"] {
            let case = format!("{source} decision={decision}");
            let mut app = App::headless_for_test();
            let running = app.native_updater_service.snapshot().current_build;
            let tried = running + 77_001;
            let replacement = tried + 1;
            stage_build_with_ledger(&mut app, tried);
            app.clear_messages_for_test();
            app.ensure_staged_decision_in(tried, ApplyPosture::ManualByConfig);
            assert!(
                app.staged_update_row().is_some(),
                "{case}: PRECONDITION: its ready row"
            );
            app.begin_update_installing(!super::source_is_automatic(source));
            let ApplyPreflightStart::Inspect(preflight) =
                app.native_updater_service.begin_apply_preflight()
            else {
                panic!("the stage admits preflight");
            };
            let ApplyDecision::Execute(command) = app
                .native_updater_service
                .finish_apply_preflight(preflight, ClosePreflight::Ready)
            else {
                panic!("preflight mints the attempt");
            };
            let attempt = command.attempt();
            command.execute(|| ());
            assert_eq!(
                app.native_updater_service.finish_returned_apply(
                    &attempt,
                    ReturnedApplyFacts::new(
                        true,
                        Some(replacement),
                        Some(attempt.target_commit()),
                        Some(attempt.target_dmg_sha256()),
                        None,
                    ),
                    "candidate exited before its proof",
                ),
                ReturnedApplyDisposition::Retired
            );
            stage_build_with_ledger(&mut app, replacement);
            if !decision {
                arm_intent(&mut app, replacement);
            }
            app.retire_update_installing();
            assert_eq!(
                app.live_update_flow().map(|flow| flow.phase),
                Some(FlowPhase::Staged { build: tried }),
                "{case}: PRECONDITION: the tried build's ready row is back"
            );
            if decision {
                app.record_apply_outcome_for_target_in_ledger(&failed(), tried);
                app.say_superseded_apply_failure(
                    source,
                    message,
                    tried,
                    replacement,
                    ApplyPosture::ManualByConfig,
                );
            } else {
                app.surface_update_apply_outcome_for_target(source, failed(), false, tried);
            }
            let tried_version = format!("v1.0.{tried}");
            let new_version = format!("v1.0.{replacement}");
            for l in app.messages.live_rows() {
                let said = format!("{} {}", l.msg.title, l.msg.detail.join(" "));
                assert!(!said.contains(&tried_version), "{case}: stale row: {said}");
                if l.msg.key.as_deref() == Some(crate::update_words::KEY_OUTCOME) {
                    assert!(!said.contains(&new_version), "{case}: {said}");
                    assert!(
                        !l.msg
                            .actions
                            .iter()
                            .any(|a| matches!(a, aterm_messages::Intent::ApplyUpdate { .. })),
                        "{case}: no press on the failure for bytes nobody tried"
                    );
                }
            }
            if let Some(said) = app.update_record_text() {
                assert!(!said.contains(&new_version), "{case}: {said}");
            }
            let told = if super::source_is_automatic(source) {
                app.update_record_text()
            } else {
                app.messages
                    .live_by_key(crate::update_words::KEY_OUTCOME)
                    .map(|l| format!("{} — {}", l.msg.title, l.msg.detail.join("; ")))
            };
            assert!(told.is_some(), "{case}: the failure is said");
            let ready = app
                .live_update_flow()
                .filter(|flow| flow.phase == FlowPhase::Staged { build: replacement })
                .and_then(|flow| app.messages.live(flow.id))
                .map(|l| (l.msg.title.clone(), l.msg.actions.clone()));
            if decision {
                let (title, actions) = ready.expect("the new stage's decision is raised");
                assert!(title.contains(&new_version), "{case}: {title}");
                assert!(
                    actions.contains(&aterm_messages::Intent::ApplyUpdate { build: replacement }),
                    "{case}: its own press"
                );
            } else {
                assert_eq!(ready, None, "{case}");
            }
        }
    }
    let mut app = App::headless_for_test();
    let running = app.native_updater_service.snapshot().current_build;
    let tried = running + 77_101;
    stage_build_with_ledger(&mut app, tried);
    app.clear_messages_for_test();
    app.surface_update_apply_outcome_for_target("manual handoff", failed(), false, tried);
    let row = app.update_row_text().expect("a stopped lane is a row");
    assert!(row.contains(&format!("v1.0.{tried}")), "{row}");
}

#[cfg(target_os = "macos")]
#[test]
fn returned_failure_ledger_conforms_to_attempt_target_after_stage_replacement() {
    use crate::native_updater_service::{
        ApplyDecision, ApplyPreflightStart, ClosePreflight, ReturnedApplyDisposition,
        ReturnedApplyFacts,
    };
    let _ledger = super::hold_update_ledger_for_test();
    let model = aterm_models_update::native_update_failure_target_model();
    for replaced in [false, true] {
        let mut app = App::headless_for_test();
        let running = app.native_updater_service.snapshot().current_build;
        let original = running + 65_101 + u64::from(replaced) * 2;
        let replacement = original + 1;
        stage_build_with_ledger(&mut app, original);
        let ApplyPreflightStart::Inspect(preflight) =
            app.native_updater_service.begin_apply_preflight()
        else {
            panic!("the original stage must admit preflight");
        };
        let ApplyDecision::Execute(command) = app
            .native_updater_service
            .finish_apply_preflight(preflight, ClosePreflight::Ready)
        else {
            panic!("preflight must mint the original attempt");
        };
        let attempt = command.attempt();
        command.execute(|| ());
        let disposition = app.native_updater_service.finish_returned_apply(
            &attempt,
            ReturnedApplyFacts::new(
                true,
                Some(if replaced { replacement } else { original }),
                Some(attempt.target_commit()),
                Some(attempt.target_dmg_sha256()),
                None,
            ),
            "the original candidate did not start",
        );
        let mut before = model.init_state();
        if replaced {
            assert_eq!(disposition, ReturnedApplyDisposition::Retired);
            stage_build_with_ledger(&mut app, replacement);
            before = model.successors("ReplaceStage", &before).remove(0);
        } else {
            assert_eq!(disposition, ReturnedApplyDisposition::Rearmed);
        }
        assert_eq!(
            app.native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .unwrap()
                .build,
            if replaced { replacement } else { original },
        );
        app.surface_update_apply_outcome_for_target(
            "manual handoff",
            UpdateOutcome::Failed {
                message: "the original candidate did not start".to_string(),
            },
            false,
            attempt.target_build(),
        );
        let report = aterm_update::apply_lane_report(running).expect("real ledger is readable");
        assert_eq!(report.last_failure_target_build, original);
        assert!(report.failures_for_target > 0);
        let mut after = before.clone();
        after.insert(
            "charged",
            if report.last_failure_target_build == original {
                1
            } else {
                2
            },
        );
        assert_eq!(model.successors("Publish", &before), vec![after.clone()]);
        assert!(model.check_invariant("FailureBelongsToAttempt", &after));
        if replaced {
            assert!(
                app.apply_trouble_for(replacement).is_none(),
                "new bytes inherit no failure"
            );
            app.record_apply_outcome_in_ledger(&UpdateOutcome::Failed {
                message: "historical inferred target".to_string(),
            });
            let historical = aterm_update::apply_lane_report(running).unwrap();
            assert_eq!(historical.last_failure_target_build, replacement);
            after.insert("charged", 2);
            assert!(!model.successors("Publish", &before).contains(&after));
            assert!(!model.check_invariant("FailureBelongsToAttempt", &after));
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn installed_and_deferred_outcomes_only_promise_a_retained_automatic_retry() {
    let _ledger = super::hold_update_ledger_for_test();
    for (automatic, armed) in [(false, true), (true, false), (true, true)] {
        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;
        stage_build_with_ledger(&mut app, build);
        app.config.update = Some(crate::app_config::UpdateConfig {
            auto_apply: Some(automatic),
            ..app.config.update.clone().unwrap_or_default()
        });
        if armed {
            arm_intent(&mut app, build);
        }
        app.react_to_update_apply_outcome(
            "manual",
            UpdateOutcome::InstalledNeedsRelaunch {
                build,
                message: App::INSTALLED_ACTIVATES_IN_PLACE.to_string(),
            },
            false,
        );
        let scheduled = automatic && armed;
        assert_eq!(
            app.update_row_text().is_some(),
            !scheduled,
            "no row for a retry that happens by itself"
        );

        app.react_to_update_apply_outcome(
            "manual",
            UpdateOutcome::Deferred {
                reason: "typing gap".to_string(),
                hold: crate::native_update_lane_hold::LaneHold::Activity,
            },
            false,
        );
        assert_eq!(
            app.update_row_text().is_some(),
            !scheduled,
            "no row for a retry that happens by itself"
        );
    }
}

/// Verify `apply_posture_with` precedence: unavailable handoff > policy > screenshot
/// seam > per-build stand-down latch > automatic.
#[test]
fn apply_posture_names_config_env_latch_and_disabled_handoff_in_that_order() {
    use crate::app_config::AutoApplySetting;
    use crate::app_update_handoff::HandoffUnavailable;
    use crate::update_apply_trouble::ApplyRetry;
    use crate::update_words::ApplyPosture as P;
    let mut app = App::headless_for_test();
    assert!(
        crate::app_config::update_auto_apply(&app.config)
            && !App::relaunch_nudge_seam_suppresses_auto_apply()
            && crate::cli::launch_flags().control_sock.is_none(),
        "PRECONDITION: no update veto may be set in the test environment"
    );
    let build = stage_one_build(&mut app);
    let latch = |build: u64, retry_at: Option<std::time::Instant>| crate::AutoApplyManualOnly {
        replacement: None,
        build,
        dmg_sha256: [0xab; 32],
        activation: false,
        retry_at,
    };
    let later = Some(std::time::Instant::now() + std::time::Duration::from_secs(600));
    let clean = ApplyPostureEnv {
        handoff_unavailable: None,
        auto_apply: AutoApplySetting::On,
        nudge_seam: false,
    };
    assert_eq!(
        app.apply_posture_for(build),
        P::HandoffDisabled {
            why: HandoffUnavailable::Headless,
            veto: None,
            image_kept: false,
        },
        "a headless App has no seamless lane, and the bar says so"
    );
    assert_eq!(
        app.apply_posture_with(build, clean),
        P::Automatic,
        "the default policy, with the lane available"
    );

    app.auto_apply_manual_only = Some(latch(build, later));
    assert_eq!(
        app.apply_posture_with(build, clean),
        P::ManualOnlyLatched { lapses: true }
    );
    assert_eq!(
        app.apply_retry_for(Some(build)),
        ApplyRetry::Scheduled,
        "…which is the Scheduled row: the same fact, on the row"
    );
    app.auto_apply_manual_only = Some(latch(build, None));
    assert_eq!(
        app.apply_posture_with(build, clean),
        P::ManualOnlyLatched { lapses: false }
    );
    assert_eq!(app.apply_retry_for(Some(build)), ApplyRetry::ManualOnly);
    app.auto_apply_manual_only = Some(latch(build + 1, None));
    assert_eq!(
        app.apply_posture_with(build, clean),
        P::Automatic,
        "another build's latch is not this build's"
    );

    app.auto_apply_manual_only = Some(latch(build, later));
    assert_eq!(
        app.apply_posture_with(
            build,
            ApplyPostureEnv {
                auto_apply: AutoApplySetting::OffByConfig,
                ..clean
            }
        ),
        P::ManualByConfig
    );
    assert_eq!(
        app.apply_posture_with(
            build,
            ApplyPostureEnv {
                nudge_seam: true,
                ..clean
            }
        ),
        P::VetoedByEnv {
            var: "ATERM_DEBUG_RELAUNCH_NUDGE"
        }
    );
    for why in HandoffUnavailable::ALL {
        assert_eq!(
            app.apply_posture_with(
                build,
                ApplyPostureEnv {
                    handoff_unavailable: Some(why),
                    auto_apply: AutoApplySetting::OffByConfig,
                    nudge_seam: true,
                }
            ),
            P::HandoffDisabled {
                why,
                veto: Some(crate::update_words::AutoApplyVeto::Config),
                image_kept: false,
            }
        );
    }
    assert_eq!(
        app.apply_posture_with(
            build,
            ApplyPostureEnv {
                handoff_unavailable: Some(HandoffUnavailable::Headless),
                auto_apply: AutoApplySetting::On,
                nudge_seam: true,
            }
        ),
        P::HandoffDisabled {
            why: HandoffUnavailable::Headless,
            veto: Some(crate::update_words::AutoApplyVeto::Env {
                var: "ATERM_DEBUG_RELAUNCH_NUDGE"
            }),
            image_kept: false,
        }
    );
    assert_eq!(
        app.apply_posture_with(
            build,
            ApplyPostureEnv {
                handoff_unavailable: Some(HandoffUnavailable::Headless),
                ..clean
            }
        ),
        P::HandoffDisabled {
            why: HandoffUnavailable::Headless,
            veto: None,
            image_kept: false,
        }
    );

    app.auto_apply_manual_only = None;
    app.config.update = Some(crate::app_config::UpdateConfig {
        auto_apply: Some(false),
        ..app.config.update.clone().unwrap_or_default()
    });
    let observed = ApplyPostureEnv::observe(&app);
    assert_eq!(observed.auto_apply, AutoApplySetting::OffByConfig);
    assert_eq!(
        app.apply_posture_with(
            build,
            ApplyPostureEnv {
                handoff_unavailable: None,
                ..observed
            }
        ),
        P::ManualByConfig
    );
    assert_eq!(
        app.apply_posture_for(build),
        P::HandoffDisabled {
            why: HandoffUnavailable::Headless,
            veto: Some(crate::update_words::AutoApplyVeto::Config),
            image_kept: false,
        }
    );
}

/// `note_update_progress` records `apply_posture_for` on a `Staged` report.
#[test]
fn a_staged_report_paints_the_bar_with_the_posture_the_app_computed() {
    use crate::app_update_handoff::HandoffUnavailable;
    use crate::update_words::{ApplyPosture, staged_detail};
    let mut app = App::headless_for_test();
    assert!(
        crate::app_config::update_auto_apply(&app.config)
            && !App::relaunch_nudge_seam_suppresses_auto_apply()
            && crate::cli::launch_flags().control_sock.is_none(),
        "PRECONDITION: no update veto may be set in the test environment"
    );
    let build = stage_one_build(&mut app);
    let staged = aterm_update::Progress::Staged {
        version: "9.9.9".to_string(),
        build,
    };
    let staged_record = |app: &App| {
        app.messages
            .log()
            .records()
            .rev()
            .find(|r| r.key.as_deref() == Some(crate::update_words::KEY_PROGRESS))
            .filter(|r| r.retired() == Some(&aterm_messages::Retired::Recorded))
            .map(|r| r.detail.join("; "))
            .expect("the staged report is recorded")
    };
    app.note_update_progress(&staged);
    assert!(
        app.messages
            .live_by_key(crate::update_words::KEY_PROGRESS)
            .is_none(),
        "a staged build nothing can press is not a live row"
    );
    let headless = ApplyPosture::HandoffDisabled {
        why: HandoffUnavailable::Headless,
        veto: None,
        image_kept: false,
    };
    assert_eq!(staged_record(&app), staged_detail(headless));

    let latch = |retry_at| crate::AutoApplyManualOnly {
        replacement: None,
        build,
        dmg_sha256: [0xab; 32],
        activation: false,
        retry_at,
    };
    app.auto_apply_manual_only = Some(latch(None));
    assert_eq!(app.apply_posture_for(build), headless);
    let before = app.messages.revision();
    app.restate_staged_bar_posture(build);
    assert_eq!(app.messages.revision(), before, "the words did not change");
}

/// End-to-end check that `surface_update_apply_outcome` writes the ledger, updates
/// the reducer immediately, and projects the failure into the Version menu, palette,
/// and Settings page.
#[cfg(target_os = "macos")]
#[test]
fn one_real_failed_apply_reaches_every_standing_surface_at_once() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let current_build = app.native_updater_service.snapshot().current_build;
    let build = current_build + 4_101;
    stage_build_with_ledger(&mut app, build);
    arm_intent(&mut app, build);

    assert!(
        app.apply_trouble_for(build).is_none(),
        "a stage nothing has attempted carries no trouble"
    );

    app.surface_update_apply_outcome(
        "automatic handoff",
        UpdateOutcome::Failed {
            message: "overlap handoff failed safely: handoff proof ended ChildDied".to_string(),
        },
        false,
    );
    use crate::update_apply_trouble::{ApplyRetry, ApplyTrouble};

    let message = "overlap handoff failed safely: handoff proof ended ChildDied";
    let trouble = app
        .apply_trouble_for(build)
        .expect("the failure that just happened is standing trouble for this build");
    assert_eq!(
        Some(&trouble),
        ApplyTrouble::new(1, message, ApplyRetry::Scheduled).as_ref(),
        "one attempt, its cause, and the armed lane's retry"
    );
    assert_ne!(
        crate::menu::staged_apply_label("\u{2191}", build, "9.9.9", Some(&trouble), false),
        crate::menu::staged_apply_label("\u{2191}", build, "9.9.9", None, false),
        "the menu item carries the trouble"
    );

    let live = app.palette_live();
    assert_eq!(
        live.staged.as_ref().map(|(b, _)| *b),
        Some(build),
        "the ledger write republished the staged nudge"
    );
    assert_eq!(live.staged_trouble.as_ref(), Some(&trouble));

    assert_eq!(
        app.update_snapshot(false)
            .projection()
            .apply_trouble
            .as_ref(),
        Some(&trouble),
        "the failure reached the page at once"
    );

    app.auto_apply_intent = None;
    app.auto_apply_manual_only = None;
    let latched = ApplyTrouble::new(1, message, ApplyRetry::ManualOnly);
    assert_eq!(app.apply_trouble_for(build), latched);
    assert_eq!(
        app.update_snapshot(false).projection().apply_trouble,
        latched,
        "a lane that has stopped must say so on the page too"
    );
}

/// Verify a fresh `App` imports prior apply failure count and reason from the ledger via `durable_update_status`.
#[cfg(target_os = "macos")]
#[test]
fn a_check_that_lands_on_a_ledger_reads_the_reason_and_not_only_the_count() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let current_build = app.native_updater_service.snapshot().current_build;
    let staged = current_build + 6_301;

    let recorded = aterm_update::record_apply_failure(
        current_build,
        staged,
        "overlap handoff failed safely: handoff proof ended AdoptionMismatch",
    )
    .expect("the scratch staging root is resolvable under test");
    assert_eq!(recorded.failures_for_target, 1);

    stage_build_with_ledger(&mut app, staged);
    app.auto_apply_intent = None;
    app.auto_apply_manual_only = None;

    use crate::update_apply_trouble::{ApplyRetry, ApplyTrouble};
    let expected = ApplyTrouble::new(
        1,
        "overlap handoff failed safely: handoff proof ended AdoptionMismatch",
        ApplyRetry::ManualOnly,
    );
    assert_eq!(
        app.apply_trouble_for(staged),
        expected,
        "the CAUSE came off the disk, and so did the fact that nothing is scheduled"
    );
    assert_eq!(
        app.update_snapshot(false).projection().apply_trouble,
        expected,
        "…and the page reads the same"
    );
}

/// Staging a newer build does not inherit `apply_trouble_for` from the superseded build.
#[cfg(target_os = "macos")]
#[test]
fn a_newer_stage_starts_clean_even_though_the_running_build_has_not_changed() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let current_build = app.native_updater_service.snapshot().current_build;
    let superseded = current_build + 5_201;
    let fresh = superseded + 1;

    let recorded = aterm_update::record_apply_failure(
        current_build,
        superseded,
        "overlap handoff failed safely: handoff proof ended ChildDied",
    )
    .expect("the scratch staging root is resolvable under test");
    assert_eq!(recorded.failures_for_target, 1);
    assert!(app.native_updater_service.note_apply_failure(&recorded));
    assert!(
        app.apply_trouble_for(superseded).is_some(),
        "PRECONDITION: the artifact that failed wears its own failure"
    );

    stage_build_with_ledger(&mut app, fresh);
    arm_intent(&mut app, fresh);
    assert!(
        app.native_updater_service.snapshot().failing_applies > 0,
        "PRECONDITION: the escalation streak really does survive the new stage — \
         that is what made this a bug and not a hypothetical"
    );
    assert_eq!(
        app.apply_trouble_for(fresh),
        None,
        "a build that has never been attempted must not wear the previous one's \
         attempts"
    );
    assert!(
        app.update_snapshot(false)
            .projection()
            .apply_trouble
            .is_none(),
        "…and the page agrees with the menu"
    );

    app.surface_update_apply_outcome(
        "automatic handoff",
        UpdateOutcome::Failed {
            message: "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
        },
        false,
    );
    let trouble = app.apply_trouble_for(fresh).expect("its own failure");
    assert_eq!(
        Some(trouble.with_retry_in(None)),
        crate::update_apply_trouble::ApplyTrouble::new(
            1,
            "overlap handoff failed safely: handoff proof ended TimedOut",
            app.apply_retry_for(Some(fresh)),
        ),
        "the count restarted with the artifact, and the CAUSE moved with it"
    );
}

/// `apply_trouble_for` returns `None` when the target build is not newer than `current_build`.
#[cfg(target_os = "macos")]
#[test]
fn a_failure_recorded_against_the_running_build_decorates_nothing() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let current_build = app.native_updater_service.snapshot().current_build;
    let recorded = aterm_update::record_apply_failure(
        current_build,
        current_build,
        "update trial for build 1787699398 was unrecoverable across 3 launches of build \
         1787690000; disarmed the boot sentinel to keep updates possible",
    )
    .expect("the scratch staging root is resolvable under test");
    assert_eq!(recorded.target_build, current_build);
    assert!(app.native_updater_service.note_apply_failure(&recorded));

    assert_eq!(
        app.apply_trouble_for(current_build),
        None,
        "there is no newer build on offer, so there is nothing to qualify"
    );
}

pub(crate) fn stage_one_build(app: &mut App) -> u64 {
    let current_build = app.native_updater_service.snapshot().current_build;
    let build = current_build + 1;
    let CheckStart::Start(ticket) = app.native_updater_service.request_check() else {
        panic!("a fresh service must start exactly one check");
    };
    assert_eq!(
        app.native_updater_service.finish_check(
            ticket,
            crate::native_updater_service::DurableUpdateStatus {
                linux_host: false,
                linux: None,
                enabled: true,
                current_build,
                staged_build: Some(build),
                staged_version: Some(format!("1.0.{build}")),
                staged_commit: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
                staged_dmg_sha256: Some("ab".repeat(32)),
                changelog: None,
                outcome: "staged".to_string(),
                failing_checks: 0,
                failing_persistent: false,
                failing_kind: String::new(),
                failing_applies: 0,
                apply_failure: String::new(),
                apply_failure_build: 0,
                apply_failures_for_target: 0,
                installable: Some(true),
                channel_unreadable: false,
                checked_at: None,
                hold: None,
                behind: false,
            },
        ),
        crate::native_updater_service::CheckCompletion::Reduced,
        "PRECONDITION: the check must reduce, or nothing is staged and the \
         predicate under test is never reached"
    );
    build
}

#[test]
fn current_intent_never_borrows_another_artifacts_cooldown() {
    let mut app = App::headless_for_test();
    let build = stage_one_build(&mut app);
    let later = std::time::Instant::now() + std::time::Duration::from_secs(600);
    app.auto_apply_intent = Some(crate::AutoApplyIntent {
        build,
        dmg_sha256: [0xab; 32],
        retry_at: later,
        attempts: 0,
        deadline_owner: crate::AutoApplyDeadlineOwner::Ordinary,
        held: crate::native_update_lane_hold::NamedHold::default(),
    });
    app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
        replacement: None,
        build,
        dmg_sha256: [0xcd; 32],
        activation: false,
        retry_at: Some(later),
    });
    assert!(app.automatic_apply_retry_scheduled(build));
    assert_eq!(app.automatic_apply_retry_in(build), None);
    app.auto_apply_manual_only.as_mut().unwrap().dmg_sha256 = [0xab; 32];
    assert!(app.automatic_apply_retry_in(build).is_some());
    app.auto_apply_manual_only.as_mut().unwrap().retry_at = None;
    assert_eq!(app.automatic_apply_retry_in(build), None);
}

#[test]
fn retry_and_latch_posture_require_the_exact_current_artifact() {
    use crate::app_config::AutoApplySetting;
    use crate::update_words::ApplyPosture;
    let mut app = App::headless_for_test();
    let build = stage_one_build(&mut app);
    let later = std::time::Instant::now() + std::time::Duration::from_secs(600);
    let available = ApplyPostureEnv {
        handoff_unavailable: None,
        auto_apply: AutoApplySetting::On,
        nudge_seam: false,
    };
    for digest in [[0xab; 32], [0xcd; 32]] {
        let exact = digest == [0xab; 32];
        app.auto_apply_manual_only = None;
        app.auto_apply_intent = Some(crate::AutoApplyIntent {
            build,
            dmg_sha256: digest,
            retry_at: later,
            attempts: 0,
            deadline_owner: crate::AutoApplyDeadlineOwner::Ordinary,
            held: crate::native_update_lane_hold::NamedHold::default(),
        });
        assert_eq!(app.automatic_apply_retry_scheduled(build), exact);
        assert!(!app.automatic_apply_retry_scheduled(build + 1));
        app.auto_apply_intent = None;
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            replacement: None,
            build,
            dmg_sha256: digest,
            activation: false,
            retry_at: Some(later),
        });
        assert_eq!(app.automatic_apply_retry_scheduled(build), exact);
        assert_eq!(
            app.apply_posture_with(build, available),
            if exact {
                ApplyPosture::ManualOnlyLatched { lapses: true }
            } else {
                ApplyPosture::Automatic
            }
        );
    }
    app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
        replacement: None,
        build,
        dmg_sha256: [0xab; 32],
        activation: false,
        retry_at: Some(later),
    });
    assert_eq!(
        app.native_updater_service
            .reconcile_durable_stage(true, None, None, None, None),
        crate::native_updater_service::DurableStageDisposition::Retired
    );
    assert!(!app.automatic_apply_retry_scheduled(build));
}

/// Scheduled automatic retries post no row; stopped lanes and manual failures do.
#[test]
fn a_scheduled_retry_is_silent_and_a_stopped_lane_names_the_menu() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    assert!(
        crate::app_config::update_auto_apply(&app.config),
        "PRECONDITION: the predicate requires the automatic lane enabled, and \
         the shipped default is ON"
    );
    let build = stage_one_build(&mut app);
    let failed = || UpdateOutcome::Failed {
        message: "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
    };
    let row = |app: &App| app.update_row_text();

    app.auto_apply_intent = None;
    app.auto_apply_manual_only = None;
    app.clear_messages_for_test();
    app.surface_update_apply_outcome("automatic", failed(), false);
    assert!(row(&app).is_some(), "a stopped lane is said");

    app.auto_apply_intent = Some(crate::AutoApplyIntent {
        build,
        dmg_sha256: [0xab; 32],
        retry_at: std::time::Instant::now() + std::time::Duration::from_secs(600),
        attempts: 0,
        deadline_owner: crate::AutoApplyDeadlineOwner::Ordinary,
        held: crate::native_update_lane_hold::NamedHold::default(),
    });
    app.clear_messages_for_test();
    app.surface_update_apply_outcome("automatic", failed(), false);
    assert_eq!(row(&app), None);

    app.auto_apply_intent = Some(crate::AutoApplyIntent {
        build: build + 7,
        dmg_sha256: [0xab; 32],
        retry_at: std::time::Instant::now() + std::time::Duration::from_secs(600),
        attempts: 0,
        deadline_owner: crate::AutoApplyDeadlineOwner::Ordinary,
        held: crate::native_update_lane_hold::NamedHold::default(),
    });
    app.clear_messages_for_test();
    app.surface_update_apply_outcome("automatic", failed(), false);
    assert!(row(&app).is_some(), "a stopped lane is said");

    app.auto_apply_intent = None;
    let lapsing = crate::AutoApplyManualOnly {
        replacement: None,
        build,
        dmg_sha256: [0xab; 32],
        activation: false,
        retry_at: Some(std::time::Instant::now() + std::time::Duration::from_secs(600)),
    };
    app.auto_apply_manual_only = Some(lapsing);
    for cycles in [1_u8, 8] {
        app.auto_apply_physical_retry = Some(crate::AutoOverlapRetry {
            build,
            dmg_sha256: [0xab; 32],
            activation: false,
            cycles,
            epoch_failures: 0,
            last_attempt: std::time::Instant::now(),
        });
        app.clear_messages_for_test();
        app.surface_update_apply_outcome("automatic handoff", failed(), false);
        assert_eq!(
            row(&app),
            None,
            "physical failure {cycles} for this artifact"
        );
    }
    app.auto_apply_physical_retry = None;

    app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
        replacement: None,
        build,
        dmg_sha256: [0xab; 32],
        activation: false,
        retry_at: None,
    });
    app.clear_messages_for_test();
    app.surface_update_apply_outcome("automatic", failed(), false);
    assert!(row(&app).is_some(), "a stopped lane is said");

    let manual_ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        build,
        "0123456789abcdef0123456789abcdef01234567",
        &"ab".repeat(32),
    );
    manual_ticket.make_current_apply_for_test(&mut app.native_updater_service);
    app.auto_apply_physical_retry = Some(crate::AutoOverlapRetry {
        build,
        dmg_sha256: [0xab; 32],
        activation: false,
        cycles: 5,
        epoch_failures: 1,
        last_attempt: std::time::Instant::now() - std::time::Duration::from_secs(600),
    });
    app.auto_apply_manual_only = Some(lapsing);
    app.clear_messages_for_test();
    let person = app.abort_reaped_native_apply_before_reconcile(
        &manual_ticket,
        "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
        crate::app_native::HandoffFailureLane::Manual,
    );
    assert_eq!(
        app.auto_apply_physical_retry.map(|retry| retry.cycles),
        Some(5),
        "a person's failure must not spend the automatic lane's budget — that \
         is how three clicks on a bad afternoon converged the background lane"
    );
    app.surface_update_apply_outcome("manual handoff", person, false);
    assert_eq!(row(&app), None, "a self-retry takes no row");
    assert!(
        app.update_record_text().is_some(),
        "whoever just asked for the update finds it did not happen, on record"
    );
    app.auto_apply_physical_retry = None;

    app.config.update = Some(crate::app_config::UpdateConfig {
        auto_apply: Some(false),
        ..app.config.update.clone().unwrap_or_default()
    });
    assert!(
        !crate::app_config::update_auto_apply(&app.config),
        "PRECONDITION: the opt-out actually took"
    );
    app.auto_apply_manual_only = Some(lapsing);
    app.clear_messages_for_test();
    app.surface_update_apply_outcome("automatic", failed(), false);
    assert!(row(&app).is_some(), "the opted-out lane is said");
}

/// Close preflight names user-actionable blockers (editor/Settings drafts) on the row
/// and suppresses self-clearing blockers while retry is scheduled.
#[test]
fn only_a_blocker_a_person_can_clear_is_named_on_the_row() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let blocked = |reasons: &[&str]| UpdateOutcome::Blocked {
        reasons: reasons.iter().map(|r| (*r).to_string()).collect(),
    };
    const SETTINGS: &str = "Review Settings Drafts: 1 Settings view(s) have unsaved text";
    for (reasons, said) in [
        (
            &[App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY][..],
            App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
        ),
        (&[SETTINGS][..], App::UNSAVED_SETTINGS_BLOCKS_APPLY),
        (
            &["Checkpoint Drafts: 2 document(s) have uncheckpointed edits"][..],
            App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
        ),
        (
            &[
                "Checkpoint Drafts: 2 document(s) have uncheckpointed edits",
                SETTINGS,
            ][..],
            App::UNSAVED_EDITOR_AND_SETTINGS_BLOCK_APPLY,
        ),
    ] {
        app.clear_messages_for_test();
        app.react_to_update_apply_outcome("automatic", blocked(reasons), false);
        assert_eq!(
            app.update_row_text(),
            Some(format!("{UPDATE_WAITS_FOR_YOU} \u{2014} {said}")),
            "{reasons:?}"
        );
    }
    app.clear_messages_for_test();
    app.react_to_update_apply_outcome(
        "automatic",
        blocked(&[App::RESTORE_IN_FLIGHT_BLOCKS_APPLY]),
        false,
    );
    assert_eq!(app.update_row_text(), None);
    assert_eq!(
        app.update_record_text(),
        Some(format!(
            "{UPDATE_DIDNT_INSTALL} \u{2014} {}",
            crate::update_words::INSTALL_FROM_MENU
        ))
    );
    app.clear_messages_for_test();
    app.note_update_blockers(&[App::RESTORE_IN_FLIGHT_BLOCKS_APPLY.to_string()], true);
    assert_eq!(app.update_row_text(), None);
    assert!(
        super::source_is_automatic("automatic · retrying later")
            && super::source_is_automatic("automatic policy fallback")
            && !super::source_is_automatic("manual handoff")
            && !super::source_is_automatic("control request")
    );
}

/// Verify `apply_ledger_verdict` maps `Accepted` and `InstalledNeedsRelaunch` to `Silent`,
/// `Failed` and `CaptureRefused` to `Failed`, and `Deferred`/`Blocked` to `Refused`.
#[test]
fn a_submitted_then_failed_apply_writes_one_streak_increment_and_never_a_clear() {
    use ApplyLedgerVerdict::{Failed, Refused, Silent};
    for (outcome, want) in [
        (UpdateOutcome::Accepted, Silent),
        (
            UpdateOutcome::Failed {
                message: "ChildDied".to_string(),
            },
            Failed("ChildDied".to_string()),
        ),
        (
            UpdateOutcome::CaptureRefused {
                message: "the park's capture refused session 3".to_string(),
            },
            Failed("the park's capture refused session 3".to_string()),
        ),
        (
            UpdateOutcome::Blocked {
                reasons: vec![
                    "Checkpoint Drafts: 1 document(s)".to_string(),
                    "busy".to_string(),
                ],
            },
            Refused("Checkpoint Drafts: 1 document(s) · busy".to_string()),
        ),
        (
            UpdateOutcome::Deferred {
                reason: "the terminal is busy".to_string(),
                hold: crate::native_update_lane_hold::LaneHold::Activity,
            },
            Refused("the terminal is busy".to_string()),
        ),
        (
            UpdateOutcome::InstalledNeedsRelaunch {
                build: 7,
                message: "activating in place".to_string(),
            },
            Silent,
        ),
    ] {
        assert_eq!(apply_ledger_verdict(&outcome), want, "{outcome:?}");
    }
}

/// Delayed events from retired checkers preserve the current progress and verification state.
#[test]
fn retired_progress_cannot_replace_the_live_download_or_verification() {
    use aterm_update::{Progress, ProgressReport, checker_watch::CheckerWatch};
    let _ledger = super::hold_update_ledger_for_test();
    let model = aterm_models_update::native_update_progress_delivery_model();
    let mut queued = model.init_state();
    assert!(model.fire("QueueCurrent", &mut queued));
    let watch = CheckerWatch::new();
    let retired = watch.register(100);
    let reports = [
        Progress::Downloading {
            version: "old".into(),
            bytes_done: 1,
            bytes_total: 10,
        },
        Progress::Verifying {
            version: "old".into(),
        },
        Progress::Staged {
            version: "old".into(),
            build: 1,
        },
        Progress::Failed {
            detail: "old failure".into(),
        },
        Progress::Deferred {
            detail: "old deferral".into(),
        },
        Progress::Withheld {
            detail: "old withdrawal".into(),
        },
    ]
    .map(|progress| ProgressReport {
        progress,
        checker_generation: Some(retired),
    });
    let live = watch.supersede(retired).unwrap();
    assert!(model.fire("Replace", &mut queued));
    let mut app = App::headless_for_test();
    app.clear_messages_for_test();
    let download = |version: &str, checker_generation| ProgressReport {
        progress: Progress::Downloading {
            version: version.into(),
            bytes_done: 5,
            bytes_total: 10,
        },
        checker_generation,
    };
    app.note_update_progress_report(download("current", Some(live)), &watch);
    let quiet = app.update_quiet.clone().expect("current download is shown");
    let record = app.update_record_text();
    for report in reports {
        let mut delivered = queued.clone();
        assert!(model.fire("Deliver", &mut delivered));
        assert_eq!(
            i64::from(report.is_current_for(&watch)),
            delivered["accepted"]
        );
        assert!(model.check_invariant("RetiredProgressIgnored", &delivered));
        app.note_update_progress_report(report, &watch);
        assert_eq!(app.update_quiet.as_ref(), Some(&quiet));
        assert_eq!(app.update_record_text(), record);
        assert!(app.update_verified.is_none());
    }
    let mut manual = model.init_state();
    for action in ["QueueManual", "Replace", "Deliver"] {
        assert!(model.fire(action, &mut manual));
    }
    let report = download("manual", None);
    assert_eq!(i64::from(report.is_current_for(&watch)), manual["accepted"]);
    app.note_update_progress_report(report, &watch);
    assert_eq!(app.update_quiet.as_ref().unwrap().version, "manual");
    let build = app.native_updater_service.snapshot().current_build + 1;
    let mut current = model.init_state();
    for action in ["Replace", "QueueCurrent", "Deliver"] {
        assert!(model.fire(action, &mut current));
    }
    assert_eq!(current["accepted"], 1);
    app.note_update_progress_report(
        ProgressReport {
            progress: Progress::Staged {
                version: "current".into(),
                build,
            },
            checker_generation: Some(live),
        },
        &watch,
    );
    assert_eq!(
        app.update_verified.map(|(verified, _)| verified),
        Some(build)
    );
    assert!(app.update_quiet.is_none());
    for (dial, actions, law) in [
        (
            1,
            ["QueueCurrent", "Replace", "Deliver"],
            "RetiredProgressIgnored",
        ),
        (
            2,
            ["QueueManual", "Replace", "Deliver"],
            "ManualProgressDelivered",
        ),
        (
            3,
            ["Replace", "QueueCurrent", "Deliver"],
            "CurrentProgressDelivered",
        ),
    ] {
        let mutant = aterm_spec::interp::with_buggy(&model, dial);
        let mut state = mutant.init_state();
        for action in actions {
            assert!(mutant.fire(action, &mut state));
        }
        assert!(!mutant.check_invariant(law, &state), "{law}");
    }
}

/// Verify `update_awaited` distinguishes user-initiated and `ctl` checks from background checks.
#[test]
fn a_download_is_the_persons_while_their_check_runs() {
    let download = |done: u64| aterm_update::Progress::Downloading {
        version: "0.79.0".into(),
        bytes_done: done,
        bytes_total: 90,
    };
    let mut app = App::headless_for_test();
    assert!(!app.update_awaited());
    app.note_update_progress(&download(45));
    assert_eq!(
        app.update_row_text(),
        None,
        "the background loop's is silent"
    );
    assert!(app.update_quiet.is_some(), "Software Update says it");
    let CheckStart::Start(_) = app.native_updater_service.request_check() else {
        panic!("a check starts");
    };
    assert!(app.update_awaited());
    app.note_update_progress(&download(60));
    assert!(
        app.update_row_text().is_some(),
        "the row is up for the person"
    );
    assert!(
        app.update_quiet.is_none(),
        "and the page's quiet line is gone"
    );
    let mut ctl = App::headless_for_test();
    ctl.note_update_check_asked(true);
    ctl.note_update_check_asked(true);
    ctl.note_update_check_asked(false);
    assert!(ctl.update_awaited(), "one of two still runs");
    ctl.note_update_progress(&download(10));
    assert!(ctl.update_row_text().is_some());
    ctl.note_update_check_asked(false);
    ctl.note_update_check_asked(false);
    assert!(!ctl.update_awaited(), "never below zero");
    ctl.note_update_progress(&download(80));
    assert!(
        ctl.update_row_text().is_some(),
        "the row already up keeps moving to its end"
    );
}

/// Quiet background download progress updates an open Software Update Settings view.
#[test]
fn the_quiet_download_repaints_an_open_settings_page() {
    let mut app = App::headless_for_test();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate));
    let (_, view) = app
        .active_native_view(WindowId(0))
        .expect("active Settings view");
    let presentation = |app: &App| match app.native_runtime.view_state(view) {
        Some(AppViewState::Settings(state)) => state.common.presentation_revision,
        _ => panic!("Settings view state"),
    };
    let before = presentation(&app);
    app.note_update_progress(&aterm_update::Progress::Downloading {
        version: "0.92.0".into(),
        bytes_done: 5_400_000,
        bytes_total: 1_200_000_000,
    });
    assert_eq!(app.update_row_text(), None, "silent");
    assert!(presentation(&app) > before, "the open page repaints");
    app.note_update_progress(&aterm_update::Progress::Staged {
        version: "0.92.0".into(),
        build: 41,
    });
    assert!(app.update_quiet.is_none());
}

#[test]
fn menu_check_route_joins_one_process_owned_check() {
    let mut app = App::headless_for_test();
    let ticket = match app.native_updater_service.request_check() {
        CheckStart::Start(ticket) => ticket,
        other => panic!("expected seeded in-flight check, got {other:?}"),
    };

    assert_eq!(
        app.open_software_update_route_and_check(),
        Ok(UpdateOutcome::Accepted)
    );
    assert_eq!(app.native_updater_service.snapshot().active, Some(ticket));
    assert_eq!(app.native_updater_service.snapshot().generation, 1);
    assert_eq!(
        app.open_software_update_route_and_check(),
        Ok(UpdateOutcome::Accepted)
    );
    assert_eq!(app.native_updater_service.snapshot().active, Some(ticket));
    assert_eq!(app.native_updater_service.snapshot().generation, 1);
}

/// `ensure_staged_decision_in` raises the ready row at most once per `(build, posture)`.
#[test]
fn a_stopped_lane_raises_the_staged_decision_once() {
    use crate::update_words::ApplyPosture;
    let mut app = App::headless_for_test();
    let build = stage_one_build(&mut app);
    app.ensure_staged_decision_in(build + 1, ApplyPosture::ManualByConfig);
    assert_eq!(app.messages.live_rows().count(), 0);
    for posture in [
        ApplyPosture::Automatic,
        ApplyPosture::ManualOnlyLatched { lapses: true },
    ] {
        app.ensure_staged_decision_in(build, posture);
        assert_eq!(
            app.messages.live_rows().count(),
            0,
            "{posture:?}: the lane still acts, so nothing is asked"
        );
    }
    app.ensure_staged_decision_in(build, ApplyPosture::ManualOnlyLatched { lapses: false });
    let id = app.staged_update_row().expect("the decision row");
    app.ensure_staged_decision_in(build, ApplyPosture::ManualOnlyLatched { lapses: false });
    app.ensure_staged_decision_in(build, ApplyPosture::ManualByConfig);
    assert_eq!(app.messages.live_rows().count(), 1, "raised once");
    assert_eq!(app.staged_update_row(), Some(id));
    let row = app.messages.live(id).expect("live");
    assert_eq!(
        row.msg.actions,
        vec![aterm_messages::Intent::ApplyUpdate { build }]
    );
    let lapsed = std::time::Instant::now() + aterm_messages::HOLD_STAGED_MANUAL * 2;
    app.messages.settle(lapsed, true);
    app.sync_messages();
    assert_eq!(app.staged_update_row(), None, "the question lapsed");
    for _ in 0..3 {
        app.ensure_staged_decision_in(build, ApplyPosture::ManualOnlyLatched { lapses: false });
    }
    assert_eq!(
        app.staged_update_row(),
        None,
        "a reconcile never asks again"
    );
    app.ensure_staged_decision_in(build, ApplyPosture::ManualByConfig);
    assert!(
        app.staged_update_row().is_some(),
        "a new posture asks again"
    );
}

/// When no stage remains, an automatic failure is log-only while a manual press posts `UPDATE_DIDNT_FINISH`.
#[test]
fn an_automatic_failure_with_no_stage_is_a_record_and_a_press_is_a_row() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let failed = || UpdateOutcome::Failed {
        message: "the staged artifact was retired".to_string(),
    };
    let mut app = App::headless_for_test();
    assert!(app.native_updater_service.snapshot().staged.is_none());
    let before = app.messages.log().len();
    app.react_to_update_apply_outcome("automatic handoff", failed(), false);
    assert_eq!(
        app.update_row_text(),
        None,
        "no row for an unattended attempt"
    );
    assert_eq!(app.messages.log().len(), before + 1, "one record");
    assert!(app.update_record_text().is_some(), "on record");

    let mut pressed = App::headless_for_test();
    pressed.react_to_update_apply_outcome("manual", failed(), false);
    assert!(
        pressed.update_row_text().is_some(),
        "a person's press is told on the glass"
    );
}

/// `InstalledNeedsRelaunch` resolves the staged row `Ok`, while `Failed` with no stage withdraws it.
#[test]
fn a_gone_artifact_never_echoes_installed_beside_its_failure() {
    use aterm_messages::{LogState, Outcome, Retired};
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let build = 41;
    for (outcome, retired) in [
        (
            UpdateOutcome::InstalledNeedsRelaunch {
                build,
                message: "activation pending".to_string(),
            },
            Retired::Resolved(Outcome::Ok),
        ),
        (
            UpdateOutcome::Failed {
                message: "the staged artifact was retired".to_string(),
            },
            Retired::Withdrawn,
        ),
    ] {
        let mut app = App::headless_for_test();
        assert!(
            app.native_updater_service.snapshot().staged.is_none(),
            "PRECONDITION: no staged build"
        );
        let id = app.post_update_row(
            crate::update_words::staged(
                "9.9.9",
                build,
                Some(crate::update_words::ApplyPosture::ManualByConfig),
            ),
            "9.9.9",
            crate::messages_host::FlowPhase::Staged { build },
        );
        assert!(
            app.staged_update_row().is_some(),
            "PRECONDITION: staged row"
        );
        app.react_to_update_apply_outcome("automatic handoff", outcome.clone(), false);
        assert_eq!(app.staged_update_row(), None, "{outcome:?}: the row left");
        assert_eq!(
            app.messages.log().get(id).map(|r| r.state.clone()),
            Some(LogState::Retired(retired)),
            "{outcome:?}"
        );
    }
}
