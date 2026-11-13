// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn linux_state(linux: Option<aterm_update::LinuxUpdateStatus>, failures: u32) -> UpdateState {
    let mut status = staged_status();
    status.linux = linux;
    status.staged_build = None;
    status.staged_version = None;
    status.staged_commit = None;
    status.staged_dmg_sha256 = None;
    status.failing_checks = failures;
    status.outcome = "authenticated Linux status receipt".into();
    let mut state = UpdateState::from_status(828, "0.5.14", Some(&status), false);
    state.linux_host = true;
    state
}

fn linux_facts() -> aterm_update::LinuxUpdateStatus {
    aterm_update::LinuxUpdateStatus {
        installed_build: 828,
        installed_version: String::new(),
        staged_build: Some(830),
        staged_version: Some("0.5.15".into()),
        staged_commit: Some("ab".repeat(20)),
        trial_phase: None,
        trial_starts: 0,
        trial_healthy: false,
    }
}

const LINUX_HELD: &str = "aterm 0.5.15 is downloaded \u{2014} `aterm update apply` installs it \
     (\u{201c}Install updates automatically\u{201d} is off)";
const LINUX_PENDING: &str = "aterm 0.5.15 is installed \u{2014} the next aterm you start \
                             finishes it; existing sessions continue unchanged";

#[test]
fn linux_update_status_download_is_read_only_and_directs_manual_apply() {
    let mut state = linux_state(Some(linux_facts()), 0);
    state.outcome = LINUX_HELD.into();
    let projection = state.projection();
    assert!(projection.headline.contains("downloaded"));
    let detail = projection.detail.as_deref().unwrap();
    assert_eq!(detail, LINUX_HELD);
    assert!(!detail.contains("build 8"), "{detail}");
    assert!(projection.staged.is_none());
    assert_eq!(projection.linux.as_ref().unwrap().staged_build, Some(830));
    assert!(
        crate::native_settings::compact_update_detail_minimum(&projection)
            .contains("aterm update apply")
    );
}

#[test]
fn linux_update_status_pending_trial_never_claims_current() {
    let mut facts = linux_facts();
    facts.staged_build = None;
    facts.staged_version = None;
    facts.staged_commit = None;
    facts.installed_build = 830;
    facts.trial_phase = Some("Installed".into());
    facts.trial_starts = 1;
    let mut state = linux_state(Some(facts), 0);
    state.outcome = LINUX_PENDING.into();
    let projection = state.projection();
    assert_eq!(projection.detail.as_deref(), Some(LINUX_PENDING));
    let failing = linux_state(state.linux.clone(), 1).projection().detail;
    assert!(
        failing
            .as_deref()
            .unwrap_or_default()
            .starts_with("Build 830 waits for a clean launch."),
        "{failing:?}"
    );
    for phase in ["Prepared", "RollbackPrepared"] {
        let mut facts = state.linux.clone().expect("the Linux facts");
        facts.trial_phase = Some(phase.into());
        let detail = linux_state(Some(facts), 1).projection().detail;
        assert!(
            !detail
                .as_deref()
                .unwrap_or_default()
                .contains("clean launch"),
            "{phase}: {detail:?}"
        );
    }
}

#[test]
fn linux_update_status_first_error_remains_visible_with_a_download() {
    let mut state = linux_state(Some(linux_facts()), 1);
    state.outcome = "signature verification refused the new manifest".into();
    let projection = state.projection();
    assert!(projection.headline.contains("attention"));
    let detail = projection.detail.as_deref().unwrap();
    assert!(
        detail.contains("0.5.15")
            && detail.contains("signature verification refused the new manifest")
            && detail.contains("`aterm update apply`"),
        "{detail}"
    );
    state.outcome = "Another aterm is updating this copy right now.".into();
    assert!(
        !state.projection().detail.unwrap().contains(".."),
        "one sentence ends once"
    );
    assert!(projection.linux_attention);
}

