// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::App;
use super::consent_card::{CardPhase, GRANTED_CAPTION, Marker, Verdict, WATCH_FOR};
use super::consent_state::ConsentState;
use super::message_reporters::KEY_FILE_ACCESS;
use aterm_containment::consent::{FdaProbe, FdaState, ProbeLabel};
use std::time::{Duration, Instant};

/// Real App/card glue over synthetic published observations; never a live OS probe.
pub(super) fn windowed(probe: FdaProbe) -> App {
    let mut app = App::headless_for_test();
    app.headless = false;
    app.consent = ConsentState::with_cached_probe_for_test(probe);
    app
}

fn granted() -> FdaProbe {
    FdaProbe {
        state: FdaState::Granted,
        label: ProbeLabel::OpenOk,
    }
}

#[cfg(target_os = "macos")]
mod fda_grant_history_tests {
    use super::*;
    use aterm_types::macos_setup::{Access, AccessOutcome, access_record};
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    struct History {
        root: PathBuf,
        writes: usize,
        grants: usize,
    }

    thread_local! {
        static HISTORY: RefCell<Option<History>> = const { RefCell::new(None) };
    }

    fn app(root: &Path, revision: u64, probe: FdaProbe) -> App {
        HISTORY.with(|state| {
            *state.borrow_mut() = Some(History {
                root: root.to_owned(),
                writes: 0,
                grants: 0,
            });
        });
        let mut app = App::headless_for_test();
        app.consent = ConsentState::with_cached_probe_for_test(probe);
        app.access_steps
            .set_reach(crate::system_state_access::StepsReach {
                record: |revision, access, outcome| {
                    let root = HISTORY.with(|state| state.borrow().as_ref().unwrap().root.clone());
                    let result = access_record::append(&root, revision, access, outcome);
                    HISTORY.with(|state| state.borrow_mut().as_mut().unwrap().writes += 1);
                    result
                },
                ..crate::system_state_access::StepsReach::INERT
            });
        app.set_system_state_handlers_for_test(crate::InitialSetupHandlers {
            fda_granted: || {
                HISTORY.with(|state| state.borrow_mut().as_mut().unwrap().grants += 1);
            },
            ..crate::InitialSetupHandlers::inert_for_test()
        });
        app.observe_initial_setup_permissions(Some(crate::InitialSetupPermissionPolicy {
            revision,
            allowed: true,
        }));
        app
    }

    #[test]
    fn fda_grant_history_failure_does_not_hold_verified_recovery() {
        let root = aterm_tempfile::tempdir().unwrap();
        let revision = 8_410;
        access_record::append(
            root.path(),
            revision - 1,
            Access::DeveloperTools,
            AccessOutcome::Declined,
        )
        .unwrap();
        let record = root.path().join(access_record::FILE);
        let retained = root.path().join("retained-access-record");
        std::fs::rename(&record, &retained).unwrap();
        std::fs::create_dir(&record).unwrap();
        let mut app = app(root.path(), revision, granted());
        assert_eq!(app.access_revision(), Some(revision));
        let now = Instant::now();

        app.resolve_fda_need(true, now);
        assert!(grant_recorded(&app), "the window verified its own grant");
        let grants = HISTORY.with(|state| state.borrow().as_ref().unwrap().grants);
        assert_eq!(
            HISTORY.with(|state| state.borrow().as_ref().unwrap().writes),
            1
        );
        let retry = app
            .next_access_step_deadline(now)
            .expect("the history save is still owed");

        // A later approval cannot retarget the completed outcome's pending save.
        app.observe_initial_setup_permissions(Some(crate::InitialSetupPermissionPolicy {
            revision: revision + 1,
            allowed: true,
        }));
        std::fs::remove_dir(&record).unwrap();
        std::fs::rename(&retained, &record).unwrap();
        app.settle_access_steps(retry);
        assert_eq!(
            access_record::latest(root.path(), revision, Access::FullDiskAccess),
            Some(AccessOutcome::Granted)
        );
        assert_eq!(
            access_record::latest(root.path(), revision + 1, Access::FullDiskAccess),
            None
        );
        assert_eq!(
            access_record::latest(root.path(), revision - 1, Access::DeveloperTools),
            Some(AccessOutcome::Declined)
        );
        app.settle_access_steps(retry + Duration::from_secs(1));
        assert_eq!(
            HISTORY.with(|state| state.borrow().as_ref().unwrap().writes),
            2
        );
        assert_eq!(
            grants, 1,
            "verified FDA must notify deferred work before its separate history is writable"
        );
    }

    #[test]
    fn fda_grant_history_success_keeps_the_existing_append_wake() {
        let root = aterm_tempfile::tempdir().unwrap();
        let revision = 8_430;
        let mut app = app(root.path(), revision, granted());
        let now = Instant::now();
        app.resolve_fda_need(true, now);
        app.resolve_fda_need(true, now);
        assert!(grant_recorded(&app));
        assert_eq!(
            access_record::latest(root.path(), revision, Access::FullDiskAccess),
            Some(AccessOutcome::Granted)
        );
        assert_eq!(
            HISTORY.with(|state| state.borrow().as_ref().unwrap().writes),
            1
        );
        assert_eq!(
            HISTORY.with(|state| state.borrow().as_ref().unwrap().grants),
            0
        );
        assert_eq!(app.next_access_step_deadline(now), None);
    }

    #[test]
    fn fda_grant_history_recovery_requires_this_process_to_have_the_grant() {
        let root = aterm_tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut unread = app(
            root.path(),
            8_420,
            FdaProbe::refused(ProbeLabel::RefusedNoHome),
        );
        unread.headless = false;
        assert!(unread.consent_card.begin_deciding(now));
        assert!(unread.consent_card.on_decided(&Verdict::Offer, now));
        unread.tick_macos_access_card(now);
        assert_eq!(unread.consent_card.phase(), CardPhase::Settled);
        assert!(!grant_recorded(&unread));
        assert_eq!(
            HISTORY.with(|state| state.borrow().as_ref().unwrap().writes),
            0
        );
        assert_eq!(
            HISTORY.with(|state| state.borrow().as_ref().unwrap().grants),
            0
        );

        let mut revoked = app(root.path(), 8_422, denied());
        revoked.headless = false;
        assert!(revoked.consent_card.begin_deciding(now));
        assert!(revoked.consent_card.on_decided(&Verdict::Offer, now));
        revoked.tick_macos_access_card(now);
        assert_eq!(
            revoked.consent_card.phase(),
            CardPhase::Watching { since: now }
        );
        assert!(!grant_recorded(&revoked));
        assert_eq!(
            HISTORY.with(|state| state.borrow().as_ref().unwrap().writes),
            0
        );
        assert_eq!(
            HISTORY.with(|state| state.borrow().as_ref().unwrap().grants),
            0
        );

        let mut foreign = app(root.path(), 8_421, granted());
        foreign
            .consent
            .set_responsible_for_test(|pids| pids.iter().map(|_| Ok(1)).collect());
        foreign.resolve_fda_need(true, now);
        assert!(!grant_recorded(&foreign));
        assert!(access_record::read(root.path(), 8_421).is_empty());
        assert_eq!(
            HISTORY.with(|state| state.borrow().as_ref().unwrap().writes),
            0
        );
        assert_eq!(
            HISTORY.with(|state| state.borrow().as_ref().unwrap().grants),
            0
        );
    }
}

