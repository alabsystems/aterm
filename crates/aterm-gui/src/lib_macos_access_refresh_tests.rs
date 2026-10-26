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
    app.tick_devtools_hand_over(now + Duration::from_secs(1));
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

/// A session's `aterm ctl devtools hand-over now` never takes the owner's Full Disk
/// Access panel away (nor, with it, the folder warm-up that follows a Not now).
#[test]
fn a_control_verbs_now_panel_waits_behind_full_disk_access() {
    use crate::devtools_helper::{HandOver, Origin, Page, Refusal, test_arms};
    let now = Instant::now();
    let mut app = launched(now);
    app.devtools_helper.note_reading(
        test_arms::reading(
            aterm_containment::DevToolGrant::NotGranted,
            test_arms::absent(2),
        ),
        now,
    );
    app.look_for_fda_moment(Instant::now(), quiet);
    assert!(app.fda_panel_up());
    assert_eq!(
        app.hand_devtools_to_owner(Origin::Ctl { now: true }),
        HandOver::Refused(Refusal::Recent)
    );
    assert_eq!(
        app.devtools_helper.panel().map(|panel| panel.page()),
        Some(Page::FullDiskAccess)
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
            .last_opened_in(&state)
            .is_some_and(|(word, _)| word == fda_word),
        own,
    };

    let verdict = decide_launch(
        &CardFacts {
            offered: true,
            headless: false,
            fda: probe.state,
            dr,
        },
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

/// The setup revision the window admits in these tests.
const REVISION: u64 = 7;

/// One alert as the stand-in was asked it: title, body and proceed button.
type Alert = (String, String, String);

thread_local! {
    /// Every alert the stand-in alert was asked, whole: System State's debugger
    /// step raises none.
    static DEBUGGER_ALERTS: std::cell::RefCell<Vec<Alert>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// The change workers the stand-in start started.
    static WORKERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The frames the stand-in System State window has presented.
    static FRAMES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn debugger_alerts() -> Vec<Alert> {
    DEBUGGER_ALERTS.with(|alerts| alerts.borrow().clone())
}

fn workers() -> usize {
    WORKERS.with(std::cell::Cell::get)
}

/// The stand-in alert: it notes what it was asked and answers yes. Nothing is
/// shown, and nothing on the step's path may ask it.
fn alert_says_yes(title: &str, body: &str, proceed: &str) -> Option<bool> {
    DEBUGGER_ALERTS.with(|alerts| {
        alerts
            .borrow_mut()
            .push((title.to_owned(), body.to_owned(), proceed.to_owned()));
    });
    Some(true)
}

/// The stand-in worker start: no thread and no process; the change stays under way
/// until the test ends it.
fn worker_started(
    _job: crate::debugger_access::DevtoolsJob,
) -> crate::debugger_access::DevtoolsSpawn {
    WORKERS.with(|count| count.set(count.get() + 1));
    crate::debugger_access::DevtoolsSpawn::Started
}

/// System State is the visible view of the focused window (window 1), which has
/// presented a frame since the last look, and no other macOS dialog is up.
fn page_up(_app: &App) -> crate::debugger_access::StepMoment {
    let frames = FRAMES.with(|frames| {
        frames.set(frames.get() + 1);
        frames.get()
    });
    crate::debugger_access::StepMoment {
        page: Some((crate::WindowId(1), frames)),
        dialog_up: false,
    }
}

/// The same page, with no frame presented since the last look.
fn page_still(_app: &App) -> crate::debugger_access::StepMoment {
    crate::debugger_access::StepMoment {
        page: Some((crate::WindowId(1), FRAMES.with(std::cell::Cell::get))),
        dialog_up: false,
    }
}

/// System State is not the visible view of the focused window: the person is in
/// a terminal tab, another app, or another window.
fn page_away(_app: &App) -> crate::debugger_access::StepMoment {
    crate::debugger_access::StepMoment {
        page: None,
        dialog_up: false,
    }
}

/// System State frontmost, and another macOS dialog of aterm's up.
fn other_dialog(app: &App) -> crate::debugger_access::StepMoment {
    crate::debugger_access::StepMoment {
        dialog_up: true,
        ..page_up(app)
    }
}

/// The debugger probe's answer on this stand-in Mac: debugger access off, the
/// account an admin, this aterm hardened — the facts under which aterm asks. It
/// reads as just taken until the hang detector's minute has passed.
fn debugger_reads_off_as(app: &mut App, hardened: aterm_containment::consent::SelfHardening) {
    use aterm_containment::consent::{
        DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel,
    };
    app.consent.publish_debugger_for_test(
        DebuggerProbe {
            mode: DebuggerMode::Disabled,
            member: DebuggerMember::Admin,
            label: DebuggerProbeLabel::StatusOk,
        },
        hardened,
    );
}

/// Debugger access off, an admin, this aterm hardened: the facts under which the step starts.
fn debugger_reads_off(app: &mut App) {
    debugger_reads_off_as(app, aterm_containment::consent::SelfHardening::Hardened);
}

fn plan(revision: u64) -> Option<crate::InitialSetupPermissionPolicy> {
    Some(crate::InitialSetupPermissionPolicy {
        revision,
        allowed: true,
    })
}

/// The window's own admission of setup revision `revision`, as the page's automatic answer to the
/// startup offer takes it.
fn window_admits(app: &mut App, revision: u64) {
    app.hold_initial_setup_permissions();
    app.note_system_state_apply(crate::system_state::Admission::Automatic);
    app.accepted_initial_setup_apply();
    app.observe_initial_setup_permissions(plan(revision));
    assert!(app.admitted_access().is_some(), "the standing yes");
}

/// The same plan admitted by a control client's Apply (`aterm ctl app act`), whose gesture
/// reached the page before the Apply was taken.
fn controller_admits(app: &mut App, revision: u64) {
    app.hold_initial_setup_permissions();
    app.with_gesture_source(true, App::note_system_state_touch);
    app.note_system_state_apply(crate::system_state::Admission::Automatic);
    app.accepted_initial_setup_apply();
    app.observe_initial_setup_permissions(plan(revision));
    assert!(app.admitted_access().is_none(), "no standing yes");
}

/// Queue the debugger step on a test App as a windowed macOS instance does, giving neither the
/// admission ([`window_admits`]) nor the probe ([`debugger_reads_off`]).
fn queue_debugger_step(app: &mut App, dir: &std::path::Path, host: fn() -> Option<&'static str>) {
    use crate::debugger_access::{DebuggerChangeLane, DebuggerStepQueue, RecordsRead};
    app.consent_card_config = Some(dir.join("aterm.toml"));
    app.debugger_change = DebuggerChangeLane::with_liveness(|_| false);
    app.debugger_change.settle_records(RecordsRead::default());
    app.debugger_change.poll(1);
    app.debugger_launch =
        DebuggerStepQueue::queued_for_test(host).recording_in(Some(dir.to_path_buf()));
    app.consent_gestures =
        crate::native_settings::ConsentGestures::for_test_with(alert_says_yes, worker_started);
}

/// A new Mac where both launch pages are over (Full Disk Access not offered, the
/// Developer Tools list read) and the ask is queued and may ask.
fn ready_to_start(dir: &std::path::Path) -> App {
    ready_to_start_at(dir, REVISION)
}

/// [`ready_to_start`] with the window's admission of setup revision `revision`.
fn ready_to_start_at(dir: &std::path::Path, revision: u64) -> App {
    use crate::devtools_helper::test_arms;
    let mut app = new_mac();
    app.consent_launch
        .set_fda(crate::consent_launch::FdaLaunch::NotAsked);
    app.devtools_helper.note_reading(
        test_arms::reading(
            aterm_containment::DevToolGrant::NotGranted,
            test_arms::absent(2),
        ),
        Instant::now(),
    );
    queue_debugger_step(&mut app, dir, || Some("READY-MAC"));
    window_admits(&mut app, revision);
    debugger_reads_off(&mut app);
    app
}

/// A named change to a test App.
type AppCase = (&'static str, fn(&mut App));

/// The next look's moment: past any look an earlier one put off, and past
/// [`crate::debugger_access::ANNOUNCE_FOR`].
fn later(at: &mut Instant) -> Instant {
    *at += crate::devtools_helper::RECHECK_EVERY + Duration::from_secs(1);
    assert!(
        crate::devtools_helper::RECHECK_EVERY + Duration::from_secs(1)
            >= crate::debugger_access::ANNOUNCE_FOR
    );
    *at
}

/// Two looks with System State up: the page names macOS's dialog, then the
/// change starts.
fn two_looks(app: &mut App, at: &mut Instant) {
    app.look_for_debugger_moment(later(at), page_up);
    app.look_for_debugger_moment(later(at), page_up);
}

/// The debugger row's latest outcome in the access record for `revision`, once
/// every line `app` asked for is written (off the UI thread).
fn access_outcome(
    app: &App,
    dir: &std::path::Path,
    revision: u64,
) -> Option<aterm_types::macos_setup::AccessOutcome> {
    app.debugger_launch.record_written_for_test();
    aterm_types::macos_setup::access_record::latest(
        dir,
        revision,
        aterm_types::macos_setup::Access::Debugger,
    )
}

/// A lane step that ends the change with `outcome`, as the park's re-probe does.
fn ended(outcome: crate::debugger_access::Outcome) -> crate::debugger_access::LaneStep {
    crate::debugger_access::LaneStep {
        outcome: Some(outcome),
        ..crate::debugger_access::LaneStep::default()
    }
}

/// The first launch on a new Mac opens the Full Disk Access page, then the Developer Tools page,
/// then starts the debugger change (macOS's dialog, no alert of aterm's), then walks the folders.
#[test]
fn a_new_mac_turns_debugger_access_on_after_both_pages_and_before_the_folders() {
    use crate::consent_launch::FdaLaunch;
    use crate::devtools_helper::{Moment, Origin, PanelEvent, test_arms};
    use aterm_types::macos_setup::{Access, AccessOutcome};
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let now = Instant::now();
    let mut t = now;
    let mut app = launched(now);
    queue_debugger_step(&mut app, root.path(), || Some("NEW-MAC"));
    window_admits(&mut app, REVISION);
    debugger_reads_off(&mut app);
    let config = root.path().join("aterm.toml");

    assert_eq!(app.consent_launch.fda(), FdaLaunch::Waiting);
    app.look_for_debugger_moment(later(&mut t), page_up);
    assert!(workers() == 0 && app.debugger_launch.queued());
    assert!(app.debugger_launch.next_deadline().is_some_and(|at| at > t));
    app.look_for_fda_moment(Instant::now(), quiet);
    assert!(app.fda_panel_up());
    app.look_for_debugger_moment(later(&mut t), page_up);
    assert_eq!(workers(), 0);
    app.devtools_helper.press_for_test(PanelEvent::NotNow);
    app.tick_devtools_hand_over(now);
    assert_eq!(
        app.consent_launch.fda(),
        FdaLaunch::Ended { owner_acted: false }
    );
    app.look_for_debugger_moment(later(&mut t), page_up);
    assert!(workers() == 0 && app.debugger_launch.queued());
    app.devtools_helper.note_reading(
        test_arms::reading(
            aterm_containment::DevToolGrant::NotGranted,
            test_arms::absent(2),
        ),
        Instant::now(),
    );
    app.devtools_helper.queue(Origin::Automatic, Instant::now());
    app.look_for_debugger_moment(later(&mut t), page_up);
    assert_eq!(workers(), 0);
    // Full Disk Access still reads off: the Developer Tools page waits for it.
    let opens = test_arms::opens();
    let (origin, listed) = app
        .devtools_helper
        .take_queued(Instant::now(), true)
        .expect("the Developer Tools hand-over at its moment");
    let _ = app.devtools_hand_over_carrying(origin, Moment::Quiet, listed);
    assert_eq!(
        test_arms::opens(),
        opens,
        "no Developer Tools page before Full Disk Access reads on"
    );
    // Full Disk Access reads on: the Developer Tools page and its panel.
    app.consent = ConsentState::with_cached_probe_for_test(granted());
    app.devtools_helper.queue(Origin::Automatic, Instant::now());
    let (origin, listed) = app
        .devtools_helper
        .take_queued(Instant::now(), true)
        .expect("the Developer Tools hand-over at its moment");
    let _ = app.devtools_hand_over_carrying(origin, Moment::Quiet, listed);
    assert_eq!(test_arms::opens(), opens + 1, "the Developer Tools page");
    assert!(app.devtools_helper.panel().is_some());
    app.look_for_debugger_moment(later(&mut t), page_up);
    assert_eq!(workers(), 0, "not over the panel");
    assert_eq!(
        app.system_state_access_step(),
        Some(Access::DeveloperTools),
        "the page names the panel that is up, not the debugger"
    );
    app.devtools_helper.press_for_test(PanelEvent::NotNow);
    app.tick_devtools_hand_over(now);
    assert!(app.devtools_helper.panel().is_none());

    app.consent = ConsentState::with_cached_probe_for_test(denied());
    debugger_reads_off(&mut app);
    app.launch_warmup_look(later(&mut t), quiet);
    assert!(
        !app.consent_launch.walking(),
        "the folders wait for the step"
    );
    assert!(app.consent_launch.next_deadline().is_some());

    assert_eq!(app.published_system_state_access_step(), None);
    app.look_for_debugger_moment(later(&mut t), page_up);
    assert_eq!(
        app.system_state_access_step(),
        Some(Access::Debugger),
        "the headline names macOS's password dialog"
    );
    assert_eq!(
        app.published_system_state_access_step(),
        Some(Access::Debugger),
        "published at once: the next frame carries the words, not the next poll's"
    );
    assert_eq!(workers(), 0, "not before a frame showed it");
    assert!(
        !crate::debugger_access::read_asked(&config, "NEW-MAC"),
        "nothing recorded yet"
    );
    // Then the change, once, with this Mac recorded first.
    app.look_for_debugger_moment(later(&mut t), page_up);
    assert_eq!(workers(), 1, "the change started");
    assert!(crate::debugger_access::read_asked(&config, "NEW-MAC"));
    assert!(app.debugger_change.under_way());
    assert!(!app.debugger_launch.queued(), "over for this process");
    assert_eq!(
        app.system_state_access_step(),
        Some(Access::Debugger),
        "named while macOS's dialog may be up"
    );
    assert_eq!(
        access_outcome(&app, root.path(), REVISION),
        Some(AccessOutcome::Handed)
    );
    assert!(debugger_alerts().is_empty(), "no alert of aterm's");
    assert_eq!(app.debugger_access_facts().refusal, None);
    app.look_for_debugger_moment(later(&mut t), page_up);
    assert_eq!(workers(), 1, "once");

    // No folder dialog over macOS's; once the change is over the folders walk.
    app.launch_warmup_look(later(&mut t), quiet);
    assert!(
        !app.consent_launch.walking(),
        "no folder dialog over macOS's"
    );
    let step = app.debugger_change.not_started();
    app.apply_debugger_step(step);
    assert!(!app.debugger_change.under_way());
    assert_eq!(app.system_state_access_step(), None, "the step is over");
    assert_eq!(
        app.published_system_state_access_step(),
        None,
        "and the page says so at once"
    );
    app.launch_warmup_look(later(&mut t), quiet);
    assert!(
        app.consent_launch.walking(),
        "then the folders are asked for"
    );
    let cached = app.consent_card_host.record;
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the access record reaches the config folder",
        || (super::consent_card::read_record(Some(&config)) == cached).then_some(()),
    );
}

/// The page names macOS's dialog on a presented frame first, and the change starts only at a later
/// look with the page up: a page with no new frame, or one the person left, starts nothing, and
/// leaving takes the words away at once.
#[test]
fn the_page_names_macos_dialog_for_a_presented_frame_before_the_change() {
    use aterm_containment::consent::DebuggerChange;
    use aterm_types::macos_setup::{Access, AccessOutcome};
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let config = root.path().join("aterm.toml");
    let mut t = Instant::now();
    let mut app = ready_to_start(root.path());
    app.look_for_debugger_moment(later(&mut t), page_up);
    assert_eq!(app.system_state_access_step(), Some(Access::Debugger));
    for _ in 0..3 {
        app.look_for_debugger_moment(later(&mut t), page_still);
        assert_eq!(workers(), 0, "no frame has shown it");
        assert!(
            app.debugger_launch
                .next_deadline()
                .is_some_and(|at| at > t && at <= t + crate::devtools_helper::PANEL_TICK),
            "looked at again soon"
        );
    }
    app.look_for_debugger_moment(later(&mut t), page_away);
    assert_eq!(app.system_state_access_step(), None, "the words go");
    assert_eq!(app.published_system_state_access_step(), None, "at once");
    assert_eq!(workers(), 0);
    assert!(app.debugger_launch.queued());
    assert!(!crate::debugger_access::read_asked(&config, "READY-MAC"));
    app.look_for_debugger_moment(later(&mut t), page_up);
    assert_eq!(workers(), 0, "named again first");
    assert_eq!(app.system_state_access_step(), Some(Access::Debugger));
    app.look_for_debugger_moment(later(&mut t), page_up);
    assert_eq!(workers(), 1);
    assert_eq!(app.system_state_access_step(), Some(Access::Debugger));
    app.apply_debugger_step(ended(crate::debugger_access::Outcome::Changed(
        DebuggerChange::On,
    )));
    assert_eq!(
        access_outcome(&app, root.path(), REVISION),
        Some(AccessOutcome::Granted)
    );
    assert!(debugger_alerts().is_empty());
}

/// Cancel in macOS's dialog settles the row declined for the revision, never retried in this
/// process or, with `ASKED_RECORD` naming this Mac, by a later launch.
#[test]
fn cancel_in_macos_dialog_settles_the_row_and_is_never_retried() {
    use aterm_containment::consent::DebuggerChange;
    use aterm_types::macos_setup::AccessOutcome;
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut t = Instant::now();
    let mut app = ready_to_start(root.path());
    two_looks(&mut app, &mut t);
    assert_eq!(workers(), 1);
    app.apply_debugger_step(ended(crate::debugger_access::Outcome::NotChanged(
        DebuggerChange::On,
    )));
    assert_eq!(
        access_outcome(&app, root.path(), REVISION),
        Some(AccessOutcome::Declined)
    );
    let step = app.debugger_change.not_started();
    app.apply_debugger_step(step);
    two_looks(&mut app, &mut t);
    assert_eq!(workers(), 1, "never retried");
    let mut next = ready_to_start(root.path());
    two_looks(&mut next, &mut t);
    assert_eq!(workers(), 1, "nor at the next launch");
    assert!(!next.debugger_launch.queued());
    assert!(debugger_alerts().is_empty());
}

/// A later revision on a Mac whose step is over still settles its row: after a cancel at R, the
/// next launch's admission of R + 1 starts nothing and records the row declined for R + 1, so that
/// revision's Permissions stage never waits on it; R's line stays.
#[test]
fn a_later_revision_after_a_cancel_records_the_row_declined() {
    use aterm_containment::consent::DebuggerChange;
    use aterm_types::macos_setup::AccessOutcome;
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut t = Instant::now();
    let mut app = ready_to_start(root.path());
    two_looks(&mut app, &mut t);
    assert_eq!(workers(), 1);
    app.apply_debugger_step(ended(crate::debugger_access::Outcome::NotChanged(
        DebuggerChange::On,
    )));
    assert_eq!(
        access_outcome(&app, root.path(), REVISION),
        Some(AccessOutcome::Declined)
    );

    let mut next = ready_to_start_at(root.path(), REVISION + 1);
    two_looks(&mut next, &mut t);
    assert_eq!(workers(), 1, "never retried");
    assert!(!next.debugger_launch.queued(), "over on this Mac");
    assert_eq!(
        access_outcome(&next, root.path(), REVISION + 1),
        Some(AccessOutcome::Declined),
        "the later revision's row is settled"
    );
    assert_eq!(
        access_outcome(&next, root.path(), REVISION),
        Some(AccessOutcome::Declined),
        "the earlier revision's line stays"
    );
    assert!(debugger_alerts().is_empty());
}

/// A later revision in this process settles the row too, with no relaunch: after a cancel at R,
/// the window's admission of R again queues nothing, and its admission of R + 1 queues the step,
/// which finds this Mac in the record, starts nothing and records the row declined for R + 1.
#[test]
fn a_later_revision_in_this_process_settles_the_row_with_no_second_dialog() {
    use aterm_containment::consent::DebuggerChange;
    use aterm_types::macos_setup::AccessOutcome;
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut t = Instant::now();
    let mut app = ready_to_start(root.path());
    two_looks(&mut app, &mut t);
    assert_eq!(workers(), 1);
    app.apply_debugger_step(ended(crate::debugger_access::Outcome::NotChanged(
        DebuggerChange::On,
    )));
    let step = app.debugger_change.not_started();
    app.apply_debugger_step(step);
    assert_eq!(
        access_outcome(&app, root.path(), REVISION),
        Some(AccessOutcome::Declined)
    );

    window_admits(&mut app, REVISION);
    assert!(
        !app.debugger_launch.queued(),
        "the same revision: its outcome stands"
    );
    window_admits(&mut app, REVISION + 1);
    assert!(app.debugger_launch.queued(), "a new approval looks again");
    two_looks(&mut app, &mut t);
    assert_eq!(workers(), 1, "no second dialog");
    assert!(!app.debugger_launch.queued(), "over for that revision");
    assert_eq!(
        access_outcome(&app, root.path(), REVISION + 1),
        Some(AccessOutcome::Declined),
        "the later revision's row is settled in this process"
    );
    assert_eq!(
        access_outcome(&app, root.path(), REVISION),
        Some(AccessOutcome::Declined),
        "the earlier revision's line stays"
    );
    assert!(debugger_alerts().is_empty());
}

/// The `[privacy]` switches turned off one at a time, each with a probe planted
/// for an hour (as [`new_mac`] plants it), and all of them on.
type Switches = (&'static str, fn() -> super::app_config::PrivacyConfig);
const SWITCHES_OFF: [Switches; 3] = [
    ("notice off", || super::app_config::PrivacyConfig {
        probe_interval_ms: Some(3_600_000),
        notice: Some(false),
        ..Default::default()
    }),
    ("checks off", || super::app_config::PrivacyConfig {
        probe_interval_ms: Some(3_600_000),
        check: Some(false),
        ..Default::default()
    }),
    ("privacy off", || super::app_config::PrivacyConfig {
        probe_interval_ms: Some(3_600_000),
        enabled: Some(false),
        ..Default::default()
    }),
];

fn switches_on() -> super::app_config::PrivacyConfig {
    super::app_config::PrivacyConfig {
        probe_interval_ms: Some(3_600_000),
        ..Default::default()
    }
}

/// A `[privacy]` switch off is a pause, never an answer: with `notice`, `check` or `enabled` off
/// the step starts nothing and records nothing (neither this Mac nor the revision's row), and once
/// every switch is on again it goes on for the same revision with no new approval. With no
/// admission yet the step parks, and an admission while a switch is off pauses it the same way.
#[test]
fn a_privacy_switch_off_pauses_the_debugger_step_and_records_nothing() {
    for (name, privacy) in SWITCHES_OFF {
        let root = crate::test_scratch::outlived_dir("aterm-access-");
        let config = root.path().join("aterm.toml");
        let mut t = Instant::now();
        let mut app = ready_to_start(root.path());
        app.config.privacy = Some(privacy());
        let before = workers();
        two_looks(&mut app, &mut t);
        two_looks(&mut app, &mut t);
        assert_eq!(workers(), before, "{name}: nothing started");
        assert!(app.debugger_launch.paused(), "{name}: paused");
        assert_eq!(
            access_outcome(&app, root.path(), REVISION),
            None,
            "{name}: no answer recorded for the revision"
        );
        assert!(
            !crate::debugger_access::read_asked(&config, "READY-MAC"),
            "{name}: this Mac never answered"
        );
        app.config.privacy = Some(switches_on());
        debugger_reads_off(&mut app);
        two_looks(&mut app, &mut t);
        assert_eq!(
            workers(),
            before + 1,
            "{name}: on again, the step goes on: macOS's dialog"
        );
        assert!(!app.debugger_launch.paused(), "{name}");

        // No admission yet: parked; the admission that comes pauses it.
        let root = crate::test_scratch::outlived_dir("aterm-access-");
        let mut app = new_mac();
        app.consent_launch
            .set_fda(crate::consent_launch::FdaLaunch::NotAsked);
        app.devtools_helper.note_reading(
            crate::devtools_helper::test_arms::reading(
                aterm_containment::DevToolGrant::NotGranted,
                crate::devtools_helper::test_arms::absent(2),
            ),
            Instant::now(),
        );
        queue_debugger_step(&mut app, root.path(), || Some("READY-MAC"));
        app.config.privacy = Some(privacy());
        let before = workers();
        two_looks(&mut app, &mut t);
        assert!(app.debugger_launch.parked(), "{name}: parked");
        window_admits(&mut app, REVISION);
        two_looks(&mut app, &mut t);
        assert!(app.debugger_launch.paused(), "{name}: admitted, and paused");
        assert_eq!(access_outcome(&app, root.path(), REVISION), None, "{name}");
        assert_eq!(workers(), before, "{name}: nothing started");
        app.config.privacy = Some(switches_on());
        debugger_reads_off(&mut app);
        two_looks(&mut app, &mut t);
        assert_eq!(workers(), before + 1, "{name}: on again, it starts");
    }
    assert!(debugger_alerts().is_empty());
}

/// A switch turned off while a kept answer waits pauses the step: the look that would spend the
/// answer reads the switches again, starts nothing, answers nothing and keeps the answer, and once
/// every switch is on again the change starts on it.
#[test]
fn a_switch_turned_off_while_a_kept_answer_waits_pauses_the_step() {
    use aterm_types::macos_setup::AccessOutcome;
    for (name, privacy) in SWITCHES_OFF {
        let mut t = Instant::now();
        let root = crate::test_scratch::outlived_dir("aterm-access-");
        let mut app = ready_to_start(root.path());
        let before = workers();
        let at = recorded_mid_start(&mut app, &mut t);
        app.look_for_debugger_moment(at, page_away);
        assert!(app.debugger_launch.starting(), "PRECONDITION: kept");
        assert_eq!(workers(), before, "PRECONDITION: nothing started yet");
        app.config.privacy = Some(privacy());
        debugger_reads_off(&mut app);
        two_looks(&mut app, &mut t);
        two_looks(&mut app, &mut t);
        assert_eq!(workers(), before, "{name}: the kept answer starts nothing");
        assert!(app.debugger_launch.paused(), "{name}: paused");
        assert!(app.debugger_launch.starting(), "{name}: its answer kept");
        assert_ne!(
            access_outcome(&app, root.path(), REVISION),
            Some(AccessOutcome::Declined),
            "{name}: nothing answers the revision"
        );
        app.config.privacy = Some(switches_on());
        debugger_reads_off(&mut app);
        two_looks(&mut app, &mut t);
        assert_eq!(
            workers(),
            before + 1,
            "{name}: on again, the kept answer starts the change"
        );
    }
}

/// Debugger access turned off while a kept answer waits is the row's wanted state (`TurnedOff`):
/// the look that spends the answer finds it off by the person's choice (the observed record says
/// it was allowed), starts nothing and settles the row declined. Control: with no such record the
/// change starts.
#[test]
fn debugger_access_turned_off_while_a_kept_answer_waits_starts_nothing() {
    use crate::debugger_access::{DebuggerChangeLane, ObservedMarker, Origin, RecordsRead};
    use aterm_containment::consent::DebuggerMode;
    use aterm_types::macos_setup::AccessOutcome;
    for turned_off in [true, false] {
        let mut t = Instant::now();
        let root = crate::test_scratch::outlived_dir("aterm-access-");
        let mut app = ready_to_start(root.path());
        let before = workers();
        let at = recorded_mid_start(&mut app, &mut t);
        app.look_for_debugger_moment(at, page_away);
        assert!(app.debugger_launch.starting(), "PRECONDITION: kept");
        // The shared record, read again: it was allowed, and it reads off now.
        app.debugger_change = DebuggerChangeLane::with_liveness(|_| false);
        app.debugger_change.settle_records(RecordsRead {
            observed: turned_off.then_some(ObservedMarker {
                mode: DebuggerMode::Enabled,
                since: 1_790_000_000,
                origin: Origin::Outside,
            }),
            ..RecordsRead::default()
        });
        app.debugger_change.poll(1);
        debugger_reads_off(&mut app);
        two_looks(&mut app, &mut t);
        if turned_off {
            assert_eq!(workers(), before, "turned off: nothing started");
            assert!(!app.debugger_launch.queued() && !app.debugger_launch.starting());
            assert_eq!(
                access_outcome(&app, root.path(), REVISION),
                Some(AccessOutcome::Declined),
                "turned off: the row's wanted state"
            );
        } else {
            assert_eq!(workers(), before + 1, "control: the change starts");
        }
    }
}

/// A new approval while the step's change is under way joins it: the admission of R + 1 while R's
/// dialog is up is handed at once, starts nothing more, and takes the change's outcome when it
/// ends.
#[test]
fn a_new_revision_while_the_change_runs_takes_its_outcome() {
    use aterm_containment::consent::DebuggerChange;
    use aterm_types::macos_setup::AccessOutcome;
    let cases = [
        (
            crate::debugger_access::Outcome::NotChanged(DebuggerChange::On),
            AccessOutcome::Declined,
        ),
        (
            crate::debugger_access::Outcome::Changed(DebuggerChange::On),
            AccessOutcome::Granted,
        ),
    ];
    for (ended_with, outcome) in cases {
        let root = crate::test_scratch::outlived_dir("aterm-access-");
        let mut t = Instant::now();
        let mut app = ready_to_start(root.path());
        let before = workers();
        two_looks(&mut app, &mut t);
        assert_eq!(workers(), before + 1);
        assert!(app.debugger_change.under_way());
        window_admits(&mut app, REVISION + 1);
        two_looks(&mut app, &mut t);
        assert_eq!(workers(), before + 1, "{outcome:?}: no second dialog");
        assert_eq!(
            access_outcome(&app, root.path(), REVISION + 1),
            Some(AccessOutcome::Handed),
            "{outcome:?}: R + 1 waits on the dialog that is up"
        );
        window_admits(&mut app, REVISION + 1);
        app.apply_debugger_step(ended(ended_with));
        for revision in [REVISION, REVISION + 1] {
            assert_eq!(
                access_outcome(&app, root.path(), revision),
                Some(outcome),
                "{outcome:?}: revision {revision}"
            );
        }
    }
    assert!(debugger_alerts().is_empty());
}

/// The step never starts while anything goes before it: the Full Disk Access decision not made or
/// its page waiting, the Developer Tools list unread, a hand-over waiting, a panel up, the owner's
/// recent decline, System State not frontmost, or another macOS dialog up.
#[test]
fn the_debugger_step_waits_behind_every_page_panel_and_moment() {
    use crate::consent_launch::FdaLaunch;
    use crate::devtools_helper::Origin;
    let blockers: [AppCase; 9] = [
        ("full disk access undecided", |app| {
            app.consent_launch.set_fda(FdaLaunch::Undecided);
        }),
        ("full disk access waiting for its moment", |app| {
            app.consent_launch.set_fda(FdaLaunch::Waiting);
            #[expect(clippy::disallowed_methods, reason = "the lane's own test")]
            app.devtools_helper
                .queue_fda(Origin::Automatic, Instant::now());
        }),
        ("the developer tools list unread", |app| {
            app.devtools_helper = crate::devtools_helper::Lane::windowed_with_fda_for_test(
                crate::devtools_helper::test_arms::opening(),
                crate::devtools_helper::test_arms::fda_opening(),
                crate::devtools_helper::test_arms::showing(),
            );
        }),
        ("a developer tools hand-over waiting", |app| {
            app.devtools_helper.queue(Origin::Automatic, Instant::now());
        }),
        ("a full disk access hand-over waiting", |app| {
            #[expect(clippy::disallowed_methods, reason = "the lane's own test")]
            app.devtools_helper
                .queue_fda(Origin::Automatic, Instant::now());
        }),
        ("a panel up", |app| {
            assert!(app.show_fda_helper(Origin::Settings, Instant::now()));
        }),
        ("the owner's recent decline", |app| {
            app.owner_quiet.lock().unwrap().note_decline();
        }),
        ("System State not frontmost", |_| {}),
        ("another macOS dialog up", |_| {}),
    ];
    for (name, block) in blockers {
        let root = crate::test_scratch::outlived_dir("aterm-access-");
        let mut t = Instant::now();
        let mut app = ready_to_start(root.path());
        block(&mut app);
        let moment: fn(&App) -> crate::debugger_access::StepMoment = match name {
            "System State not frontmost" => page_away,
            "another macOS dialog up" => other_dialog,
            _ => page_up,
        };
        let before = workers();
        app.look_for_debugger_moment(later(&mut t), moment);
        app.look_for_debugger_moment(later(&mut t), moment);
        assert_eq!(workers(), before, "{name}: nothing started");
        assert_ne!(
            app.system_state_access_step(),
            Some(aterm_types::macos_setup::Access::Debugger),
            "{name}: the debugger is not named"
        );
        assert!(app.debugger_launch.queued(), "{name}: still to come");
        assert!(
            app.debugger_launch.next_deadline().is_some_and(|at| at > t),
            "{name}: a later look"
        );
        assert!(
            !crate::debugger_access::read_asked(&root.path().join("aterm.toml"), "READY-MAC"),
            "{name}: nothing recorded"
        );
    }
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut t = Instant::now();
    let mut app = ready_to_start(root.path());
    let before = workers();
    two_looks(&mut app, &mut t);
    assert_eq!(workers(), before + 1, "unblocked: started");
    assert!(debugger_alerts().is_empty());
}

/// The owner's own Open Settings for Full Disk Access, over a grant that reads held,
/// takes the window's waiting hand-over with it and shows no panel: nothing is left to
/// say how that visit ends, so the launch counts the owner at the page.
#[test]
fn a_full_disk_access_hand_over_gone_with_no_panel_holds_nothing() {
    use crate::consent_launch::FdaLaunch;
    use crate::devtools_helper::{Origin, test_arms};
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut t = Instant::now();
    let mut app = launched(t);
    assert_eq!(app.consent_launch.fda(), FdaLaunch::Waiting);
    app.devtools_helper.note_reading(
        test_arms::reading(
            aterm_containment::DevToolGrant::NotGranted,
            test_arms::absent(2),
        ),
        Instant::now(),
    );
    queue_debugger_step(&mut app, root.path(), || Some("OWNER-MAC"));
    window_admits(&mut app, REVISION);
    app.consent = ConsentState::with_cached_probe_for_test(granted());
    debugger_reads_off(&mut app);
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
    let before = workers();
    two_looks(&mut app, &mut t);
    assert_eq!(workers(), before + 1, "started");

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = ready_to_start(root.path());
    app.consent_launch.set_fda(FdaLaunch::Waiting);
    let before = workers();
    two_looks(&mut app, &mut t);
    assert_eq!(workers(), before + 1, "waiting on nothing holds nothing");
}

/// The change starts only on a fresh reading of the rule.
#[test]
fn the_debugger_step_waits_for_a_fresh_reading_of_the_rule() {
    use crate::debugger_access::ASK_PROBE_FRESH;
    use aterm_containment::consent::{
        DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel, SelfHardening,
    };
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut t = Instant::now();
    let mut app = ready_to_start(root.path());
    let old = Duration::from_secs(120);
    app.consent.publish_debugger_aged_for_test(
        DebuggerProbe {
            mode: DebuggerMode::Disabled,
            member: DebuggerMember::Admin,
            label: DebuggerProbeLabel::StatusOk,
        },
        SelfHardening::Hardened,
        old,
    );
    let gate = app.config.privacy_probe_gate();
    app.look_for_debugger_moment(later(&mut t), page_away);
    assert_eq!(workers(), 0);
    assert!(!app.consent.debugger_in_flight(), "a wait starts no probe");
    assert!(
        app.consent
            .debugger_cached(gate)
            .and_then(|row| row.probe_age())
            .is_some_and(|age| age >= old),
        "nor retires the answer it waits on"
    );

    // One look at the moment: the probe it dispatches ends on its own thread,
    // so a second look at once would race it (this App's arm answers
    // `refused_disabled`, which drops the step).
    app.look_for_debugger_moment(later(&mut t), page_up);
    assert_eq!(workers(), 0, "nothing starts on an old reading");
    assert!(app.debugger_launch.queued(), "it looks again");
    assert!(
        app.consent
            .debugger_cached(gate)
            .is_none_or(|row| row.probe_age().is_none_or(|age| age < ASK_PROBE_FRESH)),
        "the old answer is retired and the rule read again"
    );

    // The fresh reading the next look admits on, once that probe has ended.
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the debugger probe the look dispatched ends",
        || (!app.consent.debugger_in_flight()).then_some(()),
    );
    debugger_reads_off(&mut app);
    two_looks(&mut app, &mut t);
    assert_eq!(workers(), 1, "a fresh reading starts it");
}

/// A change on disk this lane never settles — here a peer's change lock held for good
/// — refuses each offer before anything is written, and the ask looks again; but only
/// for [`crate::debugger_access::REFUSED_FOR`] from the first refusal.
#[cfg(unix)]
#[test]
fn a_change_on_disk_holds_the_step_and_the_folders_for_a_bounded_span() {
    use crate::debugger_access::{LockTry, REFUSED_FOR, read_asked, take_change_lock};
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let config = root.path().join("aterm.toml");
    let mut t = Instant::now();
    let mut app = ready_to_start(root.path());
    let LockTry::Held(_peer) = take_change_lock(&config, Duration::ZERO) else {
        panic!("the scratch lock is free");
    };
    app.look_for_debugger_moment(later(&mut t), page_up);
    let first = later(&mut t);
    app.look_for_debugger_moment(first, page_up);
    assert!(app.debugger_launch.queued(), "refused: looked at again");
    assert!(
        app.debugger_launch
            .next_deadline()
            .is_some_and(|at| at > first)
    );
    assert_eq!(app.system_state_access_step(), None, "nothing named");
    app.launch_warmup_look(first, quiet);
    assert!(
        !app.consent_launch.walking(),
        "the folders wait for the step"
    );

    let mut over = first + REFUSED_FOR;
    app.look_for_debugger_moment(over, page_up);
    app.look_for_debugger_moment(later(&mut over), page_up);
    assert!(!app.debugger_launch.queued(), "over for this process");
    assert_eq!(workers(), 0, "nothing started");
    assert!(!read_asked(&config, "READY-MAC"), "nothing recorded");
    app.launch_warmup_look(over + Duration::from_secs(10), quiet);
    assert!(app.consent_launch.walking(), "the folders are asked for");
}

thread_local! {
    /// The `aterm-launch-host` stand-in's end, which [`cold_then_ended`] sets.
    static HOST_READ: std::cell::RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>> =
        const { std::cell::RefCell::new(None) };
}

/// The host key reads cold, and the read ends right after: the worker finishing
/// between the look's two loads.
fn cold_then_ended() -> Option<&'static str> {
    HOST_READ.with(|done| {
        if let Some(done) = done.borrow().as_ref() {
            done.store(true, std::sync::atomic::Ordering::Release);
        }
    });
    None
}

/// A host-key read that ends while a look reads the key cold is not a Mac the kernel
/// never names: that look waits, and only a later look that finds the read ended
/// with the key still cold drops the ask.
#[test]
fn a_host_read_ending_mid_look_is_not_a_mac_without_a_name() {
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut t = Instant::now();
    let mut app = ready_to_start(root.path());
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    HOST_READ.with(|slot| *slot.borrow_mut() = Some(std::sync::Arc::clone(&done)));
    app.debugger_launch =
        crate::debugger_access::DebuggerStepQueue::reading_host_for_test(cold_then_ended, done);
    app.look_for_debugger_moment(later(&mut t), page_up);
    assert!(app.debugger_launch.queued(), "a cold key waits");
    app.look_for_debugger_moment(later(&mut t), page_up);
    assert!(
        !app.debugger_launch.queued(),
        "ended with no key: no host to record"
    );
    HOST_READ.with(|slot| *slot.borrow_mut() = None);
}

/// On launch, an unread Mac line in the record starts its read on a background thread, and the
/// next look acts on the result.
#[test]
fn the_debugger_steps_record_is_read_off_the_event_loop() {
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let config = root.path().join("aterm.toml");
    crate::debugger_access::note_asked(&config, "READY-MAC", 1).expect("this Mac recorded");
    let mut t = Instant::now();
    let mut app = ready_to_start(root.path());
    app.debugger_launch =
        crate::debugger_access::DebuggerStepQueue::reading_record_for_test(|| Some("READY-MAC"));
    app.look_for_debugger_moment(later(&mut t), page_up);
    assert!(app.debugger_launch.queued(), "the look waits for the read");
    let began = Instant::now();
    while app.debugger_launch.asked("READY-MAC").is_none() {
        assert!(
            began.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "the record's read never ended"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    app.look_for_debugger_moment(later(&mut t), page_up);
    assert!(
        !app.debugger_launch.queued(),
        "started on this Mac already: dropped"
    );
    assert_eq!(workers(), 0, "nothing started");
}

/// On a launch the start writes this Mac off the event loop: the look that would start waits for
/// the write, then starts the change once, and a settle writes off the loop too. A step without the
/// launch's recording lane writes in the look, on this thread.
#[test]
fn the_debugger_steps_start_writes_this_mac_off_the_event_loop() {
    use crate::debugger_access::DebuggerStepQueue;
    use aterm_durable::{Flush, seam::Recording};
    use aterm_types::macos_setup::AccessOutcome;
    let flushes = Recording::start();
    let mut t = Instant::now();
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let config = root.path().join("aterm.toml");
    let mut app = ready_to_start(root.path());
    app.debugger_launch = DebuggerStepQueue::reading_record_for_test(|| Some("READY-MAC"))
        .recording_in(Some(root.path().to_path_buf()));
    let _ = flushes.take();
    app.look_for_debugger_moment(later(&mut t), page_up);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the record's read ends",
        || app.debugger_launch.asked("READY-MAC").map(|_| ()),
    );
    debugger_reads_off(&mut app);
    app.look_for_debugger_moment(later(&mut t), page_up);
    assert!(
        app.debugger_launch.announced(),
        "PRECONDITION: the page names the dialog"
    );
    let starting = later(&mut t);
    app.look_for_debugger_moment(starting, page_up);
    assert!(app.debugger_launch.queued(), "the look waits for the write");
    assert!(app.debugger_launch.starting(), "its disk half is out");
    assert_eq!(workers(), 0, "nothing started before the record");
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the disk half ends", || {
        app.debugger_launch.due(starting).then_some(())
    });
    assert!(crate::debugger_access::read_asked(&config, "READY-MAC"));
    app.look_for_debugger_moment(starting, page_up);
    assert_eq!(workers(), 1, "started, once, for the Mac it recorded");
    assert!(
        !app.debugger_launch.queued(),
        "started: over for this process"
    );
    assert_eq!(
        access_outcome(&app, root.path(), REVISION),
        Some(AccessOutcome::Handed)
    );
    assert!(
        !flushes.take().contains(&Flush::Durable),
        "nothing synced on the window's thread"
    );

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut inline = ready_to_start(root.path());
    let _ = flushes.take();
    two_looks(&mut inline, &mut t);
    assert_eq!(workers(), 2, "in the look: started at once");
    assert!(
        flushes.take().contains(&Flush::Durable),
        "in the look: synced on this thread"
    );

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let config = root.path().join("aterm.toml");
    let mut app = ready_to_start(root.path());
    app.debugger_launch = DebuggerStepQueue::reading_record_for_test(|| Some("READY-MAC"))
        .recording_in(Some(root.path().to_path_buf()));
    app.owner_quiet.lock().unwrap().note_debugger_answered();
    let _ = flushes.take();
    app.look_for_debugger_moment(later(&mut t), page_up);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the record's read ends",
        || app.debugger_launch.asked("READY-MAC").map(|_| ()),
    );
    app.look_for_debugger_moment(later(&mut t), page_up);
    assert!(!app.debugger_launch.queued(), "answered: settled");
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the settle's record lands",
        || crate::debugger_access::read_asked(&config, "READY-MAC").then_some(()),
    );
    assert_eq!(workers(), 2, "the settle starts nothing");
    assert!(
        !flushes.take().contains(&Flush::Durable),
        "the settle synced nothing on the window's thread"
    );

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut inline = ready_to_start(root.path());
    inline.owner_quiet.lock().unwrap().note_debugger_answered();
    let _ = flushes.take();
    inline.look_for_debugger_moment(later(&mut t), page_up);
    assert!(!inline.debugger_launch.queued(), "in the look: settled");
    assert!(
        flushes.take().contains(&Flush::Durable),
        "in the look: the settle synced on this thread"
    );
}

