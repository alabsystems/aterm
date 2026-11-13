// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Every fact for a renewal, the installed bundle observed as this build.
fn renews() -> RenewalFacts {
    RenewalFacts {
        renewed_from: None,
        auto_apply: true,
        handoff_unavailable: false,
        clean_commit: true,
        update_pending: false,
        installed_build: Some(107),
        current_build: 107,
        update_work: false,
        attempts: 0,
    }
}

/// A RENEWAL THAT WAITED ON ACCOUNT WORK AND DID NOT START CLOSES: no handoff returns it, so it
/// stayed in flight for the process's life. It may still start after the wait, and what ends it
/// while it is in flight still does. CONTROLS: one that started waits for its handoff, and an
/// update's attempt moves nothing of it.
#[test]
fn a_renewal_that_waited_on_account_work_and_did_not_start_closes() {
    use crate::app_account_work::Continuation;
    use crate::app_update_handoff::SameImageHandoff;
    use crate::native_app::UpdateOutcome;
    use crate::native_updater_service::ApplyMode;
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    app.own_launch_record = Some(OwnLaunchRecord::Stale);
    let in_flight = |ending| RenewalState::InFlight {
        armed_at: Instant::now(),
        ending,
    };
    let refused = || UpdateOutcome::Deferred {
        reason: "the desk changed".to_string(),
        hold: crate::native_update_lane_hold::LaneHold::Activity,
    };
    let armed =
        |app: &crate::App| matches!(app.launch_record_renewal.state, RenewalState::Armed { .. });
    let flying = |app: &crate::App| {
        matches!(
            app.launch_record_renewal.state,
            RenewalState::InFlight { .. }
        )
    };
    app.launch_record_renewal.attempts = 1;
    app.launch_record_renewal.state = in_flight(None);
    assert!(
        app.launch_record_renewal_authorizes(),
        "the handoff that waited may still start"
    );

    app.answer_handoff_after_accounts(ApplyMode::Automatic, None, true, UpdateOutcome::Accepted);
    assert!(flying(&app), "CONTROL: its handoff returns it");
    app.answer_handoff_after_accounts(ApplyMode::Automatic, None, false, refused());
    assert!(flying(&app), "CONTROL: an update's attempt");
    app.answer_handoff_after_accounts(ApplyMode::Automatic, None, true, refused());
    assert!(armed(&app), "a refusal after the wait re-arms it");

    app.launch_record_renewal.state = in_flight(None);
    app.finish_account_handoff(Continuation::Handoff {
        mode: ApplyMode::Automatic,
        attempt: None,
        same_image: Some(SameImageHandoff::RenewLaunchRecord),
        next_attempt: app.next_update_handoff_id.wrapping_add(1),
    });
    assert!(armed(&app), "so does a handoff that gave way");

    app.launch_record_renewal.state = in_flight(None);
    app.finish_account_handoff(Continuation::Handoff {
        mode: ApplyMode::Automatic,
        attempt: None,
        same_image: Some(SameImageHandoff::DebugSeam),
        next_attempt: app.next_update_handoff_id.wrapping_add(1),
    });
    assert!(flying(&app), "CONTROL: a handoff that renews nothing");

    let flew_since = Instant::now();
    let flown = |armed_at| RenewalState::InFlight {
        armed_at,
        ending: None,
    };
    let armed_since = |app: &crate::App| match app.launch_record_renewal.state {
        RenewalState::Armed { armed_at, .. } => Some(armed_at),
        _ => None,
    };
    app.sessionless_handoff_refused = None;
    app.launch_record_renewal.attempts = 2;
    app.launch_record_renewal.state = flown(flew_since);
    app.renewal_refused_after_accounts(&crate::UpdateHandoffStartError::activity(
        crate::native_update_lane_hold::LaneHold::Activity,
        "typing",
    ));
    assert_eq!(
        app.launch_record_renewal.attempts, 1,
        "THE ATTEMPT RULE: what the desk held spends none"
    );
    assert_eq!(armed_since(&app), Some(flew_since), "and keeps its ladder");
    app.launch_record_renewal.attempts = 2;
    app.launch_record_renewal.state = flown(flew_since);
    app.renewal_refused_after_accounts(&crate::UpdateHandoffStartError::failed("no thread"));
    assert_eq!(
        app.launch_record_renewal.attempts, 2,
        "CONTROL: this spends one"
    );
    assert!(armed(&app));

    app.launch_record_renewal.state = in_flight(Some(InFlightEnd::UpdatesOff));
    app.renewal_refused_after_accounts(&crate::UpdateHandoffStartError::failed("no thread"));
    assert!(
        matches!(app.launch_record_renewal.state, RenewalState::Ended),
        "what ended it while it was in flight stands here too"
    );

    app.launch_record_renewal.state = in_flight(Some(InFlightEnd::UpdatesOff));
    app.answer_handoff_after_accounts(ApplyMode::Automatic, None, true, refused());
    assert!(
        matches!(app.launch_record_renewal.state, RenewalState::Ended),
        "what ended it while it was in flight stands"
    );
}