#[test]
fn linux_update_status_unenrolled_copy_keeps_exact_reason_and_linux_advice() {
    let mut state = linux_state(None, 0);
    state.enabled = false;
    state.installable = UpdateInstallability::NotInstallable;
    state.outcome = "installed prefix is group writable before a private ancestor".into();
    let state = state.without_inapplicable_stage();
    let projection = state.projection();
    let detail = projection.detail.as_deref().unwrap();
    assert!(detail.contains("group writable"));
    assert!(!detail.contains("aterm update enable"), "{detail}");
    assert!(!detail.contains("Applications"));
    assert!(!detail.contains(".app"));
    assert_eq!(projection.outcome, state.outcome);
}

#[test]
fn linux_automatic_checks_follow_the_running_checker_and_name_the_terminal_lane() {
    let mut idle = linux_facts();
    idle.staged_build = None;
    idle.staged_version = None;
    idle.staged_commit = None;
    let page = |facts: aterm_update::LinuxUpdateStatus, failures, running, saved| {
        linux_state(Some(facts), failures)
            .with_automatic_checks(running, saved)
            .projection()
    };
    let on = page(idle.clone(), 0, true, true);
    assert!(
        !on.detail.as_deref().unwrap().contains("Automatic checks"),
        "{:?}",
        on.detail
    );

    let off = page(idle.clone(), 0, false, false);
    let detail = off.detail.expect("the Linux detail");
    assert!(detail.contains("`aterm update check`"), "{detail}");
    assert!(
        detail.ends_with(on.detail.as_deref().unwrap()),
        "the Linux detail follows whole: {detail}"
    );
    assert!(!detail.contains("Update to Latest Now"), "{detail}");
    assert!(!detail.contains(".."), "one sentence ends once: {detail}");

    let starting = page(idle.clone(), 0, false, true);
    let starting = starting.detail.expect("the Linux detail");
    assert!(starting.contains("`aterm update check`"), "{starting}");
    assert_ne!(starting, detail, "a saved flip is its own detail");

    // A launch read with no answer yet: the checks start in this launch once it answers on,
    // and a saved off still reads as nothing checking by itself.
    let waiting = |saved| {
        linux_state(Some(idle.clone()), 0)
            .with_automatic_checks(false, saved)
            .with_automatic_checks_waiting(true)
            .projection()
            .detail
            .expect("the Linux detail")
    };
    let starting_here = waiting(true);
    assert!(
        starting_here.contains("once aterm can read its settings"),
        "{starting_here}"
    );
    assert!(!starting_here.contains("next launch"), "{starting_here}");
    assert_eq!(waiting(false), detail, "CONTROL: a saved off says the same");

    let stopping = page(idle.clone(), 0, true, false);
    assert!(
        stopping
            .detail
            .as_deref()
            .unwrap()
            .starts_with("Automatic checks stop next launch"),
        "{:?}",
        stopping.detail
    );

    let downloaded = page(linux_facts(), 0, false, false);
    assert!(
        !downloaded
            .detail
            .as_deref()
            .unwrap()
            .contains("Automatic checks"),
        "{:?}",
        downloaded.detail
    );
    let attention = page(idle.clone(), 1, false, false);
    assert!(
        !attention
            .detail
            .as_deref()
            .unwrap()
            .contains("Automatic checks"),
        "{:?}",
        attention.detail
    );
    let mut trial = idle.clone();
    trial.installed_build = 830;
    trial.trial_phase = Some("Installed".into());
    trial.trial_starts = 1;
    let mut installed = idle;
    installed.installed_build = 830;
    for (facts, headline, fact) in [
        (trial, "a pending trial", LINUX_PENDING),
        (installed, "a newer install", "aterm 0.5.15 is installed"),
    ] {
        for (running, saved) in [(false, false), (false, true), (true, false)] {
            let mut state = linux_state(Some(facts.clone()), 0);
            state.outcome = fact.into();
            let projection = state.with_automatic_checks(running, saved).projection();
            let detail = projection.detail.expect("the Linux detail");
            assert!(detail.starts_with(fact), "{headline} {detail}");
            assert!(
                !detail.contains("healthy") && !detail.contains("starts 1"),
                "no raw trial fields: {headline} {detail}"
            );
            assert!(
                !detail.contains("Automatic checks") && !detail.contains("Nothing checks"),
                "{headline} {detail}"
            );
        }
    }
}