fn denied() -> FdaProbe {
    FdaProbe {
        state: FdaState::Denied,
        label: ProbeLabel::OpenEperm,
    }
}

/// Whether the grant is on record (a grant is a record, never a row).
pub(super) fn grant_recorded(app: &App) -> bool {
    app.messages
        .log()
        .records()
        .any(|rec| rec.title == GRANTED_CAPTION)
}

/// Whether the question stands: an entry of the digest, or a `privacy.fda` row an older parent
/// carried across an update.
pub(super) fn access_row(app: &App) -> bool {
    app.messages.live_by_key(KEY_FILE_ACCESS).is_some()
        || app
            .messages
            .digest()
            .entries
            .iter()
            .any(|e| e.key.starts_with("privacy.fda."))
}

#[test]
fn pending_launch_retains_the_opened_marker_until_access_is_observed() {
    let now = Instant::now();
    let mut app = windowed(FdaProbe::refused(ProbeLabel::Pending));
    assert!(app.consent_card.begin_deciding(now));
    app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(Some(
        Marker::Opened,
    )));
    assert_eq!(app.consent_card.phase(), CardPhase::Deciding { since: now });
    assert_eq!(
        app.consent_card_host.pending,
        Some(crate::consent_card::PendingDecision::launch(Some(
            Marker::Opened
        )))
    );
    assert!(
        !access_row(&app) && !grant_recorded(&app),
        "pending is neither an offer nor a confirmation"
    );

    app.tick_macos_access_card(now);
    assert_eq!(
        app.consent_card_host.pending,
        Some(crate::consent_card::PendingDecision::launch(Some(
            Marker::Opened
        )))
    );
    assert!(!access_row(&app) && !grant_recorded(&app));
    app.consent = ConsentState::with_cached_probe_for_test(granted());
    app.tick_macos_access_card(now);
    assert_eq!(app.consent_card_host.pending, None);
    assert_eq!(app.consent_card.phase(), CardPhase::Settled);
    assert!(grant_recorded(&app), "the confirmation is a record");
    assert!(!access_row(&app));
    assert_eq!(app.messages.wanted_rows(), 0, "…and never a row");
    assert_eq!(app.next_macos_access_card_deadline(now), None);
}

/// A watch whose evidence goes pending, then denied, records no grant: a grant is recorded only
/// from a completed `granted` read, so no older observation replays as a confirmation.
#[test]
fn pending_and_withdrawn_access_never_publish_an_old_confirmation() {
    let now = Instant::now();
    let mut app = windowed(denied());
    assert!(app.consent_card.begin_deciding(now));
    assert!(app.consent_card.on_decided(&Verdict::Offer, now));
    app.tick_macos_access_card(now);
    assert!(
        !access_row(&app),
        "the question posts nothing: System State holds it"
    );
    assert_eq!(app.consent_card.phase(), CardPhase::Watching { since: now });

    app.consent = ConsentState::with_cached_probe_for_test(FdaProbe::refused(ProbeLabel::Pending));
    app.tick_macos_access_card(now + Duration::from_secs(1));
    assert_eq!(app.consent_card.phase(), CardPhase::Watching { since: now });
    assert!(
        !grant_recorded(&app),
        "expired evidence cannot confirm access"
    );
    assert!(!access_row(&app), "and still posts nothing");

    app.consent = ConsentState::with_cached_probe_for_test(denied());
    app.tick_macos_access_card(now + Duration::from_secs(2));
    assert_eq!(app.consent_card.phase(), CardPhase::Watching { since: now });
    assert!(
        !grant_recorded(&app),
        "a withdrawn grant cannot become a success record"
    );
}

#[test]
fn a_due_card_waits_for_pending_access_and_retires_an_unknown_result() {
    let now = Instant::now();
    let mut app = windowed(FdaProbe::refused(ProbeLabel::Pending));
    assert!(app.consent_card.begin_deciding(now));
    assert!(app.consent_card.on_decided(&Verdict::Offer, now));
    app.tick_macos_access_card(now);
    assert_eq!(app.consent_card.phase(), CardPhase::Due);
    assert!(!access_row(&app), "a pending probe posts nothing yet");

    app.consent =
        ConsentState::with_cached_probe_for_test(FdaProbe::refused(ProbeLabel::RefusedNoHome));
    app.tick_macos_access_card(now);
    assert_eq!(app.consent_card.phase(), CardPhase::Settled);
    assert!(
        !access_row(&app),
        "unknown access cannot post the waiting question"
    );
}

#[test]
fn disabling_checks_clears_a_pending_answer_and_its_route_row() {
    let now = Instant::now();
    let mut app = windowed(FdaProbe::refused(ProbeLabel::Pending));
    assert!(app.consent_card.begin_deciding(now));
    app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(Some(
        Marker::Opened,
    )));
    let id = app.post_message(super::message_reporters::file_access_question(
        &super::consent_card::NeedWords::sample(),
    ));
    app.consent_card_message = Some(id);
    assert!(access_row(&app));
    assert!(app.messages.restate(
        id,
        aterm_messages::Restatement {
            title: Some("Enable aterm in Full Disk Access".into()),
            actions: Some(Vec::new()),
            ..Default::default()
        },
        now,
    ));
    app.config.privacy = Some(super::app_config::PrivacyConfig {
        check: Some(false),
        ..Default::default()
    });
    app.tick_macos_access_card(now);
    assert_eq!(app.consent_card_host.pending, None);
    assert_eq!(app.consent_card.phase(), CardPhase::Settled);
    assert!(!access_row(&app), "the route row is resolved with the card");
    assert_eq!(app.consent_card_message, None);
    app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(Some(
        Marker::Opened,
    )));
    assert_eq!(app.consent_card.phase(), CardPhase::Settled);
    assert!(!access_row(&app));
    assert_eq!(app.next_macos_access_card_deadline(now), None);
}

#[test]
fn refresh_deadlines_are_future_only_and_stop_when_the_watch_settles() {
    let mut app = windowed(denied());
    let now = Instant::now();
    assert!(app.consent_card.begin_deciding(now));
    assert!(app.consent_card.on_decided(&Verdict::Offer, now));
    app.tick_macos_access_card(now);
    assert!(!access_row(&app), "the watch posts nothing");
    for interval in [0, 250, 5000] {
        app.config.privacy = Some(super::app_config::PrivacyConfig {
            probe_interval_ms: Some(interval),
            ..Default::default()
        });
        for elapsed in [Duration::ZERO, Duration::from_secs(20)] {
            let sample = now + elapsed;
            let deadline = app
                .next_macos_access_card_deadline(sample)
                .expect("active watch wakes");
            assert!(
                deadline > sample,
                "a stale entry must not schedule a past deadline"
            );
            assert!(deadline <= sample + Duration::from_secs(5));
        }
    }
    app.tick_macos_access_card(now + WATCH_FOR);
    assert_eq!(app.consent_card.phase(), CardPhase::Settled);
    assert_eq!(
        app.next_macos_access_card_deadline(now + WATCH_FOR),
        None,
        "a settled card owns no timer: no daily return, no probe cadence"
    );
}