/// Brings `app` to a start whose disk half has just recorded this Mac, the page having named
/// macOS's dialog, and returns the moment of the look that waited for the write.
fn recorded_mid_start(app: &mut App, t: &mut Instant) -> Instant {
    app.look_for_debugger_moment(later(t), page_up);
    assert!(
        app.debugger_launch.announced(),
        "PRECONDITION: the page names the dialog"
    );
    let read = app.debugger_access_facts();
    let slot = app.debugger_launch.start_prepare(read).expect("none out");
    let at = later(t);
    app.look_for_debugger_moment(at, page_up);
    assert!(
        app.debugger_launch.preparing(),
        "PRECONDITION: the look waits"
    );
    let _ = slot.set(crate::debugger_access::StartPrep::Recorded);
    at
}

/// While the start's disk half is out the step only waits: a look then ends nothing, and once the
/// answer is in it is spent before anything is judged again.
#[test]
fn the_debugger_step_waits_out_its_disk_half_and_spends_it_first() {
    let mut t = Instant::now();
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut dropped = ready_to_start(root.path());
    dropped.consent_card_config = None;
    dropped.look_for_debugger_moment(later(&mut t), page_up);
    assert!(
        !dropped.debugger_launch.queued(),
        "no config folder: dropped"
    );

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = ready_to_start(root.path());
    app.look_for_debugger_moment(later(&mut t), page_up);
    let read = app.debugger_access_facts();
    let slot = app.debugger_launch.start_prepare(read).expect("none out");
    app.consent_card_config = None;
    let before = workers();
    let waiting = later(&mut t);
    app.look_for_debugger_moment(waiting, page_up);
    assert!(
        app.debugger_launch.queued(),
        "a write may still land: nothing ends"
    );
    assert!(app.debugger_launch.preparing());
    assert_eq!(workers(), before);
    let _ = slot.set(crate::debugger_access::StartPrep::Recorded);
    app.look_for_debugger_moment(waiting, page_up);
    assert_eq!(workers(), before + 1, "the recorded Mac gets its change");
    assert!(
        !app.debugger_launch.queued(),
        "started: over for this process"
    );
}