fn staged_status() -> aterm_update::UpdateStatus {
    aterm_update::UpdateStatus {
        linux: None,
        enabled: true,
        current_build: 828,
        staged_build: Some(830),
        staged_version: Some("0.5.15".to_string()),
        staged_commit: Some("deadbeefcafe".to_string()),
        staged_dmg_sha256: Some("ab".repeat(32)),
        changelog: Some("### Features\n- **DSU**: hot-swap\n- faster startup".to_string()),
        outcome: "ok".to_string(),
        updated_at: String::new(),
        failing_checks: 0,
        failing_kind: String::new(),
        failing_applies: 0,
        installable: true,
        failing_since: String::new(),
        failing_persistent: false,
        failing_checks_kind: String::new(),
        channel_unreadable: false,
        hold: None,
        last: None,
        offered: None,
        behind: false,
    }
}

#[test]
fn automatic_checks_off_is_named_and_leaves_the_manual_lane_standing() {
    let mut st = staged_status();
    st.staged_build = None;
    st.staged_version = None;
    let off = UpdateState::from_status(828, "0.5.14", Some(&st), false)
        .with_automatic_checks(false, false)
        .projection();
    assert!(
        off.enabled,
        "the updater still runs here: a person's check works"
    );
    assert_eq!(off.detail, None);
    assert!(
        off.automatic_checks_note.is_some(),
        "the page says what is off"
    );
    let on = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
    assert_eq!((on.detail, on.automatic_checks_note), (None, None));
    let staged = UpdateState::from_status(828, "0.5.14", Some(&staged_status()), false)
        .with_automatic_checks(false, false)
        .projection();
    assert!(staged.staged.is_some());
    st.channel_unreadable = true;
    st.outcome = "the channel is unreadable".to_string();
    let stranded = UpdateState::from_status(828, "0.5.14", Some(&st), false)
        .with_automatic_checks(false, false)
        .projection();
    assert_eq!(
        stranded.detail.as_deref(),
        Some("the channel is unreadable")
    );
    assert_eq!(stranded.automatic_checks_note, None);
}

#[test]
fn a_saved_flip_of_automatic_checks_is_named_as_next_launch_in_both_directions() {
    let mut st = staged_status();
    st.staged_build = None;
    st.staged_version = None;
    let page = |running, saved| {
        UpdateState::from_status(828, "0.5.14", Some(&st), false)
            .with_automatic_checks(running, saved)
            .projection()
    };
    let off = page(false, false)
        .automatic_checks_note
        .expect("off is named");
    let stopping = page(true, false)
        .automatic_checks_note
        .expect("the pending stop is named");
    let starting = page(false, true)
        .automatic_checks_note
        .expect("the pending start is named");
    assert!(
        off != stopping && off != starting && stopping != starting,
        "each state is its own sentence: {off:?} / {stopping:?} / {starting:?}"
    );
    assert_eq!(page(true, true).automatic_checks_note, None);
}

#[test]
fn a_stage_this_copy_cannot_apply_is_not_shown_as_ready() {
    let mut st = staged_status();
    st.installable = false;
    let p = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
    assert!(
        p.staged.is_none(),
        "a stage it cannot apply is not this copy's"
    );
    assert!(
        p.changelog.is_empty(),
        "…nor are that build's release notes"
    );
    assert!(p.outcome.is_empty(), "…nor the check verdict it never ran");

    st.installable = true;
    let ok = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
    assert_eq!(ok.staged, Some((830, "0.5.15".to_string())));
}

#[test]
fn the_projection_carries_whether_this_copy_can_be_replaced_at_all() {
    let mut st = staged_status();
    st.staged_build = None;
    st.staged_version = None;
    st.staged_dmg_sha256 = None;
    st.changelog = None;
    st.installable = false;
    let projected = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
    assert!(
        projected.installable.cannot_install(),
        "a copy with no replaceable bundle must project that fact, not just headline it"
    );
    st.installable = true;
    assert!(
        UpdateState::from_status(828, "0.5.14", Some(&st), false)
            .projection()
            .installable
            .is_installable(),
        "an ordinary install must stay installable"
    );
}