/// UNSAVED WORK AT THE SAFETY RE-READ AFTER ACCOUNT WORK SPENDS NO ATTEMPT: that re-read is the same
/// look a renewal makes directly, so what the person's desk held is a wait, and the attempt the
/// wait booked goes back. A renewal that spent one for it would end after three saves' worth of
/// typing.
#[test]
fn unsaved_work_at_the_safety_re_read_after_account_work_spends_no_attempt() {
    use crate::app_account_work::Continuation;
    use crate::app_update_handoff::SameImageHandoff;
    use crate::native_app::{AppEvent, AppKind, TextInputEvent};
    use crate::native_updater_service::ApplyMode;
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("draft.md");
    std::fs::write(&file, "disk\n").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&file).unwrap();
    let mut app = crate::App::headless_for_test();
    app.use_document_journal_root_for_test(dir.path().join("drafts"));
    app.open_document_tab(AppKind::Editor, &uri).unwrap();
    app.dispatch_native_event(
        crate::WindowId(0),
        AppEvent::TextInput(TextInputEvent::Commit("a".to_string())),
    )
    .unwrap();
    assert!(
        app.revalidate_native_update_safety().is_err(),
        "PRECONDITION: a headless process carries no draft, so this holds the handoff"
    );
    app.own_launch_record = Some(OwnLaunchRecord::Stale);
    app.sessionless_handoff_refused = None;
    let flew_since = Instant::now();
    app.launch_record_renewal.attempts = 2;
    app.launch_record_renewal.state = RenewalState::InFlight {
        armed_at: flew_since,
        ending: None,
    };
    app.finish_account_handoff(Continuation::Handoff {
        mode: ApplyMode::Automatic,
        attempt: None,
        same_image: Some(SameImageHandoff::RenewLaunchRecord),
        next_attempt: app.next_update_handoff_id,
    });
    assert_eq!(app.launch_record_renewal.attempts, 1);
    assert!(matches!(
        app.launch_record_renewal.state,
        RenewalState::Armed { armed_at, .. } if armed_at == flew_since
    ));
}

/// A PERSON'S INSTALL TAKES THE PLACE OF A RENEWAL WAITING ON ACCOUNT WORK: told to wait, the press
/// was dropped where no automatic lane would follow it. The renewal does not stay in flight with
/// nothing flying: it takes the ending the stage that the press needs gave it, and with none yet it
/// goes back to its next look.
#[test]
fn a_person_s_install_takes_the_place_of_a_renewal_waiting_on_accounts() {
    for ending in [None, Some(InFlightEnd::LeaveToUpdate)] {
        let mut app = crate::App::headless_for_test();
        let _work = app.account_work.enter().unwrap();
        assert!(
            !app.accounts_drained_before(crate::app_account_work::Continuation::Handoff {
                mode: crate::native_updater_service::ApplyMode::Automatic,
                attempt: None,
                same_image: Some(crate::app_update_handoff::SameImageHandoff::RenewLaunchRecord),
                next_attempt: app.next_update_handoff_id,
            })
        );
        app.launch_record_renewal.state = RenewalState::InFlight {
            armed_at: Instant::now(),
            ending,
        };
        app.apply_staged_update_now(
            crate::app_native::NativeUpdateSafetyToken::carrying_for_test(0, 0),
            crate::native_updater_service::ApplyMode::Immediate,
            None,
            None,
        )
        .expect("the press waits on the drain in its place");
        if ending.is_some() {
            assert!(matches!(
                app.launch_record_renewal.state,
                RenewalState::Ended
            ));
        } else {
            assert!(matches!(
                app.launch_record_renewal.state,
                RenewalState::Armed { .. }
            ));
        }
    }
}