/// First-launch setup holds the debugger ask until its permissions choice is applied.
#[test]
fn a_held_first_launch_setup_holds_the_debugger_step() {
    use crate::InitialSetupPermissionPolicy as Policy;
    let mut t = Instant::now();
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let config = root.path().join("aterm.toml");
    let mut app = ready_to_start(root.path());
    app.hold_initial_setup_permissions();
    app.look_for_debugger_moment(later(&mut t), page_up);
    assert!(app.debugger_launch.queued(), "held: still to come");
    assert!(
        !app.debugger_launch.announced(),
        "held: the page names nothing"
    );
    assert_eq!(workers(), 0, "held: nothing started");
    assert!(
        !crate::debugger_access::read_asked(&config, "READY-MAC"),
        "held: no record"
    );
    app.accepted_initial_setup_apply();
    app.observe_initial_setup_permissions(Some(Policy {
        revision: REVISION + 1,
        allowed: false,
    }));
    two_looks(&mut app, &mut t);
    assert_eq!(workers(), 0, "the choice unchecked: held");
    assert!(app.debugger_launch.queued());
    window_admits(&mut app, REVISION + 2);
    two_looks(&mut app, &mut t);
    assert_eq!(workers(), 1, "admitted: started");
    assert!(crate::debugger_access::read_asked(&config, "READY-MAC"));
}