#[test]
fn a_copy_that_cannot_replace_itself_says_so_instead_of_up_to_date() {
    let mut st = staged_status();
    st.staged_build = None;
    st.staged_version = None;
    st.staged_dmg_sha256 = None;
    st.changelog = None;
    st.installable = false;
    use aterm_update::which_copy::InstallPosture;
    let page = |posture: Option<InstallPosture>| {
        UpdateState::from_status(828, "0.5.14", Some(&st), false)
            .with_install_posture(posture)
            .projection()
    };
    let p = page(Some(InstallPosture::MountedImage));
    assert_eq!(p.detail.as_deref(), InstallPosture::MountedImage.remedy());
    assert_eq!(
        page(Some(InstallPosture::Translocated)).detail.as_deref(),
        InstallPosture::Translocated.remedy()
    );
    let binary = page(Some(InstallPosture::NotABundle)).detail;
    assert_eq!(binary.as_deref(), Some(super::NOT_A_BUNDLE_DETAIL));
    assert_eq!(page(None).detail, None);
}

#[test]
fn a_dev_build_is_told_where_it_stands_not_to_move() {
    use aterm_update::dev_channel::DevLag;
    let mut st = staged_status();
    st.staged_build = None;
    st.staged_version = None;
    st.staged_dmg_sha256 = None;
    st.changelog = None;
    st.installable = false;
    let page = |standing: Option<DevLag>| {
        UpdateState::from_status(828, "0.91.0", Some(&st), false)
            .with_dev_build(Some(DevBuildPage { standing }))
            .projection()
    };
    let unread = page(None);
    let detail = unread.detail.expect("detail");
    assert!(
        detail.starts_with("The updater leaves a dev build alone"),
        "{detail}"
    );
    let behind = page(Some(DevLag::Behind {
        latest: "v0.93.0".into(),
        releases: Some(2),
    }));
    assert!(
        behind
            .detail
            .as_deref()
            .is_some_and(|d| d.starts_with("It is 2 releases behind aterm v0.93.0. ")),
        "{:?}",
        behind.detail
    );
    assert_eq!(
        page(Some(DevLag::Behind {
            latest: "v0.93.0".into(),
            releases: Some(1),
        }))
        .headline,
        "This dev build is 1 release behind."
    );
    assert_eq!(
        page(Some(DevLag::Behind {
            latest: "v1.0.0".into(),
            releases: None,
        }))
        .headline,
        "This dev build is older than aterm v1.0.0."
    );
    let current = page(Some(DevLag::Current {
        latest: "v0.91.0".into(),
    }));
    assert!(
        current
            .detail
            .as_deref()
            .is_some_and(|d| d.starts_with("It is at aterm v0.91.0, the newest release. ")),
        "{:?}",
        current.detail
    );
    assert!(behind.dev_build);
    let plain = UpdateState::from_status(828, "0.91.0", Some(&st), false)
        .with_install_posture(Some(aterm_update::which_copy::InstallPosture::MountedImage))
        .projection();
    assert!(!plain.dev_build);
    assert!(
        plain
            .detail
            .is_some_and(|d| d.contains("Drag aterm to Applications"))
    );
}

#[test]
fn a_staged_build_that_keeps_failing_to_apply_says_so() {
    let mut st = staged_status();
    st.failing_applies = 3;
    st.failing_kind = "apply".to_string();
    st.failing_persistent = true;
    st.outcome = "staged 0.5.15 (build 830) — verified and ready to apply".to_string();
    let s = UpdateState::from_status(828, "0.5.14", Some(&st), false).with_apply_lane(
        "overlap handoff failed safely: handoff proof ended ChildDied",
        ApplyRetry::ManualOnly,
    );
    let p = s.projection();
    assert!(
        p.failing_persistent,
        "the projection no longer masks a failure while staged"
    );
    let detail = p.detail.expect("detail");
    assert!(
        detail.contains("0.5.15") && detail.contains("3 times"),
        "the escalated detail names the version and how many attempts died: {detail}"
    );
    assert!(
        !detail.contains("ready to apply"),
        "not the ledger's healthy sentence: {detail}"
    );
    assert!(
        detail.contains("Settings \u{25b8} Messages"),
        "the escalated case keeps its pointer at the full history: {detail}"
    );
    assert!(
        !detail.contains("build 830"),
        "build numbers belong in About: {detail}"
    );

    let mut acquiring = staged_status();
    acquiring.failing_checks = 3;
    acquiring.failing_kind = "manifest".to_string();
    acquiring.failing_persistent = true;
    let s = UpdateState::from_status(828, "0.5.14", Some(&acquiring), false);
    let p = s.projection();
    assert!(p.failing_persistent && !p.apply_is_failing);
    let detail = p.detail.expect("detail");
    assert!(
        detail.contains("0.5.15") && detail.contains("Messages"),
        "the stage in hand is still ready; the log has the rest: {detail}"
    );
}