#[test]
fn settings_gesture_restarts_a_settled_watch_with_fresh_evidence() {
    let now = Instant::now();
    let mut app = windowed(granted());
    app.consent_card.settle();
    app.consent_card_host.pending = Some(crate::consent_card::PendingDecision::launch(Some(
        Marker::Opened,
    )));
    app.note_macos_access_settings_opened(now);
    assert_eq!(app.consent_card.phase(), CardPhase::Watching { since: now });
    assert_eq!(app.consent_card_host.pending, None);
    assert_ne!(
        app.consent_panel_facts().fda,
        FdaState::Granted,
        "the prior cached grant cannot acknowledge the new Settings gesture"
    );
    assert!(app.next_macos_access_card_deadline(now).is_some());
    app.consent = ConsentState::with_cached_probe_for_test(granted());
    app.tick_macos_access_card(now);
    assert_eq!(app.consent_card.phase(), CardPhase::Settled);
    assert!(grant_recorded(&app));
    assert_eq!(app.next_macos_access_card_deadline(now), None);

    app.config.privacy = Some(super::app_config::PrivacyConfig {
        notice: Some(false),
        ..Default::default()
    });
    app.note_macos_access_settings_opened(now);
    assert_eq!(app.consent_card.phase(), CardPhase::Settled);
    assert_eq!(app.next_macos_access_card_deadline(now), None);
}

/// A window standing in for a new Mac: the probe reads `denied`, the hand-over lane
/// opens pages and shows panels through counted stand-ins, and the launch state knows a
/// bundle id.
fn new_mac() -> App {
    use crate::consent_launch::{LaunchConsent, LaunchReach};
    use crate::devtools_helper::{Lane, test_arms};
    let mut app = windowed(denied());
    app.config.privacy = Some(super::app_config::PrivacyConfig {
        probe_interval_ms: Some(3_600_000),
        ..Default::default()
    });
    app.devtools_helper = Lane::windowed_with_fda_for_test(
        test_arms::opening(),
        test_arms::fda_opening(),
        test_arms::showing(),
    );
    app.consent_launch = LaunchConsent::with_reach(LaunchReach {
        bundle_id: || Some(crate::devtools_helper::test_arms::SUBJECT),
        live: true,
        ..LaunchReach::inert()
    });
    // The approved setup revision whose steps the window runs.
    app.observe_initial_setup_permissions(Some(crate::InitialSetupPermissionPolicy {
        revision: 1,
        allowed: true,
    }));
    app
}

/// The quiet moment, as a test grants it (no window is focused in a test).
fn quiet(_app: &App, _now: Instant) -> bool {
    true
}

/// While no approved revision admits permissions (a setup decision held, or none made), the launch
/// check still reads, putting nothing on screen, but the window's own pages wait: the Full Disk
/// Access hand-over stays queued at its moment and a late `spctl` run's Developer Tools page is
/// held. The owner's own press still opens.
#[test]
fn setup_permission_hold_defers_queued_launch_decisions_and_automatic_page_effects() {
    use crate::devtools_helper::{HandOver, Origin, test_arms};
    let now = Instant::now();
    let mut app = new_mac();
    app.hold_initial_setup_permissions();
    assert_eq!(
        app.access_revision(),
        None,
        "a held decision admits no step"
    );
    assert!(app.consent_card.begin_deciding(now));
    let decision = crate::consent_card::PendingDecision::launch(None);
    let fda_opens = test_arms::fda_counts().0;
    app.decide_macos_access_card(decision);
    assert_eq!(
        app.consent_card.phase(),
        CardPhase::Settled,
        "it only reads"
    );
    assert_eq!(app.devtools_helper.fda_queued(), Some(Origin::Automatic));
    app.look_for_fda_moment(now, quiet);
    assert_eq!(
        app.devtools_helper.fda_queued(),
        Some(Origin::Automatic),
        "held for an approved revision"
    );
    assert_eq!(test_arms::fda_counts().0, fda_opens, "nothing opens");
    test_arms::stand_in(&mut app, test_arms::opening());
    let opens = test_arms::opens();
    assert_eq!(
        app.devtools_hand_over_after_listing(Origin::Automatic, Ok(()), now),
        HandOver::Queued
    );
    assert_eq!(
        test_arms::opens(),
        opens,
        "a late worker cannot open System Settings"
    );
    assert_eq!(
        app.hand_devtools_to_owner(Origin::Settings),
        HandOver::Opened
    );
    assert_eq!(
        test_arms::opens(),
        opens + 1,
        "the owner's explicit button still works"
    );
    assert!(app.devtools_helper.next_deadline(now, || true).is_some());
}

/// A new Mac's launch check: the card settles and the window's own Full Disk Access
/// hand-over waits for its quiet moment.
fn launched(now: Instant) -> App {
    let mut app = new_mac();
    assert!(app.consent_card.begin_deciding(now));
    app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
    app
}

/// On the first launch on a new Mac, Full Disk Access opens at the quiet moment, and Not now lets
/// the folder warm-up walk.
#[test]
fn a_new_mac_brings_the_owner_to_full_disk_access_then_to_the_folders() {
    use crate::consent_launch::FdaLaunch;
    use crate::devtools_helper::{Origin, PanelEvent, test_arms};
    let now = Instant::now();
    let mut app = launched(now);
    assert_eq!(app.consent_card.phase(), CardPhase::Settled);
    assert!(!access_row(&app), "no digest entry: the page is the ask");
    assert_eq!(app.devtools_helper.fda_queued(), Some(Origin::Automatic));
    assert_eq!(app.consent_launch.fda(), FdaLaunch::Waiting);

    app.launch_warmup_look(now, quiet);
    assert!(!app.consent_launch.settled());
    assert_eq!(
        app.consent_launch.next_deadline(),
        None,
        "waits with no timer"
    );

    let (fda_opens, fda_notes, fda_declines) = test_arms::fda_counts();
    let devtools_opens = test_arms::opens();
    app.look_for_fda_moment(Instant::now(), quiet);
    assert_eq!(
        test_arms::fda_counts(),
        (fda_opens + 1, fda_notes + 1, fda_declines)
    );
    assert_eq!(
        test_arms::opens(),
        devtools_opens,
        "the Developer Tools page stays shut"
    );
    assert!(app.fda_panel_up());
    assert_eq!(app.devtools_helper.fda_queued(), None);
    assert_eq!(app.consent_launch.fda(), FdaLaunch::Waiting);

    app.devtools_helper.press_for_test(PanelEvent::NotNow);
    app.tick_devtools_hand_over(now);
    assert!(!app.fda_panel_up());
    assert_eq!(test_arms::fda_counts().2, fda_declines + 1);
    assert_eq!(
        app.consent_launch.fda(),
        FdaLaunch::Ended { owner_acted: false }
    );

    app.launch_warmup_look(now, quiet);
    assert_eq!(app.consent_launch.next_deadline(), None);
    assert!(!app.consent_launch.walking());
    app.consent = ConsentState::with_cached_probe_for_test(denied());
    app.launch_warmup_look(now, |_, _| false);
    assert!(app.consent_launch.next_deadline().is_some());
    app.launch_warmup_look(now + Duration::from_secs(5), quiet);
    assert!(app.consent_launch.walking(), "the folders are asked for");
}

