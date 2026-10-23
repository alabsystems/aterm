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

/// Whether the question stands: an entry of the digest, or a `privacy.fda` row an older parent carried across an update.
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
    assert!(access_row(&app), "the question is up");
    assert_eq!(app.consent_card.phase(), CardPhase::Watching { since: now });

    app.consent = ConsentState::with_cached_probe_for_test(FdaProbe::refused(ProbeLabel::Pending));
    app.tick_macos_access_card(now + Duration::from_secs(1));
    assert_eq!(app.consent_card.phase(), CardPhase::Watching { since: now });
    assert!(
        !grant_recorded(&app),
        "expired evidence cannot confirm access"
    );
    assert!(access_row(&app), "and the question stays up");

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
    app.raise_fda_need(&super::consent_card::NeedWords::sample(), now);
    assert!(access_row(&app));
    let id = app.post_message(super::message_reporters::file_access_question(
        &super::consent_card::NeedWords::sample(),
    ));
    app.consent_card_message = Some(id);
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
    assert!(access_row(&app));
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
    app
}

/// The quiet moment, as a test grants it (no window is focused in a test).
fn quiet(_app: &App, _now: Instant) -> bool {
    true
}

/// A moment that is never quiet: the owner is typing.
fn never_quiet(_app: &App, _now: Instant) -> bool {
    false
}

