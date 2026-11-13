// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
#[test]
fn paused_app_retires_the_actual_unclaimed_constructor_and_releases_its_gate() {
    let (root, sources, lifetime) = crate::task_launch::pending_codex_constructor_for_drain();
    let gate = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join("home/.aterm/codex-source-custody/gate"))
        .unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "observe the actual pending constructor's kernel exclusion"
    )]
    let before = gate.try_lock();
    assert!(matches!(before, Err(std::fs::TryLockError::WouldBlock)));
    let pause = lifetime
        .pause()
        .unwrap()
        .try_wait()
        .expect_err("the real constructor still owns its App scope");
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let worker_sources = sources.clone();
    let worker_lifetime = lifetime.clone();
    let worker = std::thread::spawn(move || {
        send.send(drain_account_sources(
            pause,
            Some(&worker_sources),
            &worker_lifetime,
        ))
        .unwrap();
    });
    let drained = receive
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("cancelled unclaimed constructor must retire");
    worker.join().unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "verify revocation released the actual constructor gate"
    )]
    gate.try_lock().unwrap();
    assert_eq!(sources.revoke_cancelled_unclaimed().unwrap(), 0);
    drained.resume().unwrap();
    assert!(lifetime.enter().is_some());
}

#[test]
fn source_cleanup_does_not_retire_other_owned_account_work() {
    for registered in [false, true] {
        let lifetime = aterm_agent::harness::account_work::Lifetime::default();
        let work = lifetime.enter().unwrap();
        let pause = lifetime.pause().unwrap();
        assert!(work.cancelled());
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let worker_lifetime = lifetime.clone();
        let worker = std::thread::spawn(move || {
            let sources =
                registered.then(aterm_agent::harness::codex_source_registry::Registry::default);
            let drained = drain_account_sources(pause, sources.as_ref(), &worker_lifetime);
            send.send(drained).unwrap();
        });
        assert!(matches!(
            receive.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        assert!(lifetime.enter().is_none());
        drop(work);
        let drained = receive
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("actual owned work retired");
        worker.join().unwrap();
        drained.resume().unwrap();
        assert!(lifetime.enter().is_some());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn activity_during_account_retirement_reaches_the_fresh_handoff_preflight() {
    if !crate::test_alone::alone() {
        return;
    }
    use crate::native_updater_service::{ApplyAttemptTicket, ApplyMode};
    let mut app = App::headless_for_test();
    let attempt = ApplyAttemptTicket::for_test(42, "committed-build", &"ab".repeat(32));
    attempt.make_current_apply_for_test(&mut app.native_updater_service);
    let work = app.account_work.enter().unwrap();
    assert!(!app.accounts_drained_before(Continuation::Handoff {
        mode: ApplyMode::AutomaticPastGrace,
        attempt: Some(attempt),
        same_image: None,
        next_attempt: app.next_update_handoff_id,
    }));
    app.note_update_handoff_activity();
    // A real blocker appearing while the accounts retire must be read now, rather
    // than discarding the request merely because the earlier activity counter moved. A
    // Command Line Tools install's tab is that blocker, named as its own hold
    // (`LaneHold::DeveloperTools`).
    app.land_install_session_for_test(0);
    drop(work);
    let drained = app.take_retired_account_pause().unwrap();
    let handoff = app
        .accept_account_drain(app.account_drain.serial, drained)
        .unwrap();
    app.finish_account_handoff(handoff);
    assert_eq!(
        app.native_updater_service.snapshot().error.as_deref(),
        Some("a Mac Setup tab is still running")
    );
    assert!(
        app.auto_apply_intent.is_some(),
        "the automatic request remains owed"
    );
    assert!(
        app.account_work.enter().is_some(),
        "the deferred handoff reopened account work"
    );
}

#[cfg(unix)]
#[test]
fn a_handoff_or_artifact_that_superseded_the_drain_is_never_resumed() {
    use crate::native_updater_service::{ApplyAttemptTicket, ApplyMode};
    for new_artifact in [false, true] {
        let mut app = App::headless_for_test();
        let attempt = ApplyAttemptTicket::for_test(42, "committed-build", &"ab".repeat(32));
        attempt.make_current_apply_for_test(&mut app.native_updater_service);
        let work = app.account_work.enter().unwrap();
        assert!(!app.accounts_drained_before(Continuation::Handoff {
            mode: ApplyMode::AutomaticPastGrace,
            attempt: Some(attempt.clone()),
            same_image: None,
            next_attempt: app.next_update_handoff_id,
        }));
        let newer = ApplyAttemptTicket::for_test(43, "newer-build", &"cd".repeat(32));
        if new_artifact {
            newer.make_current_apply_for_test(&mut app.native_updater_service);
        } else {
            app.next_update_handoff_id += 1;
        }
        drop(work);
        let drained = app.take_retired_account_pause().unwrap();
        let handoff = app
            .accept_account_drain(app.account_drain.serial, drained)
            .unwrap();
        app.finish_account_handoff(handoff);
        assert!(app.auto_apply_intent.is_none());
        assert!(app.account_work.enter().is_some());
        if new_artifact {
            assert!(app.native_updater_service.apply_ticket_is_current(&newer));
        } else {
            assert!(!app.native_updater_service.apply_ticket_is_current(&attempt));
        }
    }
}

#[cfg(unix)]
#[test]
fn a_late_account_deferral_retries_only_the_automatic_request_and_keeps_its_artifact() {
    use crate::native_app::UpdateOutcome;
    use crate::native_updater_service::{ApplyAttemptTicket, ApplyMode, UpdaterPhase};

    for mode in [ApplyMode::AutomaticPastGrace, ApplyMode::Immediate] {
        let mut app = App::headless_for_test();
        let digest = "ab".repeat(32);
        let attempt = ApplyAttemptTicket::for_test(42, "committed-build", &digest);
        attempt.make_current_apply_for_test(&mut app.native_updater_service);
        let outcome = app.account_update_start_outcome(
            &attempt,
            mode,
            crate::UpdateHandoffStartError::activity(
                crate::native_update_lane_hold::LaneHold::TabStarting,
                "a tab is still starting",
            ),
        );
        assert_eq!(
            outcome,
            UpdateOutcome::Deferred {
                reason: "a tab is still starting".into(),
                hold: crate::native_update_lane_hold::LaneHold::TabStarting,
            }
        );
        let snapshot = app.native_updater_service.snapshot();
        assert_eq!(snapshot.phase, UpdaterPhase::Staged);
        assert_eq!(snapshot.error.as_deref(), Some("a tab is still starting"));
        assert_eq!(snapshot.staged.as_ref().unwrap().dmg_sha256, digest);
        if mode.is_automatic() {
            let retry = app
                .auto_apply_intent
                .expect("the accepted automatic request remains owed");
            assert_eq!(retry.build, 42);
            assert_eq!(retry.dmg_sha256, [0xab; 32]);
        } else {
            assert!(app.auto_apply_intent.is_none());
        }
    }
}

#[test]
fn a_late_account_refusal_or_failure_keeps_its_verdict() {
    use crate::native_app::UpdateOutcome;
    use crate::native_updater_service::{ApplyAttemptTicket, ApplyMode};

    for (error, expected) in [
        (
            crate::UpdateHandoffStartError::refused("draft changed"),
            UpdateOutcome::Blocked {
                reasons: vec!["draft changed".into()],
            },
        ),
        (
            crate::UpdateHandoffStartError::failed("launch failed"),
            UpdateOutcome::Failed {
                message: "launch failed".into(),
            },
        ),
    ] {
        let mut app = App::headless_for_test();
        let attempt = ApplyAttemptTicket::for_test(42, "committed-build", &"ab".repeat(32));
        attempt.make_current_apply_for_test(&mut app.native_updater_service);
        assert_eq!(
            app.account_update_start_outcome(&attempt, ApplyMode::Immediate, error),
            expected
        );
        assert!(app.auto_apply_intent.is_none());
    }
}

#[cfg(unix)]
#[test]
fn a_late_preflight_block_keeps_a_bounded_retry_for_the_same_artifact() {
    use crate::native_app::UpdateOutcome;
    use crate::native_updater_service::{ApplyAttemptTicket, ApplyMode};
    let mut app = App::headless_for_test();
    let attempt = ApplyAttemptTicket::for_test(42, "committed-build", &"ab".repeat(32));
    attempt.make_current_apply_for_test(&mut app.native_updater_service);
    assert_eq!(
        app.account_update_start_outcome(
            &attempt,
            ApplyMode::AutomaticPastGrace,
            crate::UpdateHandoffStartError::refused("draft changed"),
        ),
        UpdateOutcome::Blocked {
            reasons: vec!["draft changed".into()]
        },
    );
    let retry = app.auto_apply_intent.unwrap();
    assert_eq!(retry.build, 42);
    assert_eq!(retry.dmg_sha256, [0xab; 32]);
    app.auto_overlap_retry.as_mut().unwrap().cycles = u8::MAX;
    let cooldown = app.arm_account_preflight_retry(&attempt).unwrap();
    assert!(cooldown > std::time::Duration::ZERO);
    assert!(cooldown <= std::time::Duration::from_secs(60));
    app.auto_apply_intent = None;
    attempt.make_current_apply_for_test(&mut app.native_updater_service);
    let _ = app.account_update_start_outcome(
        &attempt,
        ApplyMode::AutomaticPastGrace,
        crate::UpdateHandoffStartError::failed("launch failed"),
    );
    assert!(
        app.auto_apply_intent.is_none(),
        "a real failure is not a cheap preflight retry"
    );
}

#[test]
fn unavailable_retirement_metadata_does_not_turn_real_resource_retirement_into_a_quit_stall() {
    let mut app = App::headless_for_test();
    let work = app.account_work.enter().unwrap();
    assert!(!app.accounts_drained_before(Continuation::Quit));
    drop(work);
    let drained = app.take_retired_account_pause().unwrap();
    let serial = app.account_drain.serial;
    assert!(matches!(
        app.accept_account_settlement(
            serial,
            Settlement {
                drained,
                sealed: false
            }
        ),
        Some(Continuation::Quit)
    ));
    assert!(app.account_drain.drained.is_some());
    assert!(!app.account_drain.failed);
    assert!(!app.account_drain.retirement_pending);
    assert!(app.account_drain.worker_retry_at.is_none());
    assert!(app.account_work.enter().is_none());
    app.resume_drained_accounts();
    assert!(app.account_work.enter().is_some());
}

#[test]
fn an_empty_quit_closes_admission_without_a_worker_or_completion_channel() {
    let mut app = App::headless_for_test();
    assert!(app.proxy.is_none());
    assert!(app.account_work.enter().is_some());
    assert_eq!(app.quit_barriers(), crate::app_window::QuitBarrier::Ready);
    assert!(app.account_work.enter().is_none());
    assert!(app.account_drain.drained.is_some());
    assert!(!app.account_drain.failed);
}

#[test]
fn a_busy_last_window_stays_logically_present_without_settlement_delivery() {
    let mut app = App::headless_for_test();
    let work = app.account_work.enter().unwrap();
    let sessions = app
        .pool
        .iter()
        .map(|session| session.id)
        .collect::<Vec<_>>();
    assert!(matches!(
        app.close_window_logical(WindowId(0)),
        CloseOutcome::Stay
    ));
    assert!(work.cancelled());
    assert!(app.windows.contains_key(&WindowId(0)));
    assert_eq!(
        app.pool
            .iter()
            .map(|session| session.id)
            .collect::<Vec<_>>(),
        sessions
    );
    assert!(app.account_drain.drained.is_none());
    drop(work);
    // The retained Pause can prove actual settlement on the next request, without a worker.
    assert!(app.account_work.enter().is_none());
    assert!(app.account_drain.pause.is_some());
    // Exercise the event loop's nonblocking retirement path, with no second quit request.
    let drained = app.take_retired_account_pause().unwrap();
    let serial = app.account_drain.serial;
    assert!(matches!(
        app.accept_account_drain(serial, drained),
        Some(Continuation::Window(WindowId(0), _))
    ));
    app.resume_drained_accounts();
    assert!(app.account_work.enter().is_some());
    assert!(!app.account_drain.failed);
}

#[test]
fn initial_surface_failure_waits_for_actual_account_retirement() {
    for busy in [false, true] {
        let mut app = App::headless_for_test();
        let work = busy.then(|| app.account_work.enter().unwrap());
        assert_eq!(app.initial_surface_exit_ready(), !busy);
        assert!(app.account_work.enter().is_none());
        if let Some(work) = work {
            assert!(work.cancelled());
            assert!(app.take_retired_account_pause().is_none());
            drop(work);
            let drained = app.take_retired_account_pause().unwrap();
            let serial = app.account_drain.serial;
            assert!(matches!(
                app.accept_account_drain(serial, drained),
                Some(Continuation::StartupFailure)
            ));
            assert!(app.initial_surface_exit_ready());
        }
        assert!(app.account_work.enter().is_none());
    }
}

#[test]
fn a_refused_last_window_close_does_not_enter_the_account_gate() {
    use crate::native_app::{ActionInvocation, AppEvent, TextInputEvent};
    let mut app = App::headless_for_test();
    let window = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    let (_, settings) = app.active_native_view(window).unwrap();
    for event in [
        AppEvent::Action(ActionInvocation {
            id: crate::native_ui::ActionId::new(format!(
                "settings/set/{}",
                crate::prefs::EDIT_FONT_FAMILY
            )),
            value: None,
        }),
        AppEvent::TextInput(TextInputEvent::SelectAll),
        AppEvent::TextInput(TextInputEvent::Commit("Inert Draft Mono".into())),
    ] {
        app.dispatch_native_view_event(window, settings, event)
            .unwrap();
    }
    let work = app.account_work.enter().unwrap();
    assert!(matches!(
        app.close_window_through_barriers(window),
        CloseOutcome::Stay
    ));
    assert!(!work.cancelled());
    assert!(app.account_work.enter().is_some());
    assert!(app.account_drain.drained.is_none());
    assert!(app.windows.contains_key(&window));
}

#[test]
fn a_refused_busy_close_disarms_its_exit_even_if_the_draft_is_resolved_before_retirement() {
    use crate::native_app::{ActionInvocation, AppEvent, TextInputEvent};
    for (window_close, rearm) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut app = App::headless_for_test();
        let work = app.account_work.enter().unwrap();
        if window_close {
            assert!(matches!(
                app.close_window_through_barriers(WindowId(0)),
                CloseOutcome::Stay
            ));
            assert!(matches!(
                app.account_drain.pending,
                Some(Continuation::Window(..))
            ));
        } else {
            assert_eq!(app.quit_barriers(), crate::app_window::QuitBarrier::Saving);
            assert!(matches!(
                app.account_drain.pending,
                Some(Continuation::Quit)
            ));
        }
        let window = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        let (_, settings) = app.active_native_view(window).unwrap();
        for event in [
            AppEvent::Action(ActionInvocation {
                id: crate::native_ui::ActionId::new(format!(
                    "settings/set/{}",
                    crate::prefs::EDIT_FONT_FAMILY
                )),
                value: None,
            }),
            AppEvent::TextInput(TextInputEvent::SelectAll),
            AppEvent::TextInput(TextInputEvent::Commit("Inert Draft Mono".into())),
        ] {
            app.dispatch_native_view_event(window, settings, event)
                .unwrap();
        }
        if window_close {
            assert!(matches!(
                app.close_window_through_barriers(window),
                CloseOutcome::Stay
            ));
        } else {
            assert_eq!(app.quit_barriers(), crate::app_window::QuitBarrier::Refused);
        }
        assert!(matches!(
            app.account_drain.pending,
            Some(Continuation::ResumeAfterDrain)
        ));
        // Resolving the draft does not revive the refused request.
        for _ in 0..2 {
            app.dispatch_native_view_event(
                window,
                settings,
                AppEvent::Action(ActionInvocation {
                    id: crate::native_ui::ActionId::new("settings/drafts/discard-all"),
                    value: None,
                }),
            )
            .unwrap();
        }
        assert!(app.prepare_quit_native_shutdown().unwrap());
        assert!(app.account_work.enter().is_none());
        if rearm {
            if window_close {
                assert!(matches!(
                    app.close_window_through_barriers(window),
                    CloseOutcome::Stay
                ));
            } else {
                assert_eq!(app.quit_barriers(), crate::app_window::QuitBarrier::Saving);
            }
        }
        drop(work);
        let drained = app.take_retired_account_pause().unwrap();
        let serial = app.account_drain.serial;
        let continuation = app.accept_account_drain(serial, drained);
        if rearm {
            if window_close {
                assert!(matches!(
                    continuation,
                    Some(Continuation::Window(WindowId(0), _))
                ));
            } else {
                assert!(matches!(continuation, Some(Continuation::Quit)));
            }
            assert!(app.account_work.enter().is_none());
            assert_eq!(app.quit_barriers(), crate::app_window::QuitBarrier::Ready);
        } else {
            assert!(
                continuation.is_none(),
                "the cancelled completion cannot execute Quit"
            );
            assert!(app.account_work.enter().is_some());
        }
    }
}

#[cfg(unix)]
#[test]
fn a_busy_update_cannot_start_any_successor_without_settlement_delivery() {
    let mut app = App::headless_for_test();
    let work = app.account_work.enter().unwrap();
    let attempt = app.next_update_handoff_id;
    let result = app.apply_staged_update_now(
        crate::app_native::NativeUpdateSafetyToken::carrying_for_test(0, 0),
        crate::native_updater_service::ApplyMode::Immediate,
        None,
        None,
    );
    assert!(result.is_err());
    assert!(work.cancelled());
    assert_eq!(app.next_update_handoff_id, attempt);
    assert!(app.pending_update_handoff.is_none());
    assert!(
        matches!(
            app.account_drain.pending,
            Some(Continuation::ResumeAfterDrain)
        ),
        "the caller heard it failed, so the handoff does not start when the work ends"
    );
    assert!(app.account_drain.drained.is_none());
    assert!(app.account_work.enter().is_none());
}

/// A WORKER THAT PANICS HOLDS NOTHING: its pause dies with it, so no settlement comes, and what
/// waited stayed held for the process's life. CONTROL: one that settles says so.
#[test]
fn a_worker_that_panics_lets_what_waited_go_on() {
    assert!(matches!(
        super::worker_answer(7, || panic!("settling")),
        Wake::AccountsOutlived { serial: 7 }
    ));
    let mut app = App::headless_for_test();
    let work = app.account_work.enter().unwrap();
    assert!(!app.accounts_drained_before(Continuation::Quit));
    drop(work);
    let drained = app.take_retired_account_pause().unwrap();
    assert!(matches!(
        super::worker_answer(7, || Settlement {
            drained,
            sealed: true
        }),
        Wake::AccountsDrained { serial: 7, .. }
    ));
}

/// THE BOUND SPEAKS ONLY OF A WORKER STILL GOING: past it what waited goes on, and a worker that
/// ended says nothing, whenever it ended.
#[test]
fn the_bound_speaks_only_of_a_worker_still_going() {
    let (worker_lives, worker_ended) = std::sync::mpsc::channel::<()>();
    assert!(super::outlives_account_work_bound(
        &worker_ended,
        Duration::from_millis(1)
    ));
    drop(worker_lives);
    assert!(!super::outlives_account_work_bound(
        &worker_ended,
        aterm_spec::wait::MUST_HAPPEN
    ));
}

/// ACCOUNT WORK THAT OUTLIVES ITS BOUND HOLDS NOTHING ELSE: what waited goes on, once, and what
/// comes later waits no longer, but no new account work starts beside the stuck one, and its
/// retirement still reopens admissions. CONTROL: another drain's bound moves nothing.
#[test]
fn account_work_that_outlives_its_bound_lets_a_quit_go_on_and_keeps_its_epoch() {
    let mut app = App::headless_for_test();
    let work = app.account_work.enter().unwrap();
    assert!(!app.accounts_drained_before(Continuation::Quit));
    let serial = app.account_drain.serial;
    assert!(app.take_outlived_continuation(serial + 1).is_none());
    assert!(app.account_drain_pending(), "CONTROL: it still waits");
    assert!(matches!(
        app.take_outlived_continuation(serial),
        Some(Continuation::Quit)
    ));
    assert!(
        app.take_outlived_continuation(serial).is_none(),
        "it goes on once"
    );
    assert!(!app.account_drain_pending());
    assert!(app.accounts_drained_before(Continuation::Quit));
    assert_eq!(app.quit_barriers(), crate::app_window::QuitBarrier::Ready);
    assert!(
        app.account_work.enter().is_none(),
        "no new account work beside the stuck one"
    );
    drop(work);
    let drained = app.take_retired_account_pause().unwrap();
    assert!(app.accept_account_drain(serial, drained).is_none());
    assert!(!app.account_drain.outlived);
    assert!(
        app.account_work.enter().is_some(),
        "its retirement reopens admissions"
    );
}

/// The bound a drain only the event loop watches is read against. CONTROL: with nothing draining
/// no clock is overdue.
#[test]
fn a_drain_the_event_loop_watches_is_overdue_only_past_its_bound() {
    let mut app = App::headless_for_test();
    let _work = app.account_work.enter().unwrap();
    let far = Instant::now() + 2 * ACCOUNT_WORK_RETIRES_WITHIN;
    assert!(!app.account_drain_overdue(far), "CONTROL: nothing drains");
    assert!(!app.accounts_drained_before(Continuation::Quit));
    let since = app.account_drain.since.unwrap();
    let bound = since + ACCOUNT_WORK_RETIRES_WITHIN;
    assert!(!app.account_drain_overdue(bound - Duration::from_millis(1)));
    assert!(app.account_drain_overdue(bound));
}

/// AN APPLY WHILE A RENEWAL WAITS ON ACCOUNT WORK IS DEFERRED, NOT REFUSED: the renewal's handoff is
/// in flight, and an update follows it, as it does once that handoff has started. A refusal told a
/// person's press the update failed, and counted against the automatic attempt. CONTROL: an update's
/// own wait still refuses.
#[cfg(target_os = "macos")]
#[test]
fn an_apply_while_a_renewal_waits_on_account_work_is_deferred() {
    use crate::app_update_handoff::SameImageHandoff;
    let waiting = |same_image| {
        let mut app = App::headless_for_test();
        let work = app.account_work.enter().unwrap();
        assert!(!app.accounts_drained_before(Continuation::Handoff {
            mode: ApplyMode::Automatic,
            attempt: None,
            same_image,
            next_attempt: app.next_update_handoff_id,
        }));
        (app, work)
    };
    let apply = |app: &mut App, mode| {
        app.apply_staged_update_now(
            crate::app_native::NativeUpdateSafetyToken::carrying_for_test(0, 0),
            mode,
            None,
            None,
        )
    };
    let (mut app, _work) = waiting(Some(SameImageHandoff::RenewLaunchRecord));
    assert!(matches!(
        apply(&mut app, ApplyMode::Automatic),
        Err(crate::UpdateHandoffStartError::ActivityDeferred(..))
    ));
    assert!(app.account_drain_holds_a_renewal(), "the lane follows it");
    assert!(
        apply(&mut app, ApplyMode::Immediate).is_ok(),
        "a person's press has no lane to follow it, so it takes the renewal's place"
    );
    assert!(!app.account_drain_holds_a_renewal());
    assert!(matches!(
        app.account_drain_pending_for_test(),
        Some(Continuation::Handoff {
            mode: ApplyMode::Immediate,
            same_image: None,
            ..
        })
    ));
    for same_image in [None, Some(SameImageHandoff::DebugSeam)] {
        for mode in [ApplyMode::Automatic, ApplyMode::Immediate] {
            let (mut app, _work) = waiting(same_image);
            assert!(matches!(
                apply(&mut app, mode),
                Err(crate::UpdateHandoffStartError::Refused(_))
            ));
        }
    }
}

/// A handoff waiting on stuck account work starts past the bound, and one that comes later no
/// longer meets the drain. CONTROL: before the bound the drain refuses it.
#[cfg(unix)]
#[test]
fn an_update_goes_on_past_account_work_that_outlives_its_bound() {
    let mut app = App::headless_for_test();
    let _work = app.account_work.enter().unwrap();
    let waiting = Continuation::Handoff {
        mode: ApplyMode::Immediate,
        attempt: None,
        same_image: None,
        next_attempt: app.next_update_handoff_id,
    };
    assert!(!app.accounts_drained_before(waiting));
    assert!(app.account_drain_holds_a_handoff());
    let apply = |app: &mut App| {
        app.apply_staged_update_now(
            crate::app_native::NativeUpdateSafetyToken::carrying_for_test(0, 0),
            ApplyMode::Immediate,
            None,
            None,
        )
        .expect_err("the fixture supplies no replacement authority")
        .into_message()
    };
    let refused = apply(&mut app);
    assert!(refused.contains("account work"), "CONTROL: {refused}");
    let serial = app.account_drain.serial;
    assert!(matches!(
        app.take_outlived_continuation(serial),
        Some(Continuation::Handoff { .. })
    ));
    assert!(!app.account_drain_holds_a_handoff());
    let goes_on = apply(&mut app);
    assert!(!goes_on.contains("account work"), "{goes_on}");
    assert!(
        app.account_work.enter().is_none(),
        "account work stays off while it is stuck"
    );
}

fn attempt_for_test(build: u64) -> AutoApplyAttempt {
    AutoApplyAttempt {
        intent: crate::AutoApplyIntent {
            build,
            dmg_sha256: [0; 32],
            retry_at: Instant::now(),
            attempts: 0,
            deadline_owner: crate::AutoApplyDeadlineOwner::Ordinary,
            held: crate::native_update_lane_hold::NamedHold::default(),
        },
        phase: crate::native_update_auto_intent::ApplyPhase::PreferIdle,
        announce: false,
        activation: false,
    }
}

/// An automatic attempt's answer waits only while a handoff waits on account work, and only for
/// one that started it; a refusal has already answered. CONTROL: with no drain it is answered now.
#[cfg(unix)]
#[test]
fn an_automatic_attempt_waits_for_its_answer_only_while_its_handoff_waits_on_account_work() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = app.native_updater_service.snapshot().current_build + 1;
    let deferred = UpdateOutcome::Deferred {
        reason: "not now".to_string(),
        hold: crate::native_update_lane_hold::LaneHold::Activity,
    };
    app.settle_or_park_auto_apply(attempt_for_test(build), UpdateOutcome::Accepted);
    assert!(app.account_drain.parked_auto_apply.is_none(), "CONTROL");
    let _work = app.account_work.enter().unwrap();
    let waiting = Continuation::Handoff {
        mode: ApplyMode::Automatic,
        attempt: None,
        same_image: None,
        next_attempt: app.next_update_handoff_id,
    };
    assert!(!app.accounts_drained_before(waiting));
    app.settle_or_park_auto_apply(attempt_for_test(build), deferred);
    assert!(app.account_drain.parked_auto_apply.is_none());
    assert!(app.auto_apply_intent.is_some(), "the refusal kept it");
    app.auto_apply_intent = None;
    app.settle_or_park_auto_apply(attempt_for_test(build), UpdateOutcome::Accepted);
    assert!(app.account_drain.parked_auto_apply.is_some());
    assert!(app.auto_apply_intent.is_none(), "no answer yet");
}

/// AN AUTOMATIC ATTEMPT THAT WAITED ON ACCOUNT WORK IS ANSWERED AS ONE THAT DID NOT: it was told
/// nothing when it began, so a handoff that could not start must keep the lane's promise, and
/// losing it would leave a ready stage until some other reconcile.
#[test]
fn an_automatic_attempt_is_answered_after_account_work_as_a_direct_one_is() {
    let build_of = |app: &App| app.native_updater_service.snapshot().current_build + 1;
    for (outcome, keeps) in [
        (
            UpdateOutcome::Deferred {
                reason: "activity".to_string(),
                hold: crate::native_update_lane_hold::LaneHold::Activity,
            },
            true,
        ),
        (
            UpdateOutcome::Blocked {
                reasons: vec!["a tab is starting".to_string()],
            },
            true,
        ),
        (UpdateOutcome::Accepted, false),
    ] {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = build_of(&app);
        app.park_auto_apply_for_accounts(attempt_for_test(build));
        app.answer_handoff_after_accounts(
            ApplyMode::Automatic,
            Some(build),
            false,
            outcome.clone(),
        );
        assert!(app.account_drain.parked_auto_apply.is_none());
        assert_eq!(app.auto_apply_intent.is_some(), keeps, "{outcome:?}");
    }
}

/// AN ATTEMPT THAT JOINED A PERSON'S PRESS COUNTS NOTHING OF ITS FAILURE: the install that did not
/// start was the press's, so the lane looks again on its quiet cadence and spends no retry.
#[test]
fn an_attempt_that_joined_a_press_counts_nothing_of_its_failure() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = app.native_updater_service.snapshot().current_build + 1;
    app.park_auto_apply_for_accounts(attempt_for_test(build));
    app.answer_handoff_after_accounts(
        ApplyMode::Immediate,
        Some(build),
        false,
        UpdateOutcome::Failed {
            message: "the desk refused this install".to_string(),
        },
    );
    assert!(app.account_drain.parked_auto_apply.is_none());
    let intent = app.auto_apply_intent.expect("it looks again");
    assert_eq!(intent.build, build);
    assert_eq!(intent.attempts, 0, "no retry spent on the press's failure");
}

/// An attempt a quit retired looks again, in case that quit is refused. CONTROL: what is not a
/// handoff answers nothing.
#[test]
fn a_handoff_a_quit_retired_gives_its_attempt_back() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = app.native_updater_service.snapshot().current_build + 1;
    app.park_auto_apply_for_accounts(attempt_for_test(build));
    app.retire_account_drain_handoff(Continuation::Quit, "shutdown");
    assert!(app.account_drain.parked_auto_apply.is_some(), "CONTROL");
    app.retire_account_drain_handoff(
        Continuation::Handoff {
            mode: ApplyMode::Automatic,
            attempt: None,
            same_image: None,
            #[cfg(unix)]
            next_attempt: 0,
        },
        "shutdown",
    );
    assert!(app.account_drain.parked_auto_apply.is_none());
    assert!(
        app.auto_apply_intent
            .is_some_and(|intent| intent.build == build)
    );
}