/// A cache that expired between the launch check and the quiet moment reads pending:
/// the look waits for the fresh reading instead of refusing the hand-over as unknown.
#[test]
fn a_pending_read_at_the_quiet_moment_waits_rather_than_refuses() {
    use crate::consent_launch::FdaLaunch;
    use crate::devtools_helper::{Origin, test_arms};
    let now = Instant::now();
    let mut app = launched(now);
    app.consent = ConsentState::with_cached_probe_for_test(FdaProbe::refused(ProbeLabel::Pending));
    let (opens, _, _) = test_arms::fda_counts();
    app.look_for_fda_moment(Instant::now(), quiet);
    assert_eq!(app.devtools_helper.fda_queued(), Some(Origin::Automatic));
    assert_eq!(app.consent_launch.fda(), FdaLaunch::Waiting);
    assert_eq!(test_arms::fda_counts().0, opens);
    assert!(
        !app.devtools_helper.fda_hand_over_due(Instant::now()),
        "the next look is deferred"
    );
    app.consent = ConsentState::with_cached_probe_for_test(denied());
    let later = Instant::now() + crate::devtools_helper::PANEL_TICK;
    app.look_for_fda_moment(later, quiet);
    assert_eq!(test_arms::fda_counts().0, opens + 1);
    assert!(app.fda_panel_up());
}

/// The owner's "I've switched it on" ends the panel as the owner's act: the access
/// card's `opened` marker and watch, and no folder dialogs on top of the grant.
#[test]
fn switched_on_is_the_owners_act_and_no_folders_follow() {
    use crate::consent_launch::FdaLaunch;
    use crate::devtools_helper::PanelEvent;
    let now = Instant::now();
    let mut app = launched(now);
    app.look_for_fda_moment(Instant::now(), quiet);
    app.devtools_helper.press_for_test(PanelEvent::SwitchedOn);
    app.tick_devtools_hand_over(now);
    assert_eq!(
        app.consent_launch.fda(),
        FdaLaunch::Ended { owner_acted: true }
    );
    assert_eq!(app.consent_card_host.marker, Some(Marker::Opened));
    app.launch_warmup_look(now, quiet);
    assert!(app.consent_launch.settled());
}

/// The owner's own Open Settings for Full Disk Access, over a grant that reads held,
/// takes the window's waiting hand-over with it and shows no panel: nothing is left to
/// say how that visit ends, so the launch counts the owner at the page.
#[test]
fn a_full_disk_access_hand_over_gone_with_no_panel_holds_nothing() {
    use crate::consent_launch::FdaLaunch;
    use crate::devtools_helper::Origin;
    let now = Instant::now();
    let mut app = launched(now);
    assert_eq!(app.consent_launch.fda(), FdaLaunch::Waiting);
    app.consent = ConsentState::with_cached_probe_for_test(granted());
    #[expect(
        clippy::disallowed_methods,
        reason = "the owner's press, played by the test"
    )]
    app.fda_page_opened_by_owner(Origin::Settings);
    assert!(!app.fda_panel_up() && app.devtools_helper.fda_queued().is_none());
    assert_eq!(
        app.consent_launch.fda(),
        FdaLaunch::Ended { owner_acted: true }
    );
    app.launch_warmup_look(now, quiet);
    assert!(
        app.consent_launch.settled(),
        "waiting on nothing holds nothing"
    );
}

/// A probe that reads the grant while the panel is up turns it to its done words and
/// records the grant once, however many turns see it.
#[test]
fn a_grant_read_while_the_panel_is_up_is_recorded_once() {
    use crate::devtools_helper::Phase;
    let now = Instant::now();
    let mut app = launched(now);
    app.look_for_fda_moment(Instant::now(), quiet);
    app.consent = ConsentState::with_cached_probe_for_test(granted());
    app.tick_devtools_hand_over(now);
    app.tick_devtools_hand_over(now + crate::devtools_helper::DONE_FOR / 2);
    assert_eq!(
        app.devtools_helper.panel().map(|panel| panel.phase()),
        Some(Phase::Done)
    );
    let records = app
        .messages
        .log()
        .records()
        .filter(|rec| rec.title == GRANTED_CAPTION)
        .count();
    assert_eq!(records, 1);
}

/// Where Full Disk Access reads held, or the owner said Not now to the question for
/// good, nothing is brought up; with the question declined the folders are still asked
/// for.
#[test]
fn a_held_grant_or_a_standing_not_now_brings_up_nothing() {
    use crate::consent_launch::FdaLaunch;
    let now = Instant::now();
    let mut app = new_mac();
    app.consent = ConsentState::with_cached_probe_for_test(granted());
    assert!(app.consent_card.begin_deciding(now));
    app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
    assert_eq!(app.devtools_helper.fda_queued(), None);
    assert_eq!(app.consent_launch.fda(), FdaLaunch::NotAsked);
    app.launch_warmup_look(now, quiet);
    assert!(app.consent_launch.settled());

    let mut app = new_mac();
    assert!(app.consent_card.begin_deciding(now));
    app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(Some(
        Marker::NotNow,
    )));
    assert_eq!(app.devtools_helper.fda_queued(), None);
    assert_eq!(app.consent_launch.fda(), FdaLaunch::NotAsked);
    app.launch_warmup_look(now, quiet);
    assert!(
        app.consent_launch.walking(),
        "the folders are still asked for"
    );
}

/// The folder warm-up waits while a Developer Tools hand-over is queued: one System
/// Settings page or folder dialog at a time.
#[test]
fn the_folders_wait_behind_a_queued_developer_tools_hand_over() {
    use crate::devtools_helper::Origin;
    let now = Instant::now();
    let mut app = new_mac();
    app.consent = ConsentState::with_cached_probe_for_test(denied());
    app.consent_launch
        .set_fda(crate::consent_launch::FdaLaunch::NotAsked);
    app.devtools_helper.queue(Origin::Automatic, now);
    app.launch_warmup_look(now, quiet);
    assert!(!app.consent_launch.walking());
    assert!(app.consent_launch.next_deadline().is_some());
}

/// The owner's own Open Settings for Full Disk Access brings aterm's panel beside the
/// page, and ends a waiting automatic hand-over.
#[test]
fn the_owners_open_settings_brings_the_panel() {
    use crate::devtools_helper::Origin;
    let now = Instant::now();
    let mut app = launched(now);
    assert_eq!(app.devtools_helper.fda_queued(), Some(Origin::Automatic));
    #[expect(
        clippy::disallowed_methods,
        reason = "the owner's press, played by the test"
    )]
    app.fda_page_opened_by_owner(Origin::Settings);
    assert!(app.fda_panel_up());
    assert_eq!(app.devtools_helper.fda_queued(), None);
    assert_eq!(
        app.devtools_helper.panel().map(|panel| panel.origin()),
        Some(Origin::Settings)
    );
}