/// An answer given while the start records settles the step declined with nothing started;
/// unanswered, the change starts.
#[test]
fn an_answer_given_while_the_start_records_settles_the_step() {
    use aterm_types::macos_setup::AccessOutcome;
    let mut t = Instant::now();
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = ready_to_start(root.path());
    let at = recorded_mid_start(&mut app, &mut t);
    app.owner_quiet.lock().unwrap().note_debugger_answered();
    app.look_for_debugger_moment(at, page_up);
    assert_eq!(workers(), 0, "answered: nothing started");
    assert!(!app.debugger_launch.queued(), "answered: settled");
    assert_eq!(
        access_outcome(&app, root.path(), REVISION),
        Some(AccessOutcome::Declined)
    );

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut unanswered = ready_to_start(root.path());
    let at = recorded_mid_start(&mut unanswered, &mut t);
    unanswered.look_for_debugger_moment(at, page_up);
    assert_eq!(workers(), 1, "unanswered: started");
}

/// A wait that began while the start records defers the change, never drops it.
#[test]
fn a_wait_while_the_start_records_defers_its_change() {
    use crate::debugger_access::StepWait;
    use aterm_types::macos_setup::AccessOutcome;
    let mut t = Instant::now();
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = ready_to_start(root.path());
    let at = recorded_mid_start(&mut app, &mut t);
    app.owner_quiet.lock().unwrap().note_decline();
    app.look_for_debugger_moment(at, page_up);
    assert_eq!(workers(), 0, "quieted: nothing started yet");
    assert!(app.debugger_launch.queued(), "quieted: still to come");
    assert!(
        app.debugger_launch.starting() && !app.debugger_launch.preparing(),
        "the answer is kept"
    );
    assert!(
        !app.debugger_launch.announced(),
        "the page's words taken back"
    );
    assert!(
        !app.debugger_launch.due(at),
        "a kept answer waits for its time"
    );
    assert_eq!(
        app.debugger_launch.next_deadline(),
        Some(at + StepWait::OwnerQuiet.look_again())
    );
    *app.owner_quiet.lock().unwrap() = crate::native_settings::OwnerQuiet::default();
    app.look_for_debugger_moment(later(&mut t), page_away);
    assert_eq!(workers(), 0, "not frontmost: nothing started yet");
    assert!(app.debugger_launch.starting(), "still kept");
    two_looks(&mut app, &mut t);
    assert_eq!(workers(), 1, "the wait passed: started");
    assert!(
        !app.debugger_launch.queued(),
        "started: over for this process"
    );
    assert_eq!(
        access_outcome(&app, root.path(), REVISION),
        Some(AccessOutcome::Handed)
    );
}