#[cfg(unix)]
#[test]
fn a_quit_wait_does_not_accept_an_unqueued_update_request() {
    let mut app = App::headless_for_test();
    let _work = app.account_work.enter().unwrap();
    assert!(!app.accounts_drained_before(Continuation::Quit));
    assert!(
        app.apply_staged_update_now(
            crate::app_native::NativeUpdateSafetyToken::carrying_for_test(0, 0),
            crate::native_updater_service::ApplyMode::Immediate,
            None,
            None,
        )
        .is_err()
    );
    assert!(matches!(
        app.account_drain.pending,
        Some(Continuation::Quit)
    ));
    assert!(app.pending_update_handoff.is_none());
}

#[cfg(unix)]
#[test]
fn only_the_matching_returned_handoff_can_release_its_drained_epoch() {
    let mut app = App::headless_for_test();
    assert!(app.accounts_drained_before(Continuation::Quit));
    app.bind_account_handoff(7);
    app.returned_account_handoff(6, false);
    assert!(app.account_work.enter().is_none());
    app.returned_account_handoff(7, false);
    assert!(app.account_work.enter().is_some());
    assert!(app.accounts_drained_before(Continuation::Quit));
    app.bind_account_handoff(8);
    app.returned_account_handoff(7, false);
    assert!(app.account_work.enter().is_none());
    // A matching completion hands its closed scope directly to deferred Quit.
    app.returned_account_handoff(8, true);
    assert!(app.account_work.enter().is_none());
    app.resume_drained_accounts();
    assert!(app.account_work.enter().is_some());
}