#[test]
fn a_clean_staged_update_does_not_read_as_a_failure() {
    let st = staged_status();
    assert_eq!(st.failing_applies, 0, "the fixture is a clean stage");
    let p = UpdateState::from_status(828, "0.5.14", Some(&st), false)
        .with_apply_lane(
            "overlap handoff failed safely: handoff proof ended ChildDied",
            ApplyRetry::Scheduled,
        )
        .projection();

    assert!(
        p.apply_trouble.is_none(),
        "a reason with no attempts behind it is not an attempt"
    );
    let detail = p.detail.expect("detail");
    assert!(detail.contains("0.5.15"), "{detail}");
    for alarming in ["tried", "failed", "did not", "retry"] {
        assert!(
            !detail.contains(alarming),
            "a stage that has simply not been attempted must not read as a \
             failure ({alarming:?} in {detail:?})"
        );
    }
}

#[test]
fn an_installed_activation_is_not_called_downloaded() {
    use crate::native_updater_service::{
        NativeUpdaterService, StagedUpdate, installed_activation_digest,
    };
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let detail_for = |dmg_sha256: String| {
        let mut snapshot = NativeUpdaterService::new(828, "0.5.14", true)
            .snapshot()
            .clone();
        snapshot.installable = UpdateInstallability::Installable;
        snapshot.staged = Some(StagedUpdate {
            build: 830,
            version: "0.5.15".to_string(),
            commit: Some(commit.to_string()),
            dmg_sha256,
            changelog: None,
            generation: 1,
        });
        let p = UpdateState::from_service(&snapshot, false, ApplyRetry::Scheduled).projection();
        p.detail
    };

    let activated = detail_for(installed_activation_digest(830, commit)).expect("detail");
    assert!(!activated.contains("downloaded"), "{activated}");
    let downloaded = detail_for("ab".repeat(32)).expect("detail");
    assert!(downloaded.contains("downloaded"), "{downloaded}");
}

#[test]
fn the_surface_says_whether_the_update_will_retry_without_the_user() {
    let mut st = staged_status();
    st.failing_applies = 2;
    let detail_for = |retry| {
        UpdateState::from_status(828, "0.5.14", Some(&st), false)
            .with_apply_lane("handoff proof ended ChildDied", retry)
            .projection()
            .detail
            .expect("detail")
    };
    let scheduled = detail_for(ApplyRetry::Scheduled);
    let manual = detail_for(ApplyRetry::ManualOnly);
    assert_ne!(scheduled, manual);
    assert!(
        scheduled.contains("try again by itself"),
        "a scheduled retry tells the reader to do nothing: {scheduled}"
    );
    assert!(
        manual.contains("not try again until you ask"),
        "a latched lane tells the reader it is now up to them: {manual}"
    );
}

#[test]
fn observability_audit_the_page_tells_the_owner_to_reinstall_over_an_unsigned_install() {
    let mut st = staged_status();
    st.failing_applies = 6;
    st.failing_kind = "apply".to_string();
    st.failing_persistent = true;
    let p = UpdateState::from_status(828, "0.5.14", Some(&st), false)
        .with_apply_lane(
            "installed bundle failed pre-park verification: the installed bundle at \
             /Applications/aterm.app cannot be the rollback source the swap installs: \
             bundle policy: codesign --verify (team-pinned requirement) failed; the \
             terminal was left untouched",
            ApplyRetry::NeedsPerson(
                crate::update_apply_trouble::PersonRemedy::InstallSignedRelease,
            ),
        )
        .projection();
    let detail = p.detail.expect("detail");
    assert!(
        detail.contains("the installed copy could not be verified for replacement"),
        "the page must name the refused installed copy: {detail}"
    );
    assert!(
        detail.contains("Install the signed release from the release DMG, then retry."),
        "a current installed-source block must carry its remedy: {detail}"
    );
    assert!(
        !detail.contains("try again by itself"),
        "a current installed-source block cannot promise an automatic retry: {detail}"
    );
    assert!(
        !detail.contains("could not be set up"),
        "not the generic preparation clause — the installed copy is the cause: {detail}"
    );
}