/// A renewal the sessionless handoff cannot carry for a cause the desk holds waits for the
/// desk, uncounted, as one that unsaved work holds does: three such refusals never spend the
/// renewal. REVIEW FINDING (2026-11-03): every other cause (a launched-lane guard, which
/// nothing at the desk clears) was a wait too, so a renewal it refused retried every minute
/// for the life of the process, each look retiring the warm spare shell; it is counted now,
/// and settles after its attempts as the same cause with a terminal tab open does.
#[test]
fn a_desk_the_handoff_cannot_carry_never_spends_an_attempt() {
    for desk in [
        "no window is open to hand over",
        "no open tab is one the new build can restore",
    ] {
        assert_eq!(
            not_started(false, Some(desk)),
            NotStarted::PersonState,
            "{desk}"
        );
        assert_eq!(
            not_started(true, Some(desk)),
            NotStarted::Deferred,
            "{desk}"
        );
    }
    for lane in [
        "this platform has no LaunchServices launcher",
        "this process does not run from a .app bundle",
        "the rendezvous path does not fit sun_path on this machine",
        "the authorized target build is older than this build",
        "the launch environment needs a removal a merge cannot express",
        "the new build failed its pre-park verification",
    ] {
        assert_eq!(
            not_started(false, Some(lane)),
            NotStarted::Counted,
            "a cause no desk clears: {lane}"
        );
    }
    assert_eq!(not_started(true, None), NotStarted::Deferred);
    assert_eq!(
        not_started(false, None),
        NotStarted::Counted,
        "CONTROL: any other refusal spends one"
    );

    // The booking reads the cause the admission left beside the error.
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let now = Instant::now();
    let refused = crate::UpdateHandoffStartError::refused(
        "The update waits for the next launch: no window is open to hand over",
    );
    app.sessionless_handoff_refused = Some("no window is open to hand over");
    app.book_renewal_not_started(now, 1, now, &refused);
    assert_eq!(app.launch_record_renewal.attempts, 0, "the desk spent none");
    assert!(
        matches!(
            app.launch_record_renewal.state,
            RenewalState::Armed { retry_at, .. } if retry_at == now + PERSON_STATE_REPOLL
        ),
        "it looks again when the desk may have changed"
    );
    app.sessionless_handoff_refused = Some("this process does not run from a .app bundle");
    app.book_renewal_not_started(now, 1, now, &refused);
    assert_eq!(
        app.launch_record_renewal.attempts, 1,
        "CONTROL: a cause no desk clears spends one"
    );
}

#[test]
fn a_stale_record_renews_only_when_nothing_else_will() {
    use BookCause::{NoRenewal as Off, Spent, StillStale};
    use RenewalDecision::{Book, LeaveToUpdate, Renew, Wait};
    let base = renews();
    let against = RenewalFacts {
        renewed_from: None,
        auto_apply: false,
        handoff_unavailable: true,
        clean_commit: false,
        update_pending: true,
        installed_build: Some(106),
        current_build: 107,
        update_work: true,
        attempts: ATTEMPTS,
    };
    let rows = [
        (base, Renew),
        (
            RenewalFacts {
                renewed_from: Some(47362),
                ..base
            },
            Book(StillStale { from: 47362 }),
        ),
        (
            RenewalFacts {
                auto_apply: false,
                ..base
            },
            Book(Off(NoRenewal::UpdatesOff)),
        ),
        (
            RenewalFacts {
                handoff_unavailable: true,
                ..base
            },
            Book(Off(NoRenewal::HandoffUnavailable)),
        ),
        (
            RenewalFacts {
                clean_commit: false,
                ..base
            },
            Book(Off(NoRenewal::Unstamped)),
        ),
        (
            RenewalFacts {
                update_pending: true,
                ..base
            },
            LeaveToUpdate,
        ),
        (
            RenewalFacts {
                installed_build: Some(106),
                ..base
            },
            Book(Off(NoRenewal::InstalledIsAnotherBuild)),
        ),
        (
            RenewalFacts {
                update_work: true,
                ..base
            },
            Wait(RenewalWait::Work),
        ),
        (
            RenewalFacts {
                attempts: ATTEMPTS,
                ..base
            },
            Book(Spent { attempts: ATTEMPTS }),
        ),
        (
            RenewalFacts {
                renewed_from: Some(47362),
                ..against
            },
            Book(StillStale { from: 47362 }),
        ),
        (
            RenewalFacts {
                update_pending: true,
                installed_build: Some(108),
                ..base
            },
            LeaveToUpdate,
        ),
        (
            RenewalFacts {
                update_work: true,
                attempts: ATTEMPTS,
                ..base
            },
            Wait(RenewalWait::Work),
        ),
        (
            RenewalFacts {
                installed_build: None,
                ..base
            },
            Renew,
        ),
        (
            RenewalFacts {
                attempts: ATTEMPTS - 1,
                ..base
            },
            Renew,
        ),
    ];
    for (facts, decision) in rows {
        assert_eq!(decide(facts), decision, "{facts:?}");
    }
}