/// The launch rule's stand-in reading, Admin.
fn rule_reads(
    mode: aterm_containment::consent::DebuggerMode,
) -> aterm_containment::consent::DebuggerProbe {
    use aterm_containment::consent::{DebuggerMember, DebuggerProbe, DebuggerProbeLabel};
    DebuggerProbe {
        mode,
        member: DebuggerMember::Admin,
        label: DebuggerProbeLabel::StatusOk,
    }
}

fn rule_reads_on(
    _: aterm_containment::consent::ProbeGate,
) -> aterm_containment::consent::DebuggerProbe {
    rule_reads(aterm_containment::consent::DebuggerMode::Enabled)
}

fn rule_reads_off(
    _: aterm_containment::consent::ProbeGate,
) -> aterm_containment::consent::DebuggerProbe {
    rule_reads(aterm_containment::consent::DebuggerMode::Disabled)
}

/// A `DevToolsSecurity -status` that read no rule, for `label`'s reason.
fn rule_read_fails(
    label: aterm_containment::consent::DebuggerProbeLabel,
) -> aterm_containment::consent::DebuggerProbe {
    use aterm_containment::consent::{DebuggerMember, DebuggerMode, DebuggerProbe};
    DebuggerProbe {
        mode: DebuggerMode::Unknown,
        member: DebuggerMember::Admin,
        label,
    }
}

fn rule_read_times_out(
    _: aterm_containment::consent::ProbeGate,
) -> aterm_containment::consent::DebuggerProbe {
    rule_read_fails(aterm_containment::consent::DebuggerProbeLabel::TimedOut)
}

fn rule_read_does_not_start(
    _: aterm_containment::consent::ProbeGate,
) -> aterm_containment::consent::DebuggerProbe {
    rule_read_fails(aterm_containment::consent::DebuggerProbeLabel::SpawnFailed)
}

/// A `-status` whose answer the classifier does not claim.
fn rule_read_unclaimed(
    _: aterm_containment::consent::ProbeGate,
) -> aterm_containment::consent::DebuggerProbe {
    rule_read_fails(aterm_containment::consent::DebuggerProbeLabel::StatusUnrecognized)
}

/// `-status` says the rights are in an unknown state: a reading, not a failed read.
fn rule_reads_unrecognized(
    _: aterm_containment::consent::ProbeGate,
) -> aterm_containment::consent::DebuggerProbe {
    rule_reads(aterm_containment::consent::DebuggerMode::Unrecognized)
}

/// An app whose recorded answer waited out a decline's quiet on a reading older than
/// [`crate::debugger_access::ASK_PROBE_FRESH`].
fn kept_on_an_aging_reading(
    app: &mut App,
    t: &mut Instant,
    read_again: fn(
        aterm_containment::consent::ProbeGate,
    ) -> aterm_containment::consent::DebuggerProbe,
) -> Instant {
    use crate::debugger_access::ASK_PROBE_FRESH;
    use aterm_containment::consent::{DebuggerMode, SelfHardening};
    let at = recorded_mid_start(app, t);
    app.owner_quiet.lock().unwrap().note_decline();
    app.look_for_debugger_moment(at, page_up);
    assert!(app.debugger_launch.starting(), "PRECONDITION: kept");
    *app.owner_quiet.lock().unwrap() = crate::native_settings::OwnerQuiet::default();
    app.consent.publish_debugger_aged_for_test(
        rule_reads(DebuggerMode::Disabled),
        SelfHardening::Hardened,
        ASK_PROBE_FRESH + Duration::from_secs(10),
    );
    app.consent.probes_for_test().debugger = read_again;
    app.debugger_launch.next_deadline().expect("a later look")
}