#[test]
fn setup_permission_hold_defers_queued_launch_decisions_and_automatic_page_effects() {
    use crate::devtools_helper::{HandOver, Origin, test_arms};
    let now = Instant::now();
    let mut app = new_mac();
    test_arms::stand_in(&mut app, test_arms::opening());
    app.hold_initial_setup_permissions();
    assert!(app.consent_card.begin_deciding(now));
    let decision = crate::consent_card::PendingDecision::launch(None);
    app.decide_macos_access_card(decision);
    assert_eq!(app.consent_card_host.pending, Some(decision));
    assert_eq!(app.consent_card.phase(), CardPhase::Deciding { since: now });
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

/// THE FIRST LAUNCH ON A NEW MAC: the launch check reads Full Disk Access off and never
/// verified, so the window's own hand-over waits for its quiet moment (and the folder
/// warm-up waits behind it, with no timer); at the moment, System Settings opens at Full
/// Disk Access — not Developer Tools — with aterm's panel; the owner's Not now notes that
/// page's decline, and the folder warm-up then walks at its own quiet moment.
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

/// Switching the Full Disk Access question off after the launch check answers the
/// waiting hand-over for this process: nothing opens, and the warm-up stops waiting.
#[test]
fn a_question_switched_off_drops_the_waiting_hand_over() {
    use crate::consent_launch::FdaLaunch;
    use crate::devtools_helper::test_arms;
    let now = Instant::now();
    let mut app = launched(now);
    app.config = aterm_toml::from_str("[privacy]\nnotice = false\n").expect("config");
    let (opens, _, _) = test_arms::fda_counts();
    app.tick_devtools_hand_over(now);
    app.look_for_fda_moment(Instant::now(), quiet);
    assert_eq!(app.devtools_helper.fda_queued(), None);
    assert_eq!(app.consent_launch.fda(), FdaLaunch::NotAsked);
    assert_eq!(test_arms::fda_counts().0, opens);
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

/// `probe`, read by a window another app started in-process: macOS names that app
/// (here launchd's pid) responsible, and the probe reads ITS grant.
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

/// A window started in-process by another terminal reads THAT terminal's grant (macOS
/// holds the terminal responsible): it is never recorded as aterm's, at launch or at a
/// waiting hand-over's quiet moment, so a later aterm of its own still gets the page.
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

/// 0.104's only answer to the Full Disk Access question was its Dismiss in "To look at":
/// inside that span the window's own page stays down, as after the panel's Not now.
#[test]
fn a_dismissed_full_disk_access_question_holds_the_launch_page_back() {
    use crate::consent_card::{NeedCause, NeedWords};
    use crate::consent_launch::FdaLaunch;
    let now = Instant::now();
    let mut app = new_mac();
    app.except(&crate::message_reporters::file_access_entry(&NeedWords {
        cause: NeedCause::ProtectedRead { session: None },
        tab_number: None,
        regressed: false,
    }));
    let id = app
        .messages
        .digest()
        .entries
        .iter()
        .find(|entry| entry.key.starts_with("privacy.fda."))
        .expect("the question is open")
        .id;
    assert!(
        app.messages
            .file_digest_one(id, crate::messages_host::wall_stamp_now(), now)
    );
    assert!(app.consent_card.begin_deciding(now));
    app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
    assert_eq!(app.devtools_helper.fda_queued(), None);
    assert_eq!(app.consent_launch.fda(), FdaLaunch::NotAsked);
}

/// The owner's config directory (`~/.config/aterm`) on the Mac where 0.105.0 drew "aterm
/// would like to access files in your Downloads folder" for an agent in a tab at 08:33
/// (macOS 27.0.1; listed that evening, 2026-10-17, names only).
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

/// MAIN AS THE UPDATE SUCCESSOR OF 0.105.0, ON THE OWNER'S MAC.
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
    let fda_word = format!("{}@{BUNDLE}@{HERE}", aterm_types::version::app_version());
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
        shown_this_version: FULL_DISK_ACCESS
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
    /// How long ago the Developer Tools page came up, for [`devtools_opened_within`].
    static DEVTOOLS_AGO: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// The Developer Tools page's opened marker, aged by the test ([`DEVTOOLS_AGO`]).
fn devtools_opened_within(window_s: u64) -> Option<u64> {
    DEVTOOLS_AGO
        .with(std::cell::Cell::get)
        .filter(|ago| *ago < window_s)
}

/// THE 0.105.0 UPDATE IN THE WINDOW: the Developer Tools page the outgoing process opened
/// two minutes before holds the launch's Full Disk Access page back until that page's ten
/// minutes are up, walks no folder meanwhile, and then brings it up; it never drops it.
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

/// AN UPDATE'S SUCCESSOR ON THE FORK LANE names its outgoing process responsible for the
/// seconds its launch check runs in (TCC design §2.2): the hand-over is held all the same,
/// and at its quiet moment, the outgoing process gone and the reading the window's own,
/// the page comes up.
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

/// One alert as the stand-in was asked it: title, body and proceed button.
type Alert = (String, String, String);

thread_local! {
    /// The debugger-access alerts the stand-in alert was asked, whole.
    static DEBUGGER_ALERTS: std::cell::RefCell<Vec<Alert>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn debugger_alerts() -> Vec<Alert> {
    DEBUGGER_ALERTS.with(|alerts| alerts.borrow().clone())
}

fn debugger_alert(title: &str, body: &str, proceed: &str) {
    DEBUGGER_ALERTS.with(|alerts| {
        alerts
            .borrow_mut()
            .push((title.to_owned(), body.to_owned(), proceed.to_owned()));
    });
}

/// The alert the launch ask raises on `app`'s stand-in Mac: the Security page's
/// debugger-access alert in its launch words, over the facts that Mac publishes,
/// and not the press's.
fn launch_alert(app: &mut App) -> Alert {
    use crate::native_settings::{DebuggerAlertFrom, debugger_access_alert};
    use aterm_containment::consent::DebuggerChange;
    let facts = app.debugger_access_facts();
    let alert = |from| {
        let (title, body, proceed) = debugger_access_alert(
            DebuggerChange::On,
            &facts,
            aterm_containment::SpikeEvidence::MEASURED,
            from,
        );
        (title, body, proceed.to_owned())
    };
    let launch = alert(DebuggerAlertFrom::Launch);
    assert_ne!(
        launch,
        alert(DebuggerAlertFrom::Press),
        "the launch words, not the press's"
    );
    launch
}

/// The stand-in alert, and the owner says no. Nothing is shown.
fn owner_says_no(title: &str, body: &str, proceed: &str) -> Option<bool> {
    debugger_alert(title, body, proceed);
    Some(false)
}

/// The stand-in alert, and the owner says yes. Nothing is shown.
fn owner_says_yes(title: &str, body: &str, proceed: &str) -> Option<bool> {
    debugger_alert(title, body, proceed);
    Some(true)
}

/// The stand-in worker start: no thread and no process; the change stays under way
/// until the test ends it.
fn worker_started(
    _job: crate::debugger_access::DevtoolsJob,
) -> crate::debugger_access::DevtoolsSpawn {
    crate::debugger_access::DevtoolsSpawn::Started
}

/// The debugger probe's answer on this stand-in Mac: debugger access off, the
/// account an admin, this aterm hardened — the facts under which aterm asks.
fn debugger_reads_off(app: &mut App) {
    use aterm_containment::consent::{
        DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel, SelfHardening,
    };
    app.consent.publish_debugger_for_test(
        DebuggerProbe {
            mode: DebuggerMode::Disabled,
            member: DebuggerMember::Admin,
            label: DebuggerProbeLabel::StatusOk,
        },
        SelfHardening::Hardened,
    );
}

/// Queue the launch ask on a test App as a windowed macOS instance does: a scratch
/// config folder (`dir`) for its record, the shared debugger records read, this Mac
/// named `host`, and the stand-in alert `confirm` and worker start.
fn queue_debugger_ask(
    app: &mut App,
    dir: &std::path::Path,
    host: fn() -> Option<&'static str>,
    confirm: fn(&str, &str, &str) -> Option<bool>,
) {
    use crate::debugger_access::{DebuggerChangeLane, LaunchAskQueue, RecordsRead};
    app.consent_card_config = Some(dir.join("aterm.toml"));
    app.debugger_change = DebuggerChangeLane::with_liveness(|_| false);
    app.debugger_change.settle_records(RecordsRead::default());
    app.debugger_change.poll(1);
    app.debugger_launch = LaunchAskQueue::queued_for_test(host);
    app.consent_gestures =
        crate::native_settings::ConsentGestures::for_test_with(confirm, worker_started);
}

/// A new Mac where both launch pages are over (Full Disk Access not offered, the
/// Developer Tools list read) and the ask is queued and may ask.
fn ready_to_ask(dir: &std::path::Path, confirm: fn(&str, &str, &str) -> Option<bool>) -> App {
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
    queue_debugger_ask(&mut app, dir, || Some("READY-MAC"), confirm);
    debugger_reads_off(&mut app);
    app
}

/// A named change to a test App.
type AppCase = (&'static str, fn(&mut App));

/// The next look's moment: past any look an earlier one put off.
fn later(at: &mut Instant) -> Instant {
    *at += crate::devtools_helper::RECHECK_EVERY + Duration::from_secs(1);
    *at
}

/// THE FIRST LAUNCH ON A NEW MAC, WHOLE: the Full Disk Access page, then the Developer
/// Tools page, then aterm's debugger-access alert (once, in its launch words, with this
/// Mac recorded before it), then the folder warm-up.
#[test]
fn a_new_mac_asks_for_debugger_access_after_both_pages_and_before_the_folders() {
    use crate::consent_launch::FdaLaunch;
    use crate::devtools_helper::{Moment, Origin, PanelEvent, test_arms};
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let now = Instant::now();
    let mut t = now;
    let mut app = launched(now);
    queue_debugger_ask(&mut app, root.path(), || Some("NEW-MAC"), owner_says_no);
    debugger_reads_off(&mut app);
    let config = root.path().join("aterm.toml");

    assert_eq!(app.consent_launch.fda(), FdaLaunch::Waiting);
    app.look_for_debugger_moment(later(&mut t), quiet);
    assert!(debugger_alerts().is_empty() && app.debugger_launch.queued());
    assert!(app.debugger_launch.next_deadline().is_some_and(|at| at > t));
    app.look_for_fda_moment(Instant::now(), quiet);
    assert!(app.fda_panel_up());
    app.look_for_debugger_moment(later(&mut t), quiet);
    assert!(debugger_alerts().is_empty());
    app.devtools_helper.press_for_test(PanelEvent::NotNow);
    app.tick_devtools_hand_over(now);
    assert_eq!(
        app.consent_launch.fda(),
        FdaLaunch::Ended { owner_acted: false }
    );
    app.look_for_debugger_moment(later(&mut t), quiet);
    assert!(debugger_alerts().is_empty() && app.debugger_launch.queued());
    app.devtools_helper.note_reading(
        test_arms::reading(
            aterm_containment::DevToolGrant::NotGranted,
            test_arms::absent(2),
        ),
        Instant::now(),
    );
    app.devtools_helper.queue(Origin::Automatic, Instant::now());
    app.look_for_debugger_moment(later(&mut t), quiet);
    assert!(debugger_alerts().is_empty());
    let opens = test_arms::opens();
    let (origin, listed) = app
        .devtools_helper
        .take_queued(Instant::now(), true)
        .expect("the Developer Tools hand-over at its moment");
    let _ = app.devtools_hand_over_carrying(origin, Moment::Quiet, listed);
    assert_eq!(test_arms::opens(), opens + 1, "the Developer Tools page");
    assert!(app.devtools_helper.panel().is_some());
    app.look_for_debugger_moment(later(&mut t), quiet);
    assert!(debugger_alerts().is_empty(), "not over the panel");
    app.devtools_helper.press_for_test(PanelEvent::NotNow);
    app.tick_devtools_hand_over(now);
    assert!(app.devtools_helper.panel().is_none());

    app.consent = ConsentState::with_cached_probe_for_test(denied());
    debugger_reads_off(&mut app);
    app.launch_warmup_look(later(&mut t), quiet);
    assert!(
        !app.consent_launch.walking(),
        "the folders wait for the alert"
    );
    assert!(app.consent_launch.next_deadline().is_some());

    app.look_for_debugger_moment(later(&mut t), quiet);
    assert_eq!(debugger_alerts(), [launch_alert(&mut app)]);
    assert!(crate::debugger_access::read_asked(&config, "NEW-MAC"));
    assert!(
        !app.debugger_launch.queued(),
        "asked: over for this process"
    );
    assert!(
        !app.debugger_change.under_way(),
        "the owner said no: nothing runs"
    );
    app.look_for_debugger_moment(later(&mut t), quiet);
    assert_eq!(debugger_alerts().len(), 1, "once");

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

/// The owner's yes starts the change a press starts — the same lane, the App's one
/// start through its injected worker arm — and a press queued behind the alert does
/// not ask again.
#[test]
fn a_yes_at_launch_starts_the_change_and_the_folders_wait_for_it() {
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let now = Instant::now();
    let mut t = now;
    let mut app = ready_to_ask(root.path(), owner_says_yes);
    app.look_for_debugger_moment(later(&mut t), quiet);
    assert_eq!(debugger_alerts().len(), 1);
    assert!(
        app.debugger_change.under_way(),
        "the worker re-reads the rule"
    );
    assert_eq!(app.debugger_access_facts().refusal, None);
    assert!(
        app.owner_quiet.lock().unwrap().has_confirmed_for_test(),
        "a press behind the alert does not ask again"
    );
    assert!(app.owner_quiet.lock().unwrap().debugger_answered());
    app.launch_warmup_look(later(&mut t), quiet);
    assert!(
        !app.consent_launch.walking(),
        "no folder dialog over macOS's"
    );
    let step = app.debugger_change.not_started();
    app.apply_debugger_step(step);
    assert!(!app.debugger_change.under_way());
    app.launch_warmup_look(later(&mut t), quiet);
    assert!(app.consent_launch.walking());
}

/// The alert never comes while anything goes before it: the Full Disk Access decision
/// not made or its page waiting, the Developer Tools list unread, a hand-over waiting,
/// a panel up, a moment that is not quiet, or the owner's recent decline.
#[test]
fn the_debugger_alert_waits_behind_every_page_panel_and_moment() {
    use crate::consent_launch::FdaLaunch;
    use crate::devtools_helper::Origin;
    let blockers: [AppCase; 8] = [
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
        ("not quiet", |_| {}),
    ];
    for (name, block) in blockers {
        let root = crate::test_scratch::outlived_dir("aterm-access-");
        let mut t = Instant::now();
        let mut app = ready_to_ask(root.path(), owner_says_no);
        block(&mut app);
        let moment: fn(&App, Instant) -> bool = if name == "not quiet" {
            |_, _| false
        } else {
            quiet
        };
        let before = debugger_alerts().len();
        app.look_for_debugger_moment(later(&mut t), moment);
        assert_eq!(debugger_alerts().len(), before, "{name}: no alert");
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
    let mut app = ready_to_ask(root.path(), owner_says_no);
    let before = debugger_alerts().len();
    app.look_for_debugger_moment(later(&mut t), quiet);
    assert_eq!(debugger_alerts().len(), before + 1, "unblocked: asked");
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
    queue_debugger_ask(&mut app, root.path(), || Some("OWNER-MAC"), owner_says_no);
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
    let before = debugger_alerts().len();
    app.look_for_debugger_moment(later(&mut t), quiet);
    assert_eq!(debugger_alerts().len(), before + 1, "asked");

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = ready_to_ask(root.path(), owner_says_no);
    app.consent_launch.set_fda(FdaLaunch::Waiting);
    let before = debugger_alerts().len();
    app.look_for_debugger_moment(later(&mut t), quiet);
    assert_eq!(
        debugger_alerts().len(),
        before + 1,
        "waiting on nothing holds nothing"
    );
}

/// THE ALERT IS RAISED ONLY ON A FRESH READING OF THE RULE.
#[test]
fn the_debugger_alert_waits_for_a_fresh_reading_of_the_rule() {
    use crate::debugger_access::ASK_PROBE_FRESH;
    use aterm_containment::consent::{
        DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel, SelfHardening,
    };
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut t = Instant::now();
    let mut app = ready_to_ask(root.path(), owner_says_yes);
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
    let before = debugger_alerts().len();
    app.look_for_debugger_moment(later(&mut t), |_, _| false);
    assert_eq!(debugger_alerts().len(), before);
    assert!(
        !app.consent.debugger_in_flight_for_test(),
        "a wait starts no probe"
    );
    assert!(
        app.consent
            .debugger_cached(gate)
            .and_then(|row| row.probe_age())
            .is_some_and(|age| age >= old),
        "nor retires the answer it waits on"
    );

    app.look_for_debugger_moment(later(&mut t), quiet);
    assert_eq!(
        debugger_alerts().len(),
        before,
        "no alert on an old reading"
    );
    assert!(app.debugger_launch.queued(), "it looks again");
    assert!(
        app.consent
            .debugger_cached(gate)
            .is_none_or(|row| row.probe_age().is_none_or(|age| age < ASK_PROBE_FRESH)),
        "the old answer is retired and the rule read again"
    );
}

/// A change on disk this lane never settles — here a peer's change lock held for good
/// — refuses each offer before anything is written, and the ask looks again; but only
/// for [`crate::debugger_access::REFUSED_FOR`] from the first refusal.
#[cfg(unix)]
#[test]
fn a_change_on_disk_holds_the_ask_and_the_folders_for_a_bounded_span() {
    use crate::debugger_access::{LockTry, REFUSED_FOR, read_asked, take_change_lock};
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let config = root.path().join("aterm.toml");
    let mut t = Instant::now();
    let mut app = ready_to_ask(root.path(), owner_says_yes);
    let LockTry::Held(_peer) = take_change_lock(&config, Duration::ZERO) else {
        panic!("the scratch lock is free");
    };
    let before = debugger_alerts().len();
    let first = later(&mut t);
    app.look_for_debugger_moment(first, quiet);
    assert!(app.debugger_launch.queued(), "refused: looked at again");
    assert!(
        app.debugger_launch
            .next_deadline()
            .is_some_and(|at| at > first)
    );
    app.launch_warmup_look(first, quiet);
    assert!(
        !app.consent_launch.walking(),
        "the folders wait for the ask"
    );

    let over = first + REFUSED_FOR;
    app.look_for_debugger_moment(over, quiet);
    assert!(!app.debugger_launch.queued(), "over for this process");
    assert_eq!(debugger_alerts().len(), before, "nobody asked");
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
    let mut app = ready_to_ask(root.path(), owner_says_no);
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    HOST_READ.with(|slot| *slot.borrow_mut() = Some(std::sync::Arc::clone(&done)));
    app.debugger_launch =
        crate::debugger_access::LaunchAskQueue::reading_host_for_test(cold_then_ended, done);
    app.look_for_debugger_moment(later(&mut t), quiet);
    assert!(app.debugger_launch.queued(), "a cold key waits");
    app.look_for_debugger_moment(later(&mut t), quiet);
    assert!(
        !app.debugger_launch.queued(),
        "ended with no key: no host to record"
    );
    HOST_READ.with(|slot| *slot.borrow_mut() = None);
}

/// THE RECORD IS READ OFF THE EVENT LOOP: on a launch, a look that finds this Mac's
/// line in the record unread starts its read on a thread of its own and waits, the
/// ask still queued, instead of reading the home folder in the window's turn (a home
/// on a network volume that stopped answering held the window there until the
/// server came back); once that read has ended, the next look acts on it — here a
/// record that names this Mac already, so the ask drops with no alert.
#[test]
fn the_launch_asks_record_is_read_off_the_event_loop() {
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let config = root.path().join("aterm.toml");
    crate::debugger_access::note_asked(&config, "READY-MAC", 1).expect("this Mac recorded");
    let mut t = Instant::now();
    let mut app = ready_to_ask(root.path(), owner_says_no);
    app.debugger_launch =
        crate::debugger_access::LaunchAskQueue::reading_record_for_test(|| Some("READY-MAC"));
    let before = debugger_alerts().len();
    app.look_for_debugger_moment(later(&mut t), quiet);
    assert!(app.debugger_launch.queued(), "the look waits for the read");
    let began = Instant::now();
    while app.debugger_launch.asked("READY-MAC").is_none() {
        assert!(
            began.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "the record's read never ended"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    app.look_for_debugger_moment(later(&mut t), quiet);
    assert!(
        !app.debugger_launch.queued(),
        "asked on this Mac already: dropped"
    );
    assert_eq!(debugger_alerts().len(), before, "no alert");
}

/// THE OFFER WRITES THIS MAC OFF THE EVENT LOOP, AND ITS ALERT FOLLOWS AT THE NEXT LOOK.
#[test]
fn the_launch_asks_record_is_written_off_the_event_loop() {
    use aterm_durable::{Flush, seam::Recording};
    let flushes = Recording::start();
    let mut t = Instant::now();
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let config = root.path().join("aterm.toml");
    let mut app = ready_to_ask(root.path(), owner_says_no);
    app.debugger_launch =
        crate::debugger_access::LaunchAskQueue::reading_record_for_test(|| Some("READY-MAC"));
    let before = debugger_alerts().len();
    let _ = flushes.take();
    app.look_for_debugger_moment(later(&mut t), quiet);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the record's read ends",
        || app.debugger_launch.asked("READY-MAC").map(|_| ()),
    );
    debugger_reads_off(&mut app);
    let asking = later(&mut t);
    app.look_for_debugger_moment(asking, quiet);
    assert!(app.debugger_launch.queued(), "the look waits for the write");
    assert_eq!(
        debugger_alerts().len(),
        before,
        "no alert before the record"
    );
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the disk half ends", || {
        app.debugger_launch.due(asking).then_some(())
    });
    assert!(crate::debugger_access::read_asked(&config, "READY-MAC"));
    app.look_for_debugger_moment(asking, never_quiet);
    assert_eq!(
        debugger_alerts().len(),
        before + 1,
        "asked, once, quiet or not"
    );
    assert_eq!(debugger_alerts().last(), Some(&launch_alert(&mut app)));
    assert!(
        !app.debugger_launch.queued(),
        "asked: over for this process"
    );
    assert!(
        !flushes.take().contains(&Flush::Durable),
        "nothing synced on the window's thread"
    );

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut inline = ready_to_ask(root.path(), owner_says_no);
    let _ = flushes.take();
    inline.look_for_debugger_moment(later(&mut t), quiet);
    assert_eq!(
        debugger_alerts().len(),
        before + 2,
        "in the look: asked at once"
    );
    assert!(
        flushes.take().contains(&Flush::Durable),
        "in the look: synced on this thread"
    );

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let config = root.path().join("aterm.toml");
    let mut app = ready_to_ask(root.path(), owner_says_yes);
    app.debugger_launch =
        crate::debugger_access::LaunchAskQueue::reading_record_for_test(|| Some("READY-MAC"));
    app.owner_quiet.lock().unwrap().note_debugger_answered();
    let _ = flushes.take();
    app.look_for_debugger_moment(later(&mut t), quiet);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the record's read ends",
        || app.debugger_launch.asked("READY-MAC").map(|_| ()),
    );
    app.look_for_debugger_moment(later(&mut t), quiet);
    assert!(!app.debugger_launch.queued(), "answered: settled");
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the settle's record lands",
        || crate::debugger_access::read_asked(&config, "READY-MAC").then_some(()),
    );
    assert_eq!(debugger_alerts().len(), before + 2, "no alert");
    assert!(
        !flushes.take().contains(&Flush::Durable),
        "the settle synced nothing on the window's thread"
    );

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut inline = ready_to_ask(root.path(), owner_says_yes);
    inline.owner_quiet.lock().unwrap().note_debugger_answered();
    let _ = flushes.take();
    inline.look_for_debugger_moment(later(&mut t), quiet);
    assert!(!inline.debugger_launch.queued(), "in the look: settled");
    assert!(
        flushes.take().contains(&Flush::Durable),
        "in the look: the settle synced on this thread"
    );
}

/// WHILE THE OFFER'S DISK HALF IS OUT, THE ASK ONLY WAITS: a look then ends nothing,
/// even one that would drop the ask (here, no config folder), so no write lands after
/// the ask is over; and once the answer is in, it is spent before anything is judged
/// again, so the Mac it recorded is asked.
#[test]
fn the_launch_ask_waits_out_its_disk_half_and_spends_it_first() {
    use crate::debugger_access::OfferPrep;
    let mut t = Instant::now();
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut dropped = ready_to_ask(root.path(), owner_says_no);
    dropped.consent_card_config = None;
    dropped.look_for_debugger_moment(later(&mut t), quiet);
    assert!(
        !dropped.debugger_launch.queued(),
        "no config folder: dropped"
    );

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = ready_to_ask(root.path(), owner_says_no);
    let read = app.debugger_access_facts();
    let slot = app.debugger_launch.start_prepare(read).expect("none out");
    app.consent_card_config = None;
    let before = debugger_alerts().len();
    let waiting = later(&mut t);
    app.look_for_debugger_moment(waiting, quiet);
    assert!(
        app.debugger_launch.queued(),
        "a write may still land: nothing ends"
    );
    assert!(app.debugger_launch.preparing());
    assert_eq!(debugger_alerts().len(), before);
    let _ = slot.set(OfferPrep::Recorded);
    app.look_for_debugger_moment(waiting, quiet);
    assert_eq!(
        debugger_alerts().len(),
        before + 1,
        "the recorded Mac is asked"
    );
    assert!(
        !app.debugger_launch.queued(),
        "asked: over for this process"
    );
}

/// FIRST-LAUNCH SETUP COMES FIRST: while it holds the macOS permissions back (its offer
/// not yet answered, cancelled, or applied with its permissions choice unchecked) the
/// ask does not look: no record, no alert, still queued.
#[test]
fn a_held_first_launch_setup_holds_the_debugger_ask() {
    use crate::InitialSetupPermissionPolicy as Policy;
    let mut t = Instant::now();
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let config = root.path().join("aterm.toml");
    let mut app = ready_to_ask(root.path(), owner_says_no);
    app.hold_initial_setup_permissions();
    let before = debugger_alerts().len();
    app.look_for_debugger_moment(later(&mut t), quiet);
    assert!(app.debugger_launch.queued(), "held: still to come");
    assert_eq!(debugger_alerts().len(), before, "held: no alert");
    assert!(
        !crate::debugger_access::read_asked(&config, "READY-MAC"),
        "held: no record"
    );
    app.accepted_initial_setup_apply();
    app.observe_initial_setup_permissions(Some(Policy {
        revision: 1,
        allowed: false,
    }));
    app.look_for_debugger_moment(later(&mut t), quiet);
    assert_eq!(
        debugger_alerts().len(),
        before,
        "the choice unchecked: held"
    );
    assert!(app.debugger_launch.queued());
    app.accepted_initial_setup_apply();
    app.observe_initial_setup_permissions(Some(Policy {
        revision: 2,
        allowed: true,
    }));
    app.look_for_debugger_moment(later(&mut t), quiet);
    assert_eq!(debugger_alerts().len(), before + 1, "applied: asked");
    assert!(crate::debugger_access::read_asked(&config, "READY-MAC"));
}

/// An app whose offer's disk half has just recorded this Mac: the look that
/// started it waited, and its answer is in.
fn recorded_mid_offer(app: &mut App, t: &mut Instant) -> Instant {
    let read = app.debugger_access_facts();
    let slot = app.debugger_launch.start_prepare(read).expect("none out");
    let at = later(t);
    app.look_for_debugger_moment(at, quiet);
    assert!(
        app.debugger_launch.preparing(),
        "PRECONDITION: the look waits"
    );
    let _ = slot.set(crate::debugger_access::OfferPrep::Recorded);
    at
}

/// AN ANSWER GIVEN WHILE THE OFFER RECORDS SETTLES THE ASK: the owner answered a
/// debugger-access alert after the offer was admitted (a Settings press, its change not
/// started yet), so the Mac just recorded gets no second alert and the ask is over, as
/// admission's settle for an answer says.
#[test]
fn an_answer_given_while_the_offer_records_settles_the_ask() {
    let mut t = Instant::now();
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = ready_to_ask(root.path(), owner_says_no);
    let at = recorded_mid_offer(&mut app, &mut t);
    app.owner_quiet.lock().unwrap().note_debugger_answered();
    let before = debugger_alerts().len();
    app.look_for_debugger_moment(at, quiet);
    assert_eq!(debugger_alerts().len(), before, "answered: no second alert");
    assert!(!app.debugger_launch.queued(), "answered: settled");

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut unanswered = ready_to_ask(root.path(), owner_says_no);
    let at = recorded_mid_offer(&mut unanswered, &mut t);
    unanswered.look_for_debugger_moment(at, quiet);
    assert_eq!(debugger_alerts().len(), before + 1, "unanswered: asked");
}

/// A DECLINE QUIET THAT BEGAN WHILE THE OFFER RECORDS DEFERS THE ALERT, NEVER DROPS IT:
/// a recent decline of any Security alert quiets this one too, so the recorded answer is
/// kept and the ask waits, still queued, for its time (not at the next wake); a look
/// once the quiet has passed raises the alert, whatever the moment.
#[test]
fn a_decline_quiet_while_the_offer_records_defers_its_alert() {
    use crate::debugger_access::AskWait;
    let mut t = Instant::now();
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = ready_to_ask(root.path(), owner_says_no);
    let at = recorded_mid_offer(&mut app, &mut t);
    app.owner_quiet.lock().unwrap().note_decline();
    let before = debugger_alerts().len();
    app.look_for_debugger_moment(at, quiet);
    assert_eq!(debugger_alerts().len(), before, "quieted: no alert yet");
    assert!(app.debugger_launch.queued(), "quieted: still to come");
    assert!(app.debugger_launch.offering(), "the answer is kept");
    assert!(
        !app.debugger_launch.due(at),
        "a kept answer waits for its time"
    );
    let next = app.debugger_launch.next_deadline().expect("a later look");
    assert_eq!(next, at + AskWait::OwnerQuiet.look_again());
    *app.owner_quiet.lock().unwrap() = crate::native_settings::OwnerQuiet::default();
    app.look_for_debugger_moment(next, never_quiet);
    assert_eq!(
        debugger_alerts().len(),
        before + 1,
        "the quiet passed: asked"
    );
    assert_eq!(debugger_alerts().last(), Some(&launch_alert(&mut app)));
    assert!(
        !app.debugger_launch.queued(),
        "asked: over for this process"
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

/// An app whose recorded answer waited out a decline's quiet, its reading now
/// older than [`crate::debugger_access::ASK_PROBE_FRESH`] — as a kept answer's
/// is, past `OWNER_QUIET_FIRST` — but younger than the slot's own refresh age,
/// so only the spend's retirement reads the rule again; the rule is read again
/// by `read_again`.
fn kept_on_an_aging_reading(
    app: &mut App,
    t: &mut Instant,
    read_again: fn(
        aterm_containment::consent::ProbeGate,
    ) -> aterm_containment::consent::DebuggerProbe,
) -> Instant {
    use crate::debugger_access::ASK_PROBE_FRESH;
    use aterm_containment::consent::{DebuggerMode, SelfHardening};
    let at = recorded_mid_offer(app, t);
    app.owner_quiet.lock().unwrap().note_decline();
    app.look_for_debugger_moment(at, quiet);
    assert!(app.debugger_launch.offering(), "PRECONDITION: kept");
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
        if app.consent.debugger_in_flight_for_test() {
            return None;
        }
        let landed = app.consent.debugger_cached(gate).is_some_and(|row| {
            !row.pending() && row.probe_age().is_some_and(|age| age < ASK_PROBE_FRESH)
        });
        if landed {
            return Some(());
        }
        let next = app.debugger_launch.next_deadline()?;
        app.look_for_debugger_moment(next, quiet);
        None
    });
    app.debugger_launch.next_deadline().expect("a later look")
}

/// A KEPT ANSWER'S ALERT STATES A FRESH READING OF THE RULE: once the decline quiet has
/// passed, a reading older than [`crate::debugger_access::ASK_PROBE_FRESH`] is retired
/// and the rule read again before any alert, as admission does, so a rule switched on
/// meanwhile (by the owner's own `sudo`, which retires no cached answer) is a press the
/// lane refuses: the ask ends with no alert, the record standing.
#[test]
fn a_kept_answer_is_spent_on_a_fresh_reading_of_the_rule() {
    use crate::debugger_access::ASK_PROBE_FRESH;
    type ReadAgain =
        fn(aterm_containment::consent::ProbeGate) -> aterm_containment::consent::DebuggerProbe;
    for (read_again, asked) in [(rule_reads_on as ReadAgain, 0), (rule_reads_off, 1)] {
        let mut t = Instant::now();
        let root = crate::test_scratch::outlived_dir("aterm-access-");
        let mut app = ready_to_ask(root.path(), owner_says_no);
        let next = kept_on_an_aging_reading(&mut app, &mut t, read_again);
        let before = debugger_alerts().len();
        app.look_for_debugger_moment(next, quiet);
        assert_eq!(
            debugger_alerts().len(),
            before,
            "no alert on an old reading"
        );
        assert!(app.debugger_launch.offering(), "kept for the fresh reading");
        let gate = app.config.privacy_probe_gate();
        assert!(
            app.consent
                .debugger_cached(gate)
                .is_none_or(|row| row.probe_age().is_none_or(|age| age < ASK_PROBE_FRESH)),
            "the old reading is retired"
        );
        let next = read_again_lands(&mut app);
        app.look_for_debugger_moment(next, quiet);
        assert_eq!(
            debugger_alerts().len(),
            before + asked,
            "asked only over a rule still off"
        );
        assert!(
            !app.debugger_launch.queued(),
            "over for this process either way"
        );
    }
}

/// A FAILED READ OF THE RULE AT THE SPEND IS NO ANSWER: a `-status` that timed out, did
/// not start or answered what the classifier does not claim reads nothing, and admission
/// drops such a reading with nothing written; this Mac is recorded already, so the failed
/// read is retired and the rule read again `RECHECK_EVERY` later, and a read that comes
/// back off raises the alert.
#[test]
fn a_failed_read_of_the_rule_at_the_spend_is_read_again() {
    use crate::debugger_access::{REFUSED_FOR, UNREAD_READS};
    use crate::devtools_helper::RECHECK_EVERY;
    type ReadAgain =
        fn(aterm_containment::consent::ProbeGate) -> aterm_containment::consent::DebuggerProbe;
    let mut t = Instant::now();
    let before = debugger_alerts().len();
    let mut asked = before;
    for fails in [
        rule_read_times_out as ReadAgain,
        rule_read_does_not_start,
        rule_read_unclaimed,
    ] {
        let root = crate::test_scratch::outlived_dir("aterm-access-");
        let mut app = ready_to_ask(root.path(), owner_says_no);
        let next = kept_on_an_aging_reading(&mut app, &mut t, fails);
        app.look_for_debugger_moment(next, quiet);
        let failed_at = read_again_lands(&mut app);
        app.look_for_debugger_moment(failed_at, quiet);
        let label = fails(app.config.privacy_probe_gate()).label;
        assert_eq!(debugger_alerts().len(), asked, "{label:?}: no alert");
        assert!(
            app.debugger_launch.queued() && app.debugger_launch.offering(),
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
        app.look_for_debugger_moment(next, quiet);
        assert_eq!(
            debugger_alerts().len(),
            asked,
            "{label:?}: no alert before the rule is read"
        );
        let next = read_again_lands(&mut app);
        app.look_for_debugger_moment(next, quiet);
        asked += 1;
        assert_eq!(debugger_alerts().len(), asked, "{label:?}: read off, asked");
        assert!(
            !app.debugger_launch.queued(),
            "{label:?}: asked, over for this process"
        );
    }

    assert_eq!(UNREAD_READS, 3, "the case below fails three reads");
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = ready_to_ask(root.path(), owner_says_no);
    let next = kept_on_an_aging_reading(&mut app, &mut t, rule_read_times_out);
    app.look_for_debugger_moment(next, quiet);
    let first = read_again_lands(&mut app);
    app.look_for_debugger_moment(first, quiet);
    assert!(app.debugger_launch.queued(), "the first failure: kept");
    app.look_for_debugger_moment(first + REFUSED_FOR, quiet);
    let next = read_again_lands(&mut app);
    app.look_for_debugger_moment(next, quiet);
    assert!(
        app.debugger_launch.queued() && app.debugger_launch.offering(),
        "two failures, the second after an absence: kept"
    );
    let next = app.debugger_launch.next_deadline().expect("a later look");
    app.look_for_debugger_moment(next, quiet);
    let next = read_again_lands(&mut app);
    app.look_for_debugger_moment(next, quiet);
    assert_eq!(debugger_alerts().len(), asked, "failures held: no alert");
    assert!(
        !app.debugger_launch.queued(),
        "failures held for the span: over for this process"
    );

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = ready_to_ask(root.path(), owner_says_no);
    let next = kept_on_an_aging_reading(&mut app, &mut t, rule_reads_unrecognized);
    app.look_for_debugger_moment(next, quiet);
    let next = read_again_lands(&mut app);
    app.look_for_debugger_moment(next, quiet);
    assert_eq!(debugger_alerts().len(), asked, "unknown state: no alert");
    assert!(
        !app.debugger_launch.queued(),
        "unknown state: the lane refuses, over for this process"
    );

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = ready_to_ask(root.path(), owner_says_no);
    let next = kept_on_an_aging_reading(&mut app, &mut t, rule_reads_off);
    app.config.privacy = Some(super::app_config::PrivacyConfig {
        check: Some(false),
        ..Default::default()
    });
    app.look_for_debugger_moment(next, quiet);
    assert_eq!(debugger_alerts().len(), asked, "the gate closed: no alert");
    assert!(
        !app.debugger_launch.queued(),
        "the gate closed: over for this process"
    );
}

/// FIRST-LAUNCH SETUP'S HOLD DOES NOT STRAND AN OFFER ADMITTED BEFORE IT ARMED: the
/// hold holds back asks not yet admitted, but this Mac is recorded already, and an alert
/// held behind a setup the owner cancels would never come.
#[test]
fn a_setup_hold_armed_while_the_offer_records_does_not_strand_it() {
    use crate::metrics::DeadlineOwner;
    let fold = |app: &App, now: Instant| {
        let (mut deadline, mut owner) = (None, DeadlineOwner::None);
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
    let mut held = ready_to_ask(root.path(), owner_says_no);
    focus(&mut held);
    held.hold_initial_setup_permissions();
    let before = debugger_alerts().len();
    let at = later(&mut t);
    held.look_for_debugger_moment(at, quiet);
    assert_eq!(debugger_alerts().len(), before, "held: no alert");
    assert!(held.debugger_launch.queued(), "held: still to come");
    let look = at + Duration::from_secs(1);
    held.debugger_launch.look_later(look);
    assert_ne!(
        fold(&held, at),
        Some(look),
        "held, nothing offered: no look folded for the ask"
    );

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = ready_to_ask(root.path(), owner_says_no);
    focus(&mut app);
    let read = app.debugger_access_facts();
    let slot = app.debugger_launch.start_prepare(read).expect("none out");
    let at = later(&mut t);
    app.look_for_debugger_moment(at, quiet);
    app.hold_initial_setup_permissions();
    let backstop = app.debugger_launch.next_deadline().expect("a later look");
    assert_eq!(fold(&app, at), Some(backstop), "held: the look still wakes");
    let _ = slot.set(crate::debugger_access::OfferPrep::Recorded);
    app.look_for_debugger_moment(at, quiet);
    assert_eq!(
        debugger_alerts().len(),
        before + 1,
        "held, but recorded already: asked"
    );
    assert!(
        !app.debugger_launch.queued(),
        "asked: over for this process"
    );

    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut kept = ready_to_ask(root.path(), owner_says_no);
    focus(&mut kept);
    let at = recorded_mid_offer(&mut kept, &mut t);
    kept.owner_quiet.lock().unwrap().note_decline();
    kept.look_for_debugger_moment(at, quiet);
    assert!(
        kept.debugger_launch.offering() && !kept.debugger_launch.preparing(),
        "PRECONDITION: kept, not out"
    );
    kept.hold_initial_setup_permissions();
    assert!(
        kept.debugger_launch.next_deadline().is_some(),
        "a later look"
    );
    let next = at + Duration::from_secs(1);
    kept.debugger_launch.look_later(next);
    assert_eq!(
        fold(&kept, at),
        Some(next),
        "held: the kept look still wakes"
    );
    *kept.owner_quiet.lock().unwrap() = crate::native_settings::OwnerQuiet::default();
    kept.look_for_debugger_moment(next, quiet);
    assert_eq!(
        debugger_alerts().len(),
        before + 2,
        "held, kept, the quiet passed: asked"
    );
}

/// A headless instance never queues the ask, and one queued on it by hand drops at its
/// first look, before any read: no probe dispatched, no record, no alert.
#[test]
fn a_headless_app_never_asks_or_probes() {
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut app = App::headless_for_test();
    assert!(!app.debugger_launch.queued(), "never queued headless");
    app.tick_consent_lanes(Instant::now());
    assert!(debugger_alerts().is_empty());
    assert!(!crate::debugger_access::LaunchAskQueue::new(false).queued());
    app.consent = ConsentState::with_cached_probe_for_test(denied());
    app.consent_card_config = Some(root.path().join("aterm.toml"));
    app.debugger_launch =
        crate::debugger_access::LaunchAskQueue::queued_for_test(|| Some("HEADLESS"));
    app.consent_gestures =
        crate::native_settings::ConsentGestures::for_test_with(owner_says_yes, worker_started);
    app.look_for_debugger_moment(Instant::now(), quiet);
    assert!(!app.debugger_launch.queued(), "dropped");
    assert!(debugger_alerts().is_empty(), "nobody asked");
    assert!(app.consent.debugger_slot_idle_for_test(), "nothing probed");
    assert!(
        !root
            .path()
            .join(crate::debugger_access::ASKED_RECORD)
            .exists(),
        "nothing recorded"
    );
}

/// ONCE PER MAC: a second launch on the same config folder asks nothing and reads no
/// probe for it; another Mac sharing that folder (a synced config) is asked once.
#[test]
fn a_second_launch_on_the_same_mac_asks_nothing() {
    let root = crate::test_scratch::outlived_dir("aterm-access-");
    let mut t = Instant::now();
    let mut first = ready_to_ask(root.path(), owner_says_no);
    first.look_for_debugger_moment(later(&mut t), quiet);
    assert_eq!(debugger_alerts().len(), 1);

    let mut second = new_mac();
    second
        .consent_launch
        .set_fda(crate::consent_launch::FdaLaunch::NotAsked);
    queue_debugger_ask(
        &mut second,
        root.path(),
        || Some("READY-MAC"),
        owner_says_yes,
    );
    second.look_for_debugger_moment(later(&mut t), quiet);
    assert_eq!(debugger_alerts().len(), 1, "asked on this Mac already");
    assert!(!second.debugger_launch.queued());
    assert!(
        second.consent.debugger_slot_idle_for_test(),
        "the record decides before any probe"
    );

    let mut other = ready_to_ask(root.path(), owner_says_no);
    other.debugger_launch =
        crate::debugger_access::LaunchAskQueue::queued_for_test(|| Some("OTHER-MAC"));
    other.look_for_debugger_moment(later(&mut t), quiet);
    assert_eq!(debugger_alerts().len(), 2, "another Mac on a synced config");
}

/// Already on, seen go off (the shared record says it was on, and it reads off
/// now), or answered on the Settings page in this process: the question is answered
/// on this Mac.
#[test]
fn an_answered_question_settles_without_an_alert() {
    use aterm_containment::consent::{
        DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel, SelfHardening,
    };
    let cases: [AppCase; 3] = [
        ("already on", |app| {
            app.consent.publish_debugger_for_test(
                DebuggerProbe {
                    mode: DebuggerMode::Enabled,
                    member: DebuggerMember::Admin,
                    label: DebuggerProbeLabel::StatusOk,
                },
                SelfHardening::Hardened,
            );
        }),
        ("answered on the page", |app| {
            app.owner_quiet.lock().unwrap().note_debugger_answered();
        }),
        ("seen go off", |app| {
            use crate::debugger_access::{DebuggerChangeLane, ObservedMarker, Origin, RecordsRead};
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
        }),
    ];
    for (name, answered) in cases {
        let root = crate::test_scratch::outlived_dir("aterm-access-");
        let mut t = Instant::now();
        let mut app = ready_to_ask(root.path(), owner_says_yes);
        answered(&mut app);
        let before = debugger_alerts().len();
        app.look_for_debugger_moment(later(&mut t), quiet);
        assert_eq!(debugger_alerts().len(), before, "{name}: no alert");
        assert!(!app.debugger_launch.queued(), "{name}: settled");
        assert!(
            crate::debugger_access::read_asked(&root.path().join("aterm.toml"), "READY-MAC"),
            "{name}: recorded"
        );
    }
}