/// `aterm ctl devtools hand-over now`, the permission walk with Developer Tools first, never
/// takes the window's own Full Disk Access panel away (nor, with it, the folder warm-up that
/// follows a Not now): its page waits for that panel to go, for at most ten minutes of the
/// person's time at the Mac.
#[test]
fn a_control_verbs_now_walk_waits_behind_full_disk_access() {
    use crate::devtools_helper::{Page, test_arms};
    use crate::permission_walk::{Started, Subject};
    let now = Instant::now();
    let mut app = launched(now);
    app.user_input_recent = |_| true;
    app.devtools_helper.note_reading(
        test_arms::reading(
            aterm_containment::DevToolGrant::NotGranted,
            test_arms::absent(2),
        ),
        now,
    );
    app.look_for_fda_moment(Instant::now(), quiet);
    assert!(app.fda_panel_up());
    let asked = Instant::now();
    let started = app.ask_permission_walk(&[Subject::DeveloperTools], asked);
    assert!(matches!(started, Started::Walking { .. }), "{started:?}");
    app.drive_permission_walk(asked + crate::permission_walk::GRANT_POLL);
    assert_eq!(
        app.devtools_helper.panel().map(|panel| panel.page()),
        Some(Page::FullDiskAccess)
    );
    assert!(app.permission_walk.active(), "its page waits");
    app.drive_permission_walk(asked + crate::devtools_helper::PANEL_FOR);
    assert!(
        !app.permission_walk.active(),
        "a page held off for ten minutes of the person's time ends the walk"
    );
    assert_eq!(
        app.devtools_helper.panel().map(|panel| panel.page()),
        Some(Page::FullDiskAccess)
    );

    // NEGATIVE CONTROL: the owner's own press on the Developer Tools row takes the page at once.
    let mut pressed = launched(now);
    pressed.devtools_helper.note_reading(
        test_arms::reading(
            aterm_containment::DevToolGrant::NotGranted,
            test_arms::absent(2),
        ),
        now,
    );
    pressed.look_for_fda_moment(Instant::now(), quiet);
    assert!(pressed.fda_panel_up());
    let _ = pressed.start_permission_walk(&[Subject::DeveloperTools], "a person's press", asked);
    assert_eq!(
        pressed.devtools_helper.panel().map(|panel| panel.page()),
        Some(Page::DeveloperTools)
    );
}

/// Every folder refused, without a filesystem.
fn refusing_folder(_path: &std::path::Path) -> crate::consent_warmup::ReadDirOutcome {
    crate::consent_warmup::ReadDirOutcome::Failed(aterm_containment::consent::ERRNO_EPERM)
}

