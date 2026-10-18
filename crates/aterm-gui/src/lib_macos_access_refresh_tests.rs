// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::App;
use super::consent_card::{CardPhase, GRANTED_CAPTION, Marker, Verdict, WATCH_FOR};
use super::control_privacy::ConsentState;
use super::message_reporters::KEY_FILE_ACCESS;
use aterm_containment::consent::{FdaProbe, FdaState, ProbeLabel};
use std::time::{Duration, Instant};

/// Real App/card glue over synthetic published observations; never a live OS probe. The
/// cache's own publication and invalidation are tested in `control_privacy`.
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

/// Whether the question stands: an entry of the digest, or a `privacy.fda`
/// row an older parent carried across an update.
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
    // The question as an older parent left it (a row, restated to the
    // route words of an earlier Open Settings, still on the band), and the
    // entry this build raised beside it.
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

    // The same explicit door still respects the current notice switch.
    app.config.privacy = Some(super::app_config::PrivacyConfig {
        notice: Some(false),
        ..Default::default()
    });
    app.note_macos_access_settings_opened(now);
    assert_eq!(app.consent_card.phase(), CardPhase::Settled);
    assert_eq!(app.next_macos_access_card_deadline(now), None);
}

// ---------------------------------------------------------------------------
// FILE ACCESS ON A NEW MAC (owner ask 2026-10-16; `consent_launch`)
// ---------------------------------------------------------------------------

/// A window standing in for a new Mac: the probe reads `denied`, the hand-over lane
/// opens pages and shows panels through counted stand-ins, and the launch state knows a
/// bundle id. Nothing reaches the OS.
fn new_mac() -> App {
    use crate::consent_launch::{LaunchConsent, LaunchReach};
    use crate::devtools_helper::{Lane, test_arms};
    let mut app = windowed(denied());
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

    // The page's opening retired the cached probe; until a fresh one lands the warm-up
    // waits with no timer.
    app.launch_warmup_look(now, quiet);
    assert_eq!(app.consent_launch.next_deadline(), None);
    assert!(!app.consent_launch.walking());
    // The fresh probe still reads off: not quiet, it looks again in a few seconds;
    // quiet, it walks.
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
/// Full Disk Access need, however many wakes report the pass after it ended. The
/// owner's own pass, refused the same way, still is one.
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

    // The next process, the grant on.
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
    consent.set_responsible_for_test(Ok);
    consent
}

/// `probe`, read by a window another app started in-process: macOS names that app
/// (here launchd's pid) responsible, and the probe reads ITS grant.
fn foreign(probe: FdaProbe) -> ConsentState {
    let mut consent = ConsentState::with_cached_probe_for_test(probe);
    consent.set_responsible_for_test(|_| Ok(1));
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

    // A later launch on this Mac, the owner having switched it off.
    let mut later = new_mac();
    assert!(later.consent_card.begin_deciding(now));
    later.decide_macos_access_card(PendingDecision {
        record,
        why: DecideWhy::Launch,
        verified_here: Some(true),
        ..PendingDecision::launch(None)
    });
    assert_eq!(later.devtools_helper.fda_queued(), None);

    // A grant made while the hand-over waits is recorded at its quiet moment.
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

    // A page aterm opened earlier is on record (`opened`): a foreign reading does not
    // confirm it, and the marker stays for aterm's own next launch.
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

    // Nor does it bring aterm's page up on that terminal's reading.
    let mut denied_there = new_mac();
    denied_there.consent = foreign(denied());
    assert!(denied_there.consent_card.begin_deciding(now));
    denied_there.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
    assert_eq!(denied_there.devtools_helper.fda_queued(), None);
    assert_eq!(
        denied_there.consent_launch.fda(),
        crate::consent_launch::FdaLaunch::NotAsked
    );

    // Nor does it walk the folders: macOS would ask for the other app.
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