#[test]
fn incoming_successor_clones_stay_closed_until_its_commit_releases_them() {
    let mut app = App::headless_for_test();
    app.account_drain = Drain::for_launch(&app.account_work, true);
    let host = app.account_work.clone();
    let menu = app.account_work.clone();
    assert!(host.enter().is_none());
    assert!(menu.enter().is_none());
    assert!(app.account_drain.drained.is_some());
    app.resume_drained_accounts();
    assert!(host.enter().is_some());
    assert!(menu.enter().is_some());
}
#[test]
fn a_sweep_nothing_waits_on_stops_waking_a_hundred_times_a_second() {
    let waiting = source_sweep_waits(ACCOUNT_WORK_RETIRES_WITHIN - Duration::from_secs(1));
    let past = source_sweep_waits(ACCOUNT_WORK_RETIRES_WITHIN);
    assert_eq!(waiting, Duration::from_millis(10));
    assert_eq!(past, Duration::from_secs(1));
}

#[test]
fn unavailable_drain_worker_retries_without_losing_custody_or_reopening_admission() {
    let mut app = App::headless_for_test();
    assert!(app.proxy.is_none());
    let work = app.account_work.enter().unwrap();
    assert!(!app.accounts_drained_before(Continuation::ResumeAfterDrain));
    let serial = app.account_drain.serial;
    let due = app
        .account_drain
        .worker_retry_at
        .expect("unavailable dispatch is retried");
    assert!(work.cancelled());
    assert!(!app.retry_account_drain_worker(due - Duration::from_nanos(1)));
    assert_eq!(app.account_drain.worker_retry_at, Some(due));
    assert!(app.retry_account_drain_worker(due));
    assert_eq!(app.account_drain.serial, serial);
    assert!(app.account_drain.pause.is_some());
    assert!(app.account_drain.worker_retry_at.is_some());
    assert!(app.account_work.enter().is_none());

    drop(work);
    let drained = app.take_retired_account_pause().unwrap();
    assert!(app.accept_account_drain(serial, drained).is_none());
    assert!(app.account_drain.worker_retry_at.is_none());
    assert!(!app.retry_account_drain_worker(due + WORKER_RETRY));
    assert!(app.account_work.enter().is_some());
}