#[test]
fn a_healthy_page_says_when_it_last_checked() {
    let mut st = staged_status();
    st.staged_build = None;
    st.staged_version = None;
    st.outcome = "up to date (latest release build 828) \u{b7} checks every 30 min".to_string();
    st.updated_at = "2026-09-23T12:00:00Z".to_string();
    let p = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
    assert_eq!(p.detail, None, "nothing to add beyond when it last checked");
    let checked = aterm_update_core::pkg_check::rfc3339_to_unix(&st.updated_at).unwrap();
    assert_eq!(p.checked_at, Some(checked));
    assert!(
        p.checked_line(checked + 12 * 60 + 5)
            .is_some_and(|line| line.contains(&ago_words(checked, checked + 12 * 60 + 5)))
    );
    st.updated_at = String::new();
    let p = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
    assert_eq!(p.checked_line(checked), None);
}

#[test]
fn a_persistent_check_failure_is_plain_and_points_at_the_log() {
    let mut st = staged_status();
    st.staged_build = None;
    st.staged_version = None;
    st.failing_checks = 3;
    st.failing_kind = "network".to_string();
    st.failing_persistent = true;
    st.outcome = "FAILING (3 consecutive checks since 2026-09-23T10:00:00Z): network: \
                  curl exit 6"
        .to_string();
    let p = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
    let detail = p.detail.expect("detail");
    assert!(
        detail.contains("Messages") && !detail.contains("FAILING") && !detail.contains("curl"),
        "words, and where the rest is: {detail}"
    );
    st.failing_kind = "manifest".to_string();
    st.outcome = "FAILING (3 consecutive checks since 2026-09-23T10:00:00Z): this build's \
                  trust anchor cannot verify the channel's releases — reinstall aterm from \
                  the current release"
        .to_string();
    let p = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
    let detail = p.detail.expect("detail");
    assert!(
        detail.starts_with("This copy of aterm is too old"),
        "{detail}"
    );
    assert!(detail.contains("Reinstall aterm"), "{detail}");
    st.failing_kind = "apply".to_string();
    st.failing_applies = aterm_update::PERSISTENT_AFTER;
    st.outcome = "staged build did not apply: this install cannot update itself: \
                  reinstall the signed release"
        .to_string();
    let p = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
    let detail = p.detail.expect("detail");
    assert!(
        !detail.starts_with("This copy of aterm is too old") && detail.contains("Messages"),
        "{detail}"
    );
}

#[test]
fn snapshot_renders_staged_update_notes() {
    let st = staged_status();
    let s = UpdateState::from_status(828, "0.5.14", Some(&st), false);
    assert!(s.staged.is_some());
    assert!(s.changelog.iter().any(|l| l.contains("DSU: hot-swap")));
    assert!(
        !s.changelog
            .iter()
            .any(|l| l.contains('#') || l.contains("**"))
    );
    assert!(s.detail().unwrap().contains("0.5.15"));
}

fn held_view(build: u64, live: bool) -> aterm_update::HoldView {
    aterm_update::HoldView {
        build,
        version: "0.5.15".to_string(),
        cause: aterm_update_core::update_record::Cause::CrashLoop,
        witness: aterm_update_core::update_record::Witness::Headless,
        why: "the listener never answered".to_string(),
        tries: 2,
        until: 1_789_003_600,
        live,
        peer_lifted: false,
    }
}