/// Look as the park does until a reading of the rule has landed, then give the
/// next look's time.
fn read_again_lands(app: &mut App) -> Instant {
    use crate::debugger_access::ASK_PROBE_FRESH;
    let gate = app.config.privacy_probe_gate();
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the rule read again", || {
        if app.consent.debugger_in_flight() {
            return None;
        }
        let landed = app.consent.debugger_cached(gate).is_some_and(|row| {
            !row.pending() && row.probe_age().is_some_and(|age| age < ASK_PROBE_FRESH)
        });
        if landed {
            return Some(());
        }
        let next = app.debugger_launch.next_deadline()?;
        app.look_for_debugger_moment(next, page_up);
        None
    });
    app.debugger_launch.next_deadline().expect("a later look")
}

/// Looks at the step's own deadlines from `next`, System State up, until it is over: the page names
/// macOS's dialog, then the change starts.
fn looks_from(app: &mut App, next: Instant) {
    let mut next = Some(next);
    for _ in 0..2 {
        let Some(at) = next else { return };
        app.look_for_debugger_moment(at, page_up);
        next = app.debugger_launch.next_deadline();
    }
}

/// A kept answer is spent on a fresh reading of the rule: once the decline's quiet has passed, an
/// old reading is retired and read again, and the change starts only over a rule still off.
#[test]
fn a_kept_answer_is_spent_on_a_fresh_reading_of_the_rule() {
    use crate::debugger_access::ASK_PROBE_FRESH;
    use aterm_types::macos_setup::AccessOutcome;
    type ReadAgain =
        fn(aterm_containment::consent::ProbeGate) -> aterm_containment::consent::DebuggerProbe;
    let cases = [
        (rule_reads_on as ReadAgain, 0, AccessOutcome::Granted),
        (rule_reads_off, 1, AccessOutcome::Handed),
    ];
    for (read_again, started, outcome) in cases {
        let mut t = Instant::now();
        let root = crate::test_scratch::outlived_dir("aterm-access-");
        let mut app = ready_to_start(root.path());
        let before = workers();
        let next = kept_on_an_aging_reading(&mut app, &mut t, read_again);
        app.look_for_debugger_moment(next, page_up);
        assert_eq!(workers(), before, "nothing starts on an old reading");
        assert!(app.debugger_launch.starting(), "kept for the fresh reading");
        let gate = app.config.privacy_probe_gate();
        assert!(
            app.consent
                .debugger_cached(gate)
                .is_none_or(|row| row.probe_age().is_none_or(|age| age < ASK_PROBE_FRESH)),
            "the old reading is retired"
        );
        let next = read_again_lands(&mut app);
        looks_from(&mut app, next);
        assert_eq!(
            workers(),
            before + started,
            "started only over a rule still off"
        );
        assert!(
            !app.debugger_launch.queued(),
            "over for this process either way"
        );
        assert_eq!(
            access_outcome(&app, root.path(), REVISION),
            Some(outcome),
            "the row says how it stands"
        );
    }
}

/// A failed read of the rule at the spend is retired and read again `RECHECK_EVERY` later.
#[test]
fn a_failed_read_of_the_rule_at_the_spend_is_read_again() {
    use crate::debugger_access::{REFUSED_FOR, UNREAD_READS};
    use crate::devtools_helper::RECHECK_EVERY;
    type ReadAgain =
        fn(aterm_containment::consent::ProbeGate) -> aterm_containment::consent::DebuggerProbe;
    let mut t = Instant::now();
    for fails in [
        rule_read_times_out as ReadAgain,
        rule_read_does_not_start,
        rule_read_unclaimed,
    ] {
        let root = crate::test_scratch::outlived_dir("aterm-access-");
        let mut app = ready_to_start(root.path());
        let before = workers();
        let next = kept_on_an_aging_reading(&mut app, &mut t, fails);
        app.look_for_debugger_moment(next, page_up);
        let failed_at = read_again_lands(&mut app);
        app.look_for_debugger_moment(failed_at, page_up);
        let label = fails(app.config.privacy_probe_gate()).label;
        assert_eq!(workers(), before, "{label:?}: nothing started");
        assert!(
            app.debugger_launch.queued() && app.debugger_launch.starting(),
            "{label:?}: the answer is kept"
        );
        assert_eq!(
            app.debugger_launch.next_deadline(),
            Some(failed_at + RECHECK_EVERY),
            "{label:?}: read again RECHECK_EVERY later"
        );
        let gate = app.config.privacy_probe_gate();
        assert!(
            app.consent.debugger_cached(gate).is_none(),
            "{label:?}: the failed read is retired"
        );
        app.consent.probes_for_test().debugger = rule_reads_off;
        let next = app.debugger_launch.next_deadline().expect("a later look");
        app.look_for_debugger_moment(next, page_up);
        assert_eq!(
            workers(),
            before,
            "{label:?}: nothing started before the rule is read"
        );
        let next = read_again_lands(&mut app);
        looks_from(&mut app, next);
        assert_eq!(workers(), before + 1, "{label:?}: read off, started");
        assert!(
            !app.debugger_launch.queued(),
            "{label:?}: started, over for this process"
        );
    }

    assert_eq!(UNREAD_READS, 3, "the case below fails three reads");
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = ready_to_start(root.path());
    let before = workers();
    let next = kept_on_an_aging_reading(&mut app, &mut t, rule_read_times_out);
    app.look_for_debugger_moment(next, page_up);
    let first = read_again_lands(&mut app);
    app.look_for_debugger_moment(first, page_up);
    assert!(app.debugger_launch.queued(), "the first failure: kept");
    app.look_for_debugger_moment(first + REFUSED_FOR, page_up);
    let next = read_again_lands(&mut app);
    app.look_for_debugger_moment(next, page_up);
    assert!(
        app.debugger_launch.queued() && app.debugger_launch.starting(),
        "two failures, the second after an absence: kept"
    );
    let next = app.debugger_launch.next_deadline().expect("a later look");
    app.look_for_debugger_moment(next, page_up);
    let next = read_again_lands(&mut app);
    app.look_for_debugger_moment(next, page_up);
    assert_eq!(workers(), before, "failures held: nothing started");
    assert!(
        !app.debugger_launch.queued(),
        "failures held for the span: over for this process"
    );

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = ready_to_start(root.path());
    let before = workers();
    let next = kept_on_an_aging_reading(&mut app, &mut t, rule_reads_unrecognized);
    app.look_for_debugger_moment(next, page_up);
    let next = read_again_lands(&mut app);
    app.look_for_debugger_moment(next, page_up);
    assert_eq!(workers(), before, "unknown state: nothing started");
    assert!(
        !app.debugger_launch.queued(),
        "unknown state: the lane refuses, over for this process"
    );

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = ready_to_start(root.path());
    let before = workers();
    let next = kept_on_an_aging_reading(&mut app, &mut t, rule_reads_off);
    app.config.privacy = Some(super::app_config::PrivacyConfig {
        check: Some(false),
        ..Default::default()
    });
    app.look_for_debugger_moment(next, page_up);
    assert_eq!(workers(), before, "the gate closed: nothing started");
    assert!(
        app.debugger_launch.paused() && app.debugger_launch.starting(),
        "the gate closed: a pause, the answer kept"
    );
}

/// First-launch setup's hold does not strand a start it caught mid-record.
#[test]
fn a_setup_hold_armed_while_the_start_records_does_not_strand_it() {
    use aterm_types::macos_setup::AccessOutcome;
    let fold = |app: &App, now: Instant| {
        let (mut deadline, mut owner) = (None, crate::deadline_heal::DeadlineFold::default());
        app.fold_consent_lane_deadlines(now, &mut deadline, &mut owner);
        deadline
    };
    let focus = |app: &mut App| {
        for ws in app.windows.values_mut() {
            ws.os_window_for_test = true;
            ws.focused = true;
        }
        assert!(
            app.any_os_window_focused(),
            "PRECONDITION: a focused window"
        );
    };
    let mut t = Instant::now();
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut held = ready_to_start(root.path());
    focus(&mut held);
    held.hold_initial_setup_permissions();
    let at = later(&mut t);
    held.look_for_debugger_moment(at, page_up);
    assert_eq!(workers(), 0, "held: nothing started");
    assert!(held.debugger_launch.queued(), "held: still to come");
    let look = at + Duration::from_secs(1);
    held.debugger_launch.look_later(look);
    assert_ne!(
        fold(&held, at),
        Some(look),
        "held, nothing started: no look folded for the step"
    );

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let config = root.path().join("aterm.toml");
    let mut app = ready_to_start(root.path());
    focus(&mut app);
    app.look_for_debugger_moment(later(&mut t), page_up);
    let read = app.debugger_access_facts();
    let slot = app.debugger_launch.start_prepare(read).expect("none out");
    let at = later(&mut t);
    app.look_for_debugger_moment(at, page_up);
    app.hold_initial_setup_permissions();
    let backstop = app.debugger_launch.next_deadline().expect("a later look");
    assert_eq!(fold(&app, at), Some(backstop), "held: the look still wakes");
    // The disk half recorded this Mac, as `StartDisk::prepare` does.
    crate::debugger_access::note_asked(&config, "READY-MAC", 1).expect("this Mac recorded");
    let _ = slot.set(crate::debugger_access::StartPrep::Recorded);
    app.look_for_debugger_moment(at, page_up);
    assert_eq!(workers(), 0, "held: no admission to spend");
    assert!(
        app.debugger_launch.parked() && app.debugger_launch.starting(),
        "parked, its answer kept"
    );
    assert_eq!(
        app.debugger_launch.next_deadline(),
        None,
        "parked: no look to wake"
    );
    window_admits(&mut app, REVISION + 1);
    two_looks(&mut app, &mut t);
    assert_eq!(
        workers(),
        1,
        "admitted again: the recorded Mac gets its change"
    );
    assert!(
        !app.debugger_launch.queued(),
        "started: over for this process"
    );
    assert_eq!(
        access_outcome(&app, root.path(), REVISION + 1),
        Some(AccessOutcome::Handed)
    );

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut kept = ready_to_start(root.path());
    focus(&mut kept);
    let at = recorded_mid_start(&mut kept, &mut t);
    kept.owner_quiet.lock().unwrap().note_decline();
    kept.look_for_debugger_moment(at, page_up);
    assert!(
        kept.debugger_launch.starting() && !kept.debugger_launch.preparing(),
        "PRECONDITION: kept, not out"
    );
    kept.hold_initial_setup_permissions();
    let next = at + Duration::from_secs(1);
    kept.debugger_launch.look_later(next);
    assert_eq!(
        fold(&kept, at),
        Some(next),
        "held: the kept look still wakes"
    );
}

/// A headless instance never queues the step, and one queued on it by hand drops at
/// first look, before any read: no probe dispatched, no record, no alert.
#[test]
fn a_headless_app_never_starts_or_probes() {
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = App::headless_for_test();
    assert!(!app.debugger_launch.queued(), "never queued headless");
    app.tick_consent_lanes(Instant::now());
    assert_eq!(workers(), 0);
    assert!(!crate::debugger_access::DebuggerStepQueue::new(false).queued());
    app.consent = ConsentState::with_cached_probe_for_test(denied());
    queue_debugger_step(&mut app, root.path(), || Some("HEADLESS"));
    window_admits(&mut app, REVISION);
    app.look_for_debugger_moment(Instant::now(), page_up);
    assert!(!app.debugger_launch.queued(), "dropped");
    assert_eq!(workers(), 0, "nothing started");
    assert!(app.consent.debugger_slot_idle_for_test(), "nothing probed");
    assert!(
        !root
            .path()
            .join(crate::debugger_access::ASKED_RECORD)
            .exists(),
        "nothing recorded"
    );
    assert!(
        !app.turn_on_debugger_from_system_state(true),
        "no window, no change"
    );
    assert_eq!(workers(), 0);
}