/// Wait (a one-minute hang detector) for the warm-up worker to end.
fn walked(app: &mut App) {
    let give_up = Instant::now() + Duration::from_secs(60);
    while app.consent_warmup_live() {
        assert!(Instant::now() < give_up, "the warm-up worker never ended");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// What `Wake::WarmupResult` does to the window, once per wake.
fn warmup_wake(app: &mut App) {
    let _ = app.consent_warmup.drain();
    app.note_warmup_denials();
    app.note_launch_warmup_progress();
}

/// A Don't Allow in the launch pass is the owner's answer to aterm's own question: no
/// Full Disk Access need, however many wakes report the pass after it ended.
#[test]
fn the_launch_pass_refusals_raise_no_need_but_the_owners_pass_does() {
    let now = Instant::now();
    let mut app = new_mac();
    app.consent_warmup =
        crate::consent_warmup::WarmupState::with_read_dir_for_test(refusing_folder);
    app.consent_launch
        .set_fda(crate::consent_launch::FdaLaunch::NotAsked);
    app.launch_warmup_look(now, quiet);
    assert!(app.consent_launch.walking());
    walked(&mut app);
    for _ in 0..3 {
        warmup_wake(&mut app);
    }
    assert!(app.consent_launch.settled());
    assert_eq!(app.consent_card_host.needs, 0);
    assert!(!access_row(&app));

    assert_eq!(
        app.begin_consent_warmup(),
        crate::consent_warmup::StartOutcome::Started
    );
    walked(&mut app);
    warmup_wake(&mut app);
    assert_eq!(app.consent_card_host.needs, 1, "the owner's pass is a need");
}

/// A launch that finds a grant verified before, reading off now: on the Mac it was
/// verified on, nothing comes up (it was switched off there); on another Mac — a config
/// directory synced or migrated — it is a new Mac, and the page comes up; a record from
/// an older build, with no Mac on it, keeps its word.
#[test]
fn a_grant_verified_on_another_mac_does_not_stop_the_new_macs_page() {
    use crate::consent_card::{AccessRecord, DecideWhy, PendingDecision};
    use crate::devtools_helper::Origin;
    let now = Instant::now();
    for (verified_here, page) in [(Some(false), true), (Some(true), false), (None, false)] {
        let mut app = new_mac();
        assert!(app.consent_card.begin_deciding(now));
        app.decide_macos_access_card(PendingDecision {
            marker: None,
            record: AccessRecord {
                verified_at: Some(1_790_000_000),
                ..AccessRecord::default()
            },
            why: DecideWhy::Launch,
            verified_here,
        });
        assert_eq!(
            app.devtools_helper.fda_queued(),
            page.then_some(Origin::Automatic),
            "{verified_here:?}"
        );
        if page {
            assert_eq!(
                app.consent_card_host.settle_look,
                crate::consent_card::SettleLook::Idle,
                "the new Mac's page: no look again"
            );
        }
    }
}

/// An update's successor that starts while its predecessor's Full Disk Access page may
/// still be in front of the owner (opened within ten minutes) brings up no page and
/// walks no folders over it.
#[test]
fn a_successor_leaves_its_predecessors_page_alone() {
    use crate::consent_launch::FdaLaunch;
    use crate::devtools_helper::{Lane, test_arms};
    let now = Instant::now();
    let mut app = new_mac();
    app.devtools_helper = Lane::windowed_with_fda_for_test(
        test_arms::opening(),
        test_arms::fda_opened_just_now(),
        test_arms::showing(),
    );
    assert!(app.consent_card.begin_deciding(now));
    app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
    assert_eq!(app.devtools_helper.fda_queued(), None);
    assert_eq!(
        app.consent_launch.fda(),
        FdaLaunch::Ended { owner_acted: true }
    );
    app.launch_warmup_look(now, quiet);
    assert!(app.consent_launch.settled());
}

/// The page aterm brings up itself is the card's Open Settings: the `opened` marker and
/// `opened_at` are on record the moment it opens, so a grant made there and followed by
/// System Settings' own Quit & Reopen is confirmed by the next process.
#[test]
fn the_automatic_page_is_on_record_for_the_next_process_to_confirm() {
    let now = Instant::now();
    let mut app = launched(now);
    assert_eq!(app.consent_card_host.marker, None);
    app.look_for_fda_moment(Instant::now(), quiet);
    assert!(app.fda_panel_up());
    assert_eq!(app.consent_card_host.marker, Some(Marker::Opened));
    assert!(app.consent_card_host.record.opened_at.is_some());

    let mut next = new_mac();
    next.consent = ConsentState::with_cached_probe_for_test(granted());
    assert!(next.consent_card.begin_deciding(now));
    next.decide_macos_access_card(crate::consent_card::PendingDecision {
        record: app.consent_card_host.record,
        ..crate::consent_card::PendingDecision::launch(app.consent_card_host.marker)
    });
    assert!(grant_recorded(&next), "the grant is confirmed and recorded");
    assert!(next.consent_card_host.record.verified_at.is_some());
}

/// `probe`, read by a window macOS holds responsible for itself: its own grant.
fn own(probe: FdaProbe) -> ConsentState {
    let mut consent = ConsentState::with_cached_probe_for_test(probe);
    consent.set_responsible_for_test(|pids| pids.iter().map(|&pid| Ok(pid)).collect());
    consent
}

/// `probe`, read by a window another app started in-process: macOS names that app (here
/// launchd's pid) responsible, and the probe reads its grant.
fn foreign(probe: FdaProbe) -> ConsentState {
    let mut consent = ConsentState::with_cached_probe_for_test(probe);
    consent.set_responsible_for_test(|pids| pids.iter().map(|_| Ok(1)).collect());
    consent
}

/// A grant aterm reads held — at launch, never asked for, or at a waiting hand-over's
/// quiet moment — is recorded as seen on this Mac, so a later switch-off here is not
/// taken for a new Mac and brings nothing up.
#[test]
fn a_grant_aterm_reads_held_is_recorded_so_a_switch_off_brings_nothing_up() {
    use crate::consent_card::{DecideWhy, PendingDecision};
    use crate::consent_launch::FdaLaunch;
    use crate::devtools_helper::test_arms;
    let now = Instant::now();
    let mut app = new_mac();
    app.consent = own(granted());
    assert!(app.consent_card.begin_deciding(now));
    app.decide_macos_access_card(PendingDecision::launch(None));
    let record = app.consent_card_host.record;
    assert!(record.verified_at.is_some(), "the held grant is on record");
    assert!(!grant_recorded(&app), "silently: the owner was never asked");

    let mut later = new_mac();
    assert!(later.consent_card.begin_deciding(now));
    later.decide_macos_access_card(PendingDecision {
        record,
        why: DecideWhy::Launch,
        verified_here: Some(true),
        ..PendingDecision::launch(None)
    });
    assert_eq!(later.devtools_helper.fda_queued(), None);

    let mut waiting = launched(now);
    waiting.consent = own(granted());
    let (opens, _, _) = test_arms::fda_counts();
    waiting.look_for_fda_moment(Instant::now(), quiet);
    assert_eq!(test_arms::fda_counts().0, opens, "nothing comes up");
    assert_eq!(waiting.devtools_helper.fda_queued(), None);
    assert!(waiting.consent_card_host.record.verified_at.is_some());
    assert_eq!(waiting.consent_launch.fda(), FdaLaunch::NotAsked);
}

/// A window started in-process by another terminal reads that terminal's grant (macOS holds the
/// terminal responsible): it is never recorded as aterm's, at launch or at a waiting hand-over's
/// quiet moment, so a later aterm of its own still gets the page.
#[test]
fn another_terminals_grant_is_never_recorded_as_aterms() {
    use crate::devtools_helper::test_arms;
    let now = Instant::now();
    let mut app = new_mac();
    app.consent = foreign(granted());
    assert!(!app.consent.window_reads_own_grant());
    assert!(app.consent_card.begin_deciding(now));
    app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
    assert_eq!(app.consent_card_host.record.verified_at, None);

    let mut confirm = new_mac();
    confirm.consent = foreign(granted());
    assert!(confirm.consent_card.begin_deciding(now));
    confirm.decide_macos_access_card(crate::consent_card::PendingDecision {
        record: crate::consent_card::AccessRecord {
            opened_at: Some(1_790_000_000),
            ..crate::consent_card::AccessRecord::default()
        },
        ..crate::consent_card::PendingDecision::launch(Some(Marker::Opened))
    });
    assert_eq!(confirm.consent_card_host.record.verified_at, None);
    assert_eq!(confirm.consent_card_host.marker, Some(Marker::Opened));
    assert!(!grant_recorded(&confirm));

    let mut denied_there = new_mac();
    denied_there.consent = foreign(denied());
    assert!(denied_there.consent_card.begin_deciding(now));
    denied_there.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
    let (opens, _, _) = test_arms::fda_counts();
    denied_there.look_for_fda_moment(Instant::now(), quiet);
    assert_eq!(test_arms::fda_counts().0, opens, "nothing comes up");
    assert_eq!(denied_there.devtools_helper.fda_queued(), None);
    assert_eq!(
        denied_there.consent_launch.fda(),
        crate::consent_launch::FdaLaunch::NotAsked
    );

    let mut walker = new_mac();
    walker.consent = foreign(denied());
    walker
        .consent_launch
        .set_fda(crate::consent_launch::FdaLaunch::NotAsked);
    walker.launch_warmup_look(now, quiet);
    assert!(walker.consent_launch.settled() && !walker.consent_launch.walking());

    let mut waiting = launched(now);
    waiting.consent = foreign(granted());
    let (opens, _, _) = test_arms::fda_counts();
    waiting.look_for_fda_moment(Instant::now(), quiet);
    assert_eq!(test_arms::fda_counts().0, opens, "nothing comes up");
    assert_eq!(waiting.devtools_helper.fda_queued(), None);
    assert_eq!(waiting.consent_card_host.record.verified_at, None);
    assert!(!grant_recorded(&waiting));
}

/// The owner's config directory (`~/.config/aterm`) on the Mac where 0.105.0 prompted for
/// Downloads access.
const OWNER_CONFIG_0_105: &[&str] = &[
    "connections-first-use",
    "kitty-collectibles.toml",
    "kitty-collectibles.toml.lock",
    "kitty-log.toml",
    "kitty-log.toml.lock",
    "privacy-debugger-observed",
];

/// Its state directory (`~/Library/Application Support/aterm`) then, sockets, the control
/// token and the journal left out; a trailing `/` is a directory.
const OWNER_STATE_0_105: &[&str] = &[
    ".cell-metrics.toml.aterm-restore.lock",
    ".session.toml.aterm-restore.lock",
    "Updates/",
    "cell-metrics.toml",
    "claims/",
    "devtools-pane-opened",
    "drafts/",
    "drive/",
    "edges/",
    "graph/",
    "harness/",
    "integration/",
    "pkg/",
    "rekey/",
    "spare/",
    "spare-gate-v1.tsv",
];

/// A Developer ID designated requirement, as the released bundle carries it.
const DEVELOPER_ID_DR: &str = "identifier \"com.aterm.aterm\" and anchor apple generic and \
     certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and certificate \
     leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and certificate leaf[subject.OU] = \
     A66A9P66Z7";

/// Main as the update successor of 0.105.0 hands Full Disk Access over first.
#[test]
fn main_as_the_successor_of_0_105_0_on_the_owners_mac_hands_full_disk_access_over_first() {
    use crate::consent_card::{
        AccessRecord, CardFacts, LAUNCH_SETTLE, LaunchVerdict, NotDue, decide_launch, read_marker,
        read_record, verified_here,
    };
    use crate::consent_launch::{
        FdaLaunch, WarmupFacts, WarmupStep, answered_at, decide_warmup, hands_over,
    };
    use crate::devtools_helper::{
        Admit, FdaFacts, HandOver, Lane, Moment, Origin, admit_fda, quiet as quiet_moment,
        test_arms,
    };
    use aterm_containment::consent::{
        ERRNO_ENOENT, ERRNO_EPERM, ProbeGate, ProbeOutcome, SYSTEM_TCC_DB, classify_dr,
        probe_fda_with,
    };
    use atpkg::devtools::{DEVELOPER_TOOLS, FULL_DISK_ACCESS, REOPEN_AFTER_S};
    use std::path::Path;
    const BUNDLE: &str = "com.aterm.aterm";
    const HERE: &str = "5B1E0C2A-7F3D-4E8B-9A61-0D2C4F6E8A10";
    let opened: u64 = 1_792_226_100;
    let started = opened + 120;

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let config = root.path().join("config");
    let state = root.path().join("state");
    for dir in [&config, &state] {
        std::fs::create_dir_all(dir).expect("a scratch directory");
    }
    for name in OWNER_CONFIG_0_105 {
        let text = if *name == "privacy-debugger-observed" {
            format!("enabled {} unknown\n", opened - 90)
        } else {
            String::new()
        };
        std::fs::write(config.join(name), text).expect("a config file");
    }
    for name in OWNER_STATE_0_105 {
        if let Some(dir) = name.strip_suffix('/') {
            std::fs::create_dir_all(state.join(dir)).expect("a state directory");
        } else {
            let text = if *name == "devtools-pane-opened" {
                format!("0.105.0 {opened}\n")
            } else {
                String::new()
            };
            std::fs::write(state.join(name), text).expect("a state file");
        }
    }
    let toml = config.join("aterm.toml");

    let marker = read_marker(Some(&toml));
    let record = read_record(Some(&toml));
    let here = verified_here(Some(&toml), Some(HERE), Some(BUNDLE));
    assert_eq!(
        (marker, record, here),
        (None, AccessRecord::default(), None)
    );
    let probe = probe_fda_with(
        ProbeGate::on(),
        Some(Path::new("/Applications/aterm.app/Contents/MacOS/aterm")),
        Some(Path::new("/Users//owner")),
        |db| {
            ProbeOutcome::Errno(if db == Path::new(SYSTEM_TCC_DB) {
                ERRNO_EPERM
            } else {
                ERRNO_ENOENT
            })
        },
    );
    assert_eq!(probe.state, FdaState::Denied);
    let dr = classify_dr(DEVELOPER_ID_DR);
    assert!(dr.grant_stable());
    let fda_word = format!("{}@{BUNDLE}@{HERE}", atpkg::devtools::revision_key(1));
    assert_eq!(
        DEVELOPER_TOOLS.last_opened_in(&state),
        Some(("0.105.0".to_owned(), opened))
    );
    assert_eq!(FULL_DISK_ACCESS.last_opened_in(&state), None);
    let facts_at = |now: u64, own: bool| FdaFacts {
        fda: probe.state,
        declined: FULL_DISK_ACCESS.declined_until_in(&state, now).is_some()
            || marker == Some(Marker::NotNow),
        grant_stable: dr.grant_stable(),
        recent: FULL_DISK_ACCESS
            .opened_within_in(&state, REOPEN_AFTER_S, now)
            .is_some(),
        devtools_recent: DEVELOPER_TOOLS
            .opened_within_in(&state, REOPEN_AFTER_S, now)
            .is_some(),
        shown_this_revision: FULL_DISK_ACCESS
            .opened_as_in(&state, &fda_word)
            .is_some_and(|(word, _)| atpkg::devtools::same_step(&word, &fda_word)),
        own,
    };

    let verdict = decide_launch(
        &CardFacts {
            offered: true,
            headless: false,
            fda: probe.state,
            dr,
        },
        Duration::ZERO,
        marker,
        &record,
    );
    assert_eq!(verdict, LaunchVerdict::Quiet(NotDue::NoNeedYet));
    assert!(hands_over(probe.state, &verdict, here));
    for own in [true, false] {
        assert_eq!(
            admit_fda(Origin::Automatic, Moment::Asked, &facts_at(started, own)),
            Admit::Queue,
            "own {own}"
        );
    }
    let answered = answered_at(&state.join("privacy-warmup-answered"), BUNDLE);
    assert_eq!(answered, Some(false));
    let warmup = WarmupFacts {
        automatic: true,
        fda_launch: FdaLaunch::after(HandOver::Queued, false),
        fda: probe.state,
        probe_pending: false,
        grant_stable: dr.grant_stable(),
        answered,
        walking: false,
    };
    assert_eq!(decide_warmup(&warmup), WarmupStep::Wait);

    let mut lane = Lane::windowed_with_fda_for_test(
        test_arms::opening(),
        test_arms::fda_opening(),
        test_arms::showing(),
    );
    let born = Instant::now();
    lane.queue(Origin::Automatic, born);
    #[expect(clippy::disallowed_methods, reason = "the lane's own test")]
    lane.queue_fda(Origin::Automatic, born);
    assert!(lane.fda_hand_over_due(born));
    assert!(!lane.hand_over_due(born), "the Developer Tools page waits");
    let settle = LAUNCH_SETTLE.as_secs();
    let quiet_after = |secs: u64| {
        quiet_moment(
            true,
            false,
            true,
            lane.settled(born + Duration::from_secs(secs)),
            false,
        )
    };
    assert!(
        !quiet_after(settle - 1),
        "no quiet moment before LAUNCH_SETTLE"
    );
    assert!(quiet_after(settle));

    let hand = Admit::Hand {
        list_terminal: false,
    };
    let first_quiet = started + settle;
    assert!(first_quiet < opened + REOPEN_AFTER_S);
    assert_eq!(
        admit_fda(
            Origin::Automatic,
            Moment::Quiet,
            &facts_at(first_quiet, true)
        ),
        Admit::Queue
    );
    assert_eq!(
        admit_fda(
            Origin::Automatic,
            Moment::Quiet,
            &facts_at(opened + REOPEN_AFTER_S, true)
        ),
        hand
    );
    assert_eq!(
        admit_fda(
            Origin::Automatic,
            Moment::Quiet,
            &facts_at(opened + 3_600 + settle, true)
        ),
        hand
    );
}

std::thread_local! {
    /// How long ago the Developer Tools page came up, for [`devtools_opened_within`].
    static DEVTOOLS_AGO: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// The Developer Tools page's opened marker, aged by the test ([`DEVTOOLS_AGO`]).
fn devtools_opened_within(window_s: u64) -> Option<u64> {
    DEVTOOLS_AGO
        .with(std::cell::Cell::get)
        .filter(|ago| *ago < window_s)
}

/// A Developer Tools page opened two minutes before holds the launch's Full Disk Access page back
/// until its ten minutes are up.
#[test]
fn a_developer_tools_page_up_minutes_ago_holds_the_launch_page_back_and_never_drops_it() {
    use crate::consent_launch::FdaLaunch;
    use crate::devtools_helper::{Lane, Origin, test_arms};
    let now = Instant::now();
    DEVTOOLS_AGO.with(|ago| ago.set(Some(120)));
    let mut app = new_mac();
    app.devtools_helper = Lane::windowed_with_fda_for_test(
        test_arms::opened(devtools_opened_within),
        test_arms::fda_opening(),
        test_arms::showing(),
    );
    assert!(app.consent_card.begin_deciding(now));
    app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
    assert_eq!(app.devtools_helper.fda_queued(), Some(Origin::Automatic));
    assert_eq!(app.consent_launch.fda(), FdaLaunch::Waiting);
    let left = Duration::from_secs(atpkg::devtools::REOPEN_AFTER_S - 120);
    assert!(
        !app.devtools_helper
            .fda_hand_over_due(now + left - Duration::from_secs(1)),
        "its next look is when the ten minutes are up"
    );
    app.launch_warmup_look(now, quiet);
    assert!(!app.consent_launch.walking());

    DEVTOOLS_AGO.with(|ago| ago.set(Some(atpkg::devtools::REOPEN_AFTER_S)));
    let at = Instant::now() + left;
    let (opens, _, _) = test_arms::fda_counts();
    app.look_for_fda_moment(at, quiet);
    assert_eq!(test_arms::fda_counts().0, opens + 1);
    assert!(app.fda_panel_up());
}

/// An update's successor on the fork lane holds the hand-over and brings the page up once the
/// reading is the window's own.
#[test]
fn a_fork_lane_successor_gets_the_page_once_its_reading_is_its_own() {
    use crate::devtools_helper::{Origin, test_arms};
    let now = Instant::now();
    let mut app = new_mac();
    app.consent = foreign(denied());
    assert!(app.consent_card.begin_deciding(now));
    app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
    assert_eq!(app.devtools_helper.fda_queued(), Some(Origin::Automatic));
    app.consent = own(denied());
    let (opens, _, _) = test_arms::fda_counts();
    app.look_for_fda_moment(Instant::now(), quiet);
    assert_eq!(test_arms::fda_counts().0, opens + 1);
    assert!(app.fda_panel_up());
}

/// An update's successor with `entries` its predecessor raised: the successor reads the
/// predecessor's log at launch and adopts each open entry under the id it had there.
fn successor_holding(entries: &[&aterm_messages::exceptions::Entry]) -> App {
    use aterm_messages::{LogLine, MessageLog};
    let mut predecessor = App::headless_for_test();
    for entry in entries {
        predecessor.except(entry);
    }
    let lines: Vec<LogLine> = predecessor
        .messages
        .drain_shelved_for_persist()
        .into_iter()
        .map(|(line, _)| line)
        .collect();
    let mut log = MessageLog::empty();
    log.replay_all(lines);
    let now = Instant::now();
    let mut center = crate::messages_host::new_center(log, now);
    assert_eq!(
        center.adopt_digest(crate::messages_host::wall_stamp_now(), now),
        entries.len()
    );
    let mut app = new_mac();
    app.messages = center;
    app
}

/// The id of the open entry under exactly `key`.
fn open_entry(app: &App, key: &str) -> Option<aterm_messages::MessageId> {
    app.messages
        .digest()
        .entries
        .iter()
        .find(|e| e.key == key)
        .map(|e| e.id)
}

/// The card's word on Full Disk Access ends only its own entries: other reporters' `privacy.`
/// entries a seamless update's successor adopted (the vault join's, the Developer Tools line's)
/// survive a held grant, a *Not now* on record and a `[privacy]` switch turned off, and the join's
/// reporter restates its entry under the id it had. The card's own entry ends on each: fixed where
/// the probe read the grant, withdrawn where nothing read it.
#[test]
fn the_cards_word_ends_only_its_own_entries_across_an_update() {
    use crate::consent_card::{AccessRecord, PendingDecision};
    use crate::system_state_access::JOIN_PENDING_KEY;
    use aterm_messages::exceptions::{Entry, Family};
    use aterm_messages::need::{Reason, Then, Why};
    use aterm_messages::{Outcome, Retired};
    let join = Entry::new(
        Family::Privacy,
        "secrets",
        "join-pending",
        "This Mac needs vault approval",
        Why::OnlyYou(Reason::new("an admin Mac approves this Mac")),
        Then::step("On an admin Mac run aterm secrets approve ABCD-EFGH"),
    );
    let devtools = Entry::new(
        Family::Privacy,
        "devtools",
        "not-granted",
        "aterm is not in Developer Tools",
        Why::OnlyYou(Reason::new(
            "only you can add aterm to Developer Tools in System Settings",
        )),
        Then::step("Turn on aterm in Developer Tools"),
    );
    let fda = Entry::new(
        Family::Privacy,
        "fda",
        "needed",
        "aterm cannot read your files",
        Why::OnlyYou(Reason::new(
            "only you can turn on Full Disk Access in System Settings",
        )),
        Then::step("Turn on Full Disk Access"),
    );
    let fda_key = fda.key();
    let others = [JOIN_PENDING_KEY.to_string(), devtools.key()];
    /// What the card read before its word.
    #[derive(Clone, Copy)]
    enum Word {
        Held,
        NotNow,
        SwitchOff,
    }
    fn say(app: &mut App, word: Word) {
        let decision = match word {
            Word::Held => {
                app.consent = own(granted());
                PendingDecision {
                    record: AccessRecord {
                        verified_at: Some(1_790_000_000),
                        ..AccessRecord::default()
                    },
                    verified_here: Some(true),
                    ..PendingDecision::launch(None)
                }
            }
            Word::NotNow => PendingDecision::launch(Some(Marker::NotNow)),
            Word::SwitchOff => {
                app.config.privacy = Some(super::app_config::PrivacyConfig {
                    notice: Some(false),
                    ..Default::default()
                });
                app.tick_macos_access_card(Instant::now());
                return;
            }
        };
        assert!(app.consent_card.begin_deciding(Instant::now()));
        app.decide_macos_access_card(decision);
    }
    for word in [Word::Held, Word::NotNow, Word::SwitchOff] {
        let (label, how) = match word {
            Word::Held => (
                "the launch check that finds the grant held",
                Retired::Resolved(Outcome::Ok),
            ),
            Word::NotNow => (
                "the launch check that finds a Not now on record",
                Retired::Withdrawn,
            ),
            Word::SwitchOff => ("a [privacy] switch off", Retired::Withdrawn),
        };
        let mut app = successor_holding(&[&join, &devtools, &fda]);
        let adopted: Vec<_> = others
            .iter()
            .map(|key| open_entry(&app, key).expect("another reporter's entry is adopted"))
            .collect();
        let card = open_entry(&app, &fda_key).expect("the card's entry is adopted");
        say(&mut app, word);
        for (key, id) in others.iter().zip(&adopted) {
            assert_eq!(
                open_entry(&app, key),
                Some(*id),
                "{label}: {key} stands under its id"
            );
            assert!(
                app.messages.log().get(*id).is_some_and(|r| r.is_live()),
                "{label}: {key} is not ended in the log"
            );
        }
        assert_eq!(open_entry(&app, &fda_key), None, "{label}: the card's ends");
        assert_eq!(
            app.messages
                .log()
                .get(card)
                .and_then(|r| r.retired().cloned()),
            Some(how),
            "{label}: how the card's ends"
        );
        app.except(&join);
        assert_eq!(
            open_entry(&app, JOIN_PENDING_KEY),
            Some(adopted[0]),
            "{label}: raised again, it is the same entry"
        );
        assert_eq!(
            app.messages
                .log()
                .records()
                .filter(|r| r.key.as_deref() == Some(JOIN_PENDING_KEY))
                .count(),
            1,
            "{label}: nothing is posted anew"
        );
    }
}