/// A HELD BUILD IS NEVER "READY" OR "UP TO DATE": the update record's live hold on a newer build
/// is the headline, and the detail says why and when aterm tries it again by itself, on a staged
/// build and on one this page never staged alike, on macOS and on Linux. NEGATIVE CONTROLS: a
/// lapsed hold, and a hold on the build that runs, leave the page as it was.
#[test]
fn a_live_hold_is_the_headline_and_a_lapsed_one_leaves_the_page_alone() {
    let mut st = staged_status();
    st.hold = Some(held_view(830, true));
    let staged = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
    assert_eq!(staged.headline, "Update held");
    let held = staged.held.clone().expect("the record's hold");
    assert_eq!(
        (held.name.as_str(), held.why, held.tries),
        ("0.5.15", "it did not start cleanly", 2)
    );
    let detail = staged.detail.expect("a detail");
    assert!(
        detail.starts_with(
            "Version 0.5.15 is held: it did not start cleanly. aterm tries it again by itself "
        ) && detail.ends_with("or Try Now tries it now."),
        "{detail}"
    );

    let mut idle = st.clone();
    idle.staged_build = None;
    idle.staged_version = None;
    idle.outcome = "held: …".to_string();
    let unstaged = UpdateState::from_status(828, "0.5.14", Some(&idle), false).projection();
    assert_eq!(
        unstaged.headline, "Update held",
        "a hold on a build never staged here"
    );

    let mut linux = st.clone();
    linux.linux = Some(linux_facts());
    let mut on_linux = UpdateState::from_status(828, "0.5.14", Some(&linux), false);
    on_linux.linux_host = true;
    let on_linux = on_linux.projection();
    assert_eq!(on_linux.headline, "Update held.");
    assert!(
        on_linux
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("`aterm update retry` tries it now")),
        "{:?}",
        on_linux.detail
    );

    st.hold = Some(held_view(830, false));
    let lapsed = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
    assert_eq!(
        (lapsed.headline.as_str(), lapsed.held),
        ("Update ready", None),
        "NEGATIVE CONTROL: a lapsed hold is tried again, so the page is as it was"
    );
    st.hold = Some(held_view(828, true));
    let running = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
    assert_eq!(
        running.held, None,
        "NEGATIVE CONTROL: a hold on the build that runs holds no update"
    );
}

/// NEVER "UP TO DATE" WHILE BEHIND: with nothing staged and nothing held now, a lapsed hold
/// waiting for its next check, and a newer offer with no hold at all, each give a headline and
/// compact texts that say a newer version is out. NEGATIVE CONTROL: a copy that is not behind
/// still reads "You're up to date."
#[test]
fn a_lapsed_hold_or_a_newer_offer_with_nothing_staged_never_reads_up_to_date() {
    let mut st = staged_status();
    st.staged_build = None;
    st.staged_version = None;
    st.outcome = "behind: …".to_string();
    st.installable = true;
    st.hold = Some(held_view(830, false));
    st.behind = true;
    let says_current = |p: &crate::update_screen::UpdateProjection| {
        p.headline.contains("up to date")
            || crate::native_settings::compact_update_headline(p) == "Current"
            || crate::native_settings::compact_update_detail_minimum(p) == "Up to date."
    };
    let lapsed = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
    assert_eq!(lapsed.headline, "A newer version is out.");
    assert!(lapsed.behind && !says_current(&lapsed), "{lapsed:?}");
    assert!(
        lapsed
            .detail
            .as_deref()
            .is_some_and(|d| d.starts_with("Version 0.5.15 was held: it did not start cleanly.")),
        "{:?}",
        lapsed.detail
    );

    st.hold = None;
    let behind = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
    assert_eq!(behind.headline, "A newer version is out.");
    assert!(!says_current(&behind), "{behind:?}");

    st.behind = false;
    st.outcome = "up to date (latest release build 828)".to_string();
    let current = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
    assert_eq!(
        current.headline, "You\u{2019}re up to date.",
        "NEGATIVE CONTROL: a copy that is not behind"
    );
}

/// Linux lets only "Up to date" speak for the channel, by the predicate status uses to keep it
/// from standing while a build is held ([`aterm_update::says_up_to_date`]), case and all.
#[test]
fn only_up_to_date_words_are_silenced_while_a_linux_check_does_not_answer() {
    let mut state = linux_state(Some(linux_facts()), 0);
    state.check_unanswered = true;
    for (outcome, silenced) in [
        ("Up to date \u{2014} build 828", true),
        ("up to date", true),
        ("held: aterm 0.5.15 (build 830) is held until …", false),
        ("rolled back build 830", false),
    ] {
        state.outcome = outcome.to_string();
        assert_eq!(
            state.linux_outcome_predates_silence(),
            silenced,
            "{outcome}"
        );
    }
}