/// Once per Mac: a second launch on the same config folder starts nothing and reads no probe for
/// it; another Mac sharing that folder (a synced config) is asked once.
#[test]
fn a_second_launch_on_the_same_mac_starts_nothing() {
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut t = Instant::now();
    let mut first = ready_to_start(root.path());
    two_looks(&mut first, &mut t);
    assert_eq!(workers(), 1);

    let mut second = new_mac();
    second
        .consent_launch
        .set_fda(crate::consent_launch::FdaLaunch::NotAsked);
    queue_debugger_step(&mut second, root.path(), || Some("READY-MAC"));
    window_admits(&mut second, REVISION);
    two_looks(&mut second, &mut t);
    assert_eq!(workers(), 1, "started on this Mac already");
    assert!(!second.debugger_launch.queued());
    assert!(
        second.consent.debugger_slot_idle_for_test(),
        "the record decides before any probe"
    );

    let mut other = ready_to_start(root.path());
    other.debugger_launch =
        crate::debugger_access::DebuggerStepQueue::queued_for_test(|| Some("OTHER-MAC"))
            .recording_in(Some(root.path().to_path_buf()));
    two_looks(&mut other, &mut t);
    assert_eq!(workers(), 2, "another Mac on a synced config");
    assert!(debugger_alerts().is_empty());
}

/// Already on, seen go off (the shared record says it was on, and it reads off
/// now), or answered on the Settings page in this process: the question is answered
/// on this Mac.
#[test]
fn an_answered_question_settles_without_a_start() {
    use aterm_containment::consent::{
        DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel, SelfHardening,
    };
    use aterm_types::macos_setup::AccessOutcome;
    /// A named case: its name, the change, and what the access record says.
    type Case = (&'static str, fn(&mut App), Option<AccessOutcome>);
    let cases: [Case; 3] = [
        (
            "already on",
            |app| {
                app.consent.publish_debugger_for_test(
                    DebuggerProbe {
                        mode: DebuggerMode::Enabled,
                        member: DebuggerMember::Admin,
                        label: DebuggerProbeLabel::StatusOk,
                    },
                    SelfHardening::Hardened,
                );
            },
            Some(AccessOutcome::Granted),
        ),
        (
            "answered on the page",
            |app| {
                app.owner_quiet.lock().unwrap().note_debugger_answered();
            },
            Some(AccessOutcome::Declined),
        ),
        (
            "turned off",
            |app| {
                use crate::debugger_access::{
                    DebuggerChangeLane, ObservedMarker, Origin, RecordsRead,
                };
                app.debugger_change = DebuggerChangeLane::with_liveness(|_| false);
                app.debugger_change.settle_records(RecordsRead {
                    observed: Some(ObservedMarker {
                        mode: DebuggerMode::Enabled,
                        since: 1_790_000_000,
                        origin: Origin::Outside,
                    }),
                    ..RecordsRead::default()
                });
                app.debugger_change.poll(1);
            },
            Some(AccessOutcome::Declined),
        ),
    ];
    for (name, answered, outcome) in cases {
        let root = crate::test_scratch::outlived_dir("aterm-access-");
        let mut t = Instant::now();
        let mut app = ready_to_start(root.path());
        answered(&mut app);
        let before = workers();
        two_looks(&mut app, &mut t);
        assert_eq!(workers(), before, "{name}: nothing started");
        assert!(!app.debugger_launch.queued(), "{name}: settled");
        assert!(
            crate::debugger_access::read_asked(&root.path().join("aterm.toml"), "READY-MAC"),
            "{name}: recorded"
        );
        assert_eq!(
            access_outcome(&app, root.path(), REVISION),
            outcome,
            "{name}"
        );
    }
    assert!(debugger_alerts().is_empty());
}

/// Never from a control client: a plan admitted by a control client's Apply (`aterm ctl app act`),
/// or one only a status names, is no standing yes, so the step parks with nothing read, written or
/// started, and a control client's `Turn On…` is no person's press. Positive control: the window's
/// own admission of a later plan unparks it, and it starts.
#[test]
fn a_control_clients_act_never_starts_the_change() {
    use crate::workstation_setup::PagePress;
    use aterm_types::macos_setup::{Access, AccessOutcome};
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let config = root.path().join("aterm.toml");
    let mut t = Instant::now();
    let mut app = new_mac();
    app.consent_launch
        .set_fda(crate::consent_launch::FdaLaunch::NotAsked);
    app.devtools_helper.note_reading(
        crate::devtools_helper::test_arms::reading(
            aterm_containment::DevToolGrant::NotGranted,
            crate::devtools_helper::test_arms::absent(2),
        ),
        Instant::now(),
    );
    queue_debugger_step(&mut app, root.path(), || Some("READY-MAC"));
    debugger_reads_off(&mut app);
    controller_admits(&mut app, REVISION);
    two_looks(&mut app, &mut t);
    assert!(app.debugger_launch.parked(), "parked");
    assert!(
        !app.debugger_launch.queued(),
        "the folders do not wait on it"
    );
    assert_eq!(workers(), 0, "nothing started");
    assert!(!crate::debugger_access::read_asked(&config, "READY-MAC"));
    assert_eq!(app.system_state_access_step(), None);

    // A control client's Turn On…: its press is taken, and it is no person's.
    let by_person = app.with_gesture_source(true, |app| {
        app.note_system_state_touch();
        app.take_system_state_press() == PagePress::Person
    });
    assert!(!by_person);
    assert!(!app.open_access_from_system_state(Access::Debugger, by_person));
    assert_eq!(workers(), 0);

    // A status alone — a poll, a record edited by hand — admits nothing either.
    app.observe_initial_setup_permissions(plan(REVISION + 1));
    assert!(app.admitted_access().is_none() && app.debugger_launch.parked());
    two_looks(&mut app, &mut t);
    assert_eq!(workers(), 0);

    // The window's own admission of a later plan: unparked, and started.
    window_admits(&mut app, REVISION + 2);
    assert!(app.debugger_launch.queued() && !app.debugger_launch.parked());
    two_looks(&mut app, &mut t);
    assert_eq!(workers(), 1, "the window's own admission starts it");
    assert_eq!(
        access_outcome(&app, root.path(), REVISION + 2),
        Some(AccessOutcome::Handed)
    );
    assert_eq!(access_outcome(&app, root.path(), REVISION), None);
    assert!(debugger_alerts().is_empty());
}

/// Not needed on a build that is not hardened: the step drops with nothing recorded or started, and
/// a person's `Turn On…` starts nothing either.
#[test]
fn a_build_that_is_not_hardened_never_starts_it() {
    use aterm_containment::consent::SelfHardening;
    for hardening in [SelfHardening::NotHardened, SelfHardening::Unknown] {
        let root = crate::test_scratch::outlived_dir("aterm-access-");
        let mut t = Instant::now();
        let mut app = ready_to_start(root.path());
        debugger_reads_off_as(&mut app, hardening);
        two_looks(&mut app, &mut t);
        assert!(!app.debugger_launch.queued(), "{hardening:?}: dropped");
        assert!(
            !crate::debugger_access::read_asked(&root.path().join("aterm.toml"), "READY-MAC"),
            "{hardening:?}: nothing recorded"
        );
        assert!(
            !app.turn_on_debugger_from_system_state(true),
            "{hardening:?}: not needed"
        );
        assert_eq!(workers(), 0, "{hardening:?}: nothing started");
        assert_eq!(access_outcome(&app, root.path(), REVISION), None);
    }
}

/// A person's `Turn On…` on the Debugger row, while an admitted plan stands, is the yes whatever
/// the once-per-Mac record says: no alert of aterm's, and the page names macOS's dialog while the
/// change is under way. One change at a time; nothing without an admitted plan.
#[test]
fn a_persons_turn_on_starts_the_change_with_no_alert() {
    use crate::workstation_setup::PagePress;
    use aterm_types::macos_setup::{Access, AccessOutcome};
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let config = root.path().join("aterm.toml");
    crate::debugger_access::note_asked(&config, "READY-MAC", 1).expect("this Mac recorded");
    let mut app = ready_to_start(root.path());
    app.note_system_state_touch();
    let by_person = app.take_system_state_press() == PagePress::Person;
    assert!(by_person, "a person's touch, no control client's");
    assert!(app.open_access_from_system_state(Access::Debugger, by_person));
    assert_eq!(workers(), 1);
    assert!(app.debugger_change.under_way());
    assert_eq!(app.system_state_access_step(), Some(Access::Debugger));
    assert_eq!(
        access_outcome(&app, root.path(), REVISION),
        Some(AccessOutcome::Handed)
    );
    assert!(
        !app.turn_on_debugger_from_system_state(true),
        "one change at a time"
    );
    assert_eq!(workers(), 1);
    assert!(debugger_alerts().is_empty(), "no alert of aterm's");

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut unadmitted = new_mac();
    // `new_mac` stands a plan for the access steps; this Mac has none.
    unadmitted.observe_initial_setup_permissions(None);
    queue_debugger_step(&mut unadmitted, root.path(), || Some("READY-MAC"));
    debugger_reads_off(&mut unadmitted);
    assert!(
        !unadmitted.turn_on_debugger_from_system_state(true),
        "no plan admitted"
    );
    assert_eq!(workers(), 1);
}

/// System State's page, its Debugger row standing as `standing` in the setup worker's reading.
fn page_reads_debugger(app: &mut App, standing: aterm_types::macos_setup::Standing) {
    use aterm_types::macos_setup::{Access, Observation, Plan, Selection, Subject};
    let subject = Subject::Access(Access::Debugger);
    app.publish_system_state(crate::SetupStatus {
        observed_vault_url: Some(Plan::default().vault_url),
        plan: Some(Plan::from(Selection::all())),
        observed: vec![Observation {
            subject,
            standing,
            now: None,
            after_login: None,
            wanted: subject.wanted().to_string(),
            note: None,
            detail: None,
        }],
        ..crate::SetupStatus::default()
    });
}

/// Whether System State offers the Debugger row's `Turn On…`.
fn turn_on_offered(app: &App) -> bool {
    use crate::native_app::{AppKind, NativeApp};
    app.native_runtime
        .instance_by_kind(AppKind::SystemState)
        .is_some_and(|instance| {
            matches!(app.native_runtime.app(instance),
                Some(NativeApp::SystemState(page))
                    if page.can_open_access(aterm_types::macos_setup::Access::Debugger))
        })
}

/// A launch whose debugger step is over (this Mac was asked before Full Disk Access's Quit &
/// Reopen) with System State up and the window's plan admitted; the rule reads off and the build
/// hardened when the window reads them, and nothing is read yet.
fn relaunched_with_system_state_up(dir: &std::path::Path) -> App {
    use aterm_containment::consent::SelfHardening;
    crate::debugger_access::note_asked(&dir.join("aterm.toml"), "READY-MAC", 1)
        .expect("this Mac recorded");
    let mut app = new_mac();
    queue_debugger_step(&mut app, dir, || Some("READY-MAC"));
    window_admits(&mut app, REVISION);
    app.debugger_launch.end();
    app.consent.probes_for_test().debugger = rule_reads_off;
    app.consent.probes_for_test().hardening = || SelfHardening::Hardened;
    assert!(app.open_system_state_tab(), "PRECONDITION: System State up");
    app
}

/// While System State offers `Turn On…`, the window takes its own reading of debugger access
/// through the real tick, so the first press after a launch starts the change rather than finding
/// this aterm's hardening unread; with nothing offered it reads nothing.
#[test]
fn an_offered_turn_on_has_the_windows_reading_in_hand_for_the_first_press() {
    use crate::workstation_setup::PagePress;
    use aterm_containment::consent::{DebuggerMode, SelfHardening};
    use aterm_types::macos_setup::{Access, Standing};
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = relaunched_with_system_state_up(root.path());
    page_reads_debugger(&mut app, Standing::InPlace);
    assert!(!turn_on_offered(&app), "PRECONDITION: on already");
    app.tick_consent_lanes(Instant::now());
    assert!(
        app.consent.debugger_slot_idle_for_test(),
        "nothing offered, nothing read"
    );

    page_reads_debugger(&mut app, Standing::NeedsYou);
    assert!(turn_on_offered(&app), "PRECONDITION: Turn On… offered");
    app.tick_consent_lanes(Instant::now());
    let gate = app.config.privacy_probe_gate();
    let row = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the window's own reading of debugger access",
        || {
            app.consent
                .debugger_cached(gate)
                .filter(|row| !row.pending())
        },
    );
    assert_eq!(row.probe().mode, DebuggerMode::Disabled);
    assert_eq!(row.self_hardened(), SelfHardening::Hardened);

    app.note_system_state_touch();
    let by_person = app.take_system_state_press() == PagePress::Person;
    assert!(by_person, "a person's touch");
    assert!(
        app.open_access_from_system_state(Access::Debugger, by_person),
        "the first press starts the change"
    );
    assert_eq!(workers(), 1);

    // The control: the same press with no reading in hand starts nothing.
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut cold = relaunched_with_system_state_up(root.path());
    page_reads_debugger(&mut cold, Standing::NeedsYou);
    cold.note_system_state_touch();
    let by_person = cold.take_system_state_press() == PagePress::Person;
    assert!(by_person, "a person's touch");
    assert!(
        !cold.open_access_from_system_state(Access::Debugger, by_person),
        "a press before any reading starts nothing"
    );
    assert_eq!(workers(), 1);
}

/// What the window keeps while `Turn On…` is offered: a reading, however old, is kept (a press reads
/// the rule again past the slot's minute); a read that brought no reading is read again once it is
/// `RECHECK_EVERY` old and not before, never while a read is out, and never while the launch step,
/// which retired one, reads the rule again at that pace itself. The cases that must see no read
/// come before the first read this test dispatches, so the slot's retry floor (half a second after
/// each dispatch) cannot be what holds one back.
#[test]
fn an_offered_turn_on_reads_again_only_a_read_that_brought_no_reading() {
    use crate::debugger_access::StepWait;
    use crate::devtools_helper::RECHECK_EVERY;
    use aterm_containment::consent::{DebuggerMode, DebuggerProbeLabel, SelfHardening};
    use aterm_types::macos_setup::Standing;
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = relaunched_with_system_state_up(root.path());
    page_reads_debugger(&mut app, Standing::NeedsYou);
    assert!(turn_on_offered(&app), "PRECONDITION: Turn On… offered");
    let gate = app.config.privacy_probe_gate();
    let unread = rule_read_fails(DebuggerProbeLabel::TimedOut);
    let past_the_minute = Duration::from_secs(120);
    // Unchanged and none out: had one been dispatched it would still be out, or its reading (the
    // rule off, aged nothing) would have replaced `held`, published `aged` ago.
    let kept = |app: &App, held, aged| {
        !app.consent.debugger_in_flight()
            && app
                .consent
                .debugger_cached(gate)
                .is_some_and(|row| row.probe() == held && row.probe_age() >= Some(aged))
    };

    let old = rule_reads(DebuggerMode::Disabled);
    app.consent
        .publish_debugger_aged_for_test(old, SelfHardening::Hardened, past_the_minute);
    app.tick_consent_lanes(Instant::now());
    assert!(
        kept(&app, old, past_the_minute),
        "a reading in hand is kept, an old one too"
    );

    app.consent
        .publish_debugger_aged_for_test(unread, SelfHardening::Hardened, Duration::ZERO);
    app.tick_consent_lanes(Instant::now());
    assert!(
        kept(&app, unread, Duration::ZERO),
        "a read that just failed is not read again at once"
    );

    // The step's own ProbeUnread: it retires the failed row and looks again at RECHECK_EVERY.
    queue_debugger_step(&mut app, root.path(), || Some("READY-MAC"));
    app.consent.invalidate_debugger();
    app.debugger_launch
        .wait(StepWait::ProbeUnread, Instant::now() + RECHECK_EVERY);
    assert!(
        !app.debugger_launch.due(Instant::now()),
        "PRECONDITION: no look due"
    );
    app.tick_consent_lanes(Instant::now());
    assert!(
        !app.consent.debugger_in_flight() && app.consent.debugger_cached(gate).is_none(),
        "the step reads it again itself"
    );

    app.debugger_launch.end();
    app.consent.publish_debugger_aged_for_test(
        unread,
        SelfHardening::Hardened,
        RECHECK_EVERY + Duration::from_secs(1),
    );
    app.tick_consent_lanes(Instant::now());
    let row = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the failed read read again",
        || {
            app.consent
                .debugger_cached(gate)
                .filter(|row| !row.pending() && row.probe() != unread)
        },
    );
    assert_eq!(row.probe().mode, DebuggerMode::Disabled);

    // A read out is never retired, which would void it: the failed row it refreshes stands until
    // it lands. Past the slot's minute a read refreshes the row it still serves; the loop waits
    // out the retry floor the read above set.
    let held = READ_HELD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    app.consent.probes_for_test().debugger = rule_read_held;
    app.consent
        .publish_debugger_aged_for_test(unread, SelfHardening::Hardened, past_the_minute);
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "a read out", || {
        let _ = app.consent.debugger(gate);
        app.consent.debugger_in_flight().then_some(())
    });
    app.tick_consent_lanes(Instant::now());
    assert!(
        app.consent.debugger_in_flight()
            && app
                .consent
                .debugger_cached(gate)
                .is_some_and(|row| row.probe() == unread),
        "a read out is not retired"
    );
    drop(held);
    let row = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the read out landed",
        || {
            app.consent
                .debugger_cached(gate)
                .filter(|row| !row.pending() && row.probe() != unread)
        },
    );
    assert_eq!(row.probe().mode, DebuggerMode::Disabled);
}

/// Held by a test while [`rule_read_held`]'s read is to stay out.
static READ_HELD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A read of the rule that stays out while [`READ_HELD`] is held, then reads it off.
fn rule_read_held(
    gate: aterm_containment::consent::ProbeGate,
) -> aterm_containment::consent::DebuggerProbe {
    drop(
        READ_HELD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    rule_reads_off(gate)
}

/// A person's `Turn On…` answers the once-per-Mac question: their press writes this Mac into
/// `ASKED_RECORD` and ends the launch step, parked or queued, so a Cancel in macOS's dialog brings
/// no second dialog in this process or at the next launch.
#[test]
fn a_persons_turn_on_answers_the_question_so_a_cancel_brings_no_second_dialog() {
    use crate::workstation_setup::PagePress;
    use aterm_containment::consent::DebuggerChange;
    use aterm_types::macos_setup::Access;
    /// The person presses `Turn On…` and cancels macOS's dialog; macOS still reads
    /// off afterwards, freshly, so nothing waits on a stale reading.
    fn press_then_cancel(app: &mut App) {
        let before = workers();
        app.note_system_state_touch();
        let by_person = app.take_system_state_press() == PagePress::Person;
        assert!(by_person, "a person's touch");
        assert!(app.open_access_from_system_state(Access::Debugger, by_person));
        assert_eq!(workers(), before + 1, "the press starts the change");
        app.apply_debugger_step(ended(crate::debugger_access::Outcome::NotChanged(
            DebuggerChange::On,
        )));
        let step = app.debugger_change.not_started();
        app.apply_debugger_step(step);
        debugger_reads_off(app);
        // The quiet a decline buys is a bounded wait, not an answer: let it be over.
        *app.owner_quiet.lock().unwrap() = crate::native_settings::OwnerQuiet::default();
    }
    let mut t = Instant::now();

    // Parked: the window has admitted nothing of its own yet.
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let config = root.path().join("aterm.toml");
    let mut app = new_mac();
    app.consent_launch
        .set_fda(crate::consent_launch::FdaLaunch::NotAsked);
    app.devtools_helper.note_reading(
        crate::devtools_helper::test_arms::reading(
            aterm_containment::DevToolGrant::NotGranted,
            crate::devtools_helper::test_arms::absent(2),
        ),
        Instant::now(),
    );
    queue_debugger_step(&mut app, root.path(), || Some("READY-MAC"));
    debugger_reads_off(&mut app);
    two_looks(&mut app, &mut t);
    assert!(
        app.debugger_launch.parked(),
        "parked for want of a standing yes"
    );
    press_then_cancel(&mut app);
    window_admits(&mut app, REVISION);
    two_looks(&mut app, &mut t);
    assert_eq!(workers(), 1, "no second dialog in this process");
    assert!(
        !app.debugger_launch.parked() && !app.debugger_launch.queued(),
        "the step is over for this process"
    );
    assert!(
        crate::debugger_access::read_asked(&config, "READY-MAC"),
        "the press answered the question for this Mac"
    );
    let mut next = ready_to_start(root.path());
    two_looks(&mut next, &mut t);
    assert_eq!(workers(), 1, "nor at the next launch");
    assert!(!next.debugger_launch.queued());

    // Queued: the window admitted, the step not started yet.
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = ready_to_start(root.path());
    assert!(app.debugger_launch.queued());
    press_then_cancel(&mut app);
    two_looks(&mut app, &mut t);
    assert_eq!(workers(), 2, "the queued step starts no second dialog");
    assert!(!app.debugger_launch.queued());
    assert!(crate::debugger_access::read_asked(
        &root.path().join("aterm.toml"),
        "READY-MAC"
    ));
    assert!(debugger_alerts().is_empty(), "no alert of aterm's");
}

/// An update's successor with `entries` its predecessor raised: the successor reads the
/// predecessor's log at launch and adopts each open entry under the id it had there.
fn successor_holding(entries: &[&aterm_messages::exceptions::Entry]) -> App {
    use aterm_messages::{LogLine, MessageCenter, MessageLog};
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
    let mut center = MessageCenter::new(log, now);
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
