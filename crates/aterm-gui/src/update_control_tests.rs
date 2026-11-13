// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn status() -> aterm_update::UpdateStatus {
    aterm_update::UpdateStatus {
        enabled: true,
        installable: true,
        current_build: 10,
        staged_build: Some(11),
        staged_version: Some("test".into()),
        staged_commit: Some("a".repeat(40)),
        staged_dmg_sha256: Some("b".repeat(64)),
        changelog: None,
        outcome: String::new(),
        updated_at: String::new(),
        failing_checks: 0,
        failing_kind: String::new(),
        failing_applies: 0,
        failing_since: String::new(),
        failing_persistent: false,
        failing_checks_kind: String::new(),
        channel_unreadable: false,
        linux: None,
    }
}

#[test]
fn automatic_check_requires_both_live_and_readable_persisted_policy() {
    let settings = |auto_apply| aterm_update::CheckSettings {
        source: aterm_update::Source {
            owner: "owner".into(),
            repo: "repo".into(),
        },
        auto_apply,
    };
    assert!(admit_check_settings(settings(true), None).is_none());
    for live in [false, true] {
        for disk in [false, true] {
            assert_eq!(
                admit_check_settings(settings(live), Some(settings(disk)))
                    .unwrap()
                    .auto_apply,
                live && disk
            );
        }
    }
    let mut repointed = settings(true);
    repointed.source.repo = "changed".into();
    assert!(admit_check_settings(settings(true), Some(repointed)).is_none());
}

#[test]
fn control_check_announces_every_completion_without_inventing_a_stage() {
    let mut status = status();
    let mut sent = false;
    announce_stage(&status, |wake| {
        assert!(matches!(wake, Wake::UpdateStaged { build: 11, .. }));
        sent = true;
    });
    assert!(sent, "a manual socket check must wake the GUI reducer");
    status.enabled = false;
    announce_stage(&status, |wake| {
        assert!(matches!(wake, Wake::UpdateCheckCompleted))
    });
    status.enabled = true;
    status.staged_build = Some(10);
    announce_stage(&status, |wake| {
        assert!(matches!(wake, Wake::UpdateCheckCompleted))
    });
    status.staged_build = None;
    announce_stage(&status, |wake| {
        assert!(matches!(wake, Wake::UpdateCheckCompleted))
    });
}

#[test]
fn control_posture_requires_the_same_artifact_and_a_real_retry() {
    let mut status = status();
    let mut snapshot = Snapshot {
        enabled: true,
        owner: None,
        repo: None,
        auto_apply: true,
        staged: Some(StagedUpdate {
            build: 11,
            version: "test".into(),
            commit: status.staged_commit.clone(),
            dmg_sha256: status.staged_dmg_sha256.clone().unwrap(),
            changelog: None,
            generation: 1,
        }),
        posture: Some(ApplyPosture::Automatic),
        phase: Some(ApplyPhase::PreferOutputGap),
        restored_hold: false,
        lane_hold: None,
        account_work_hold: false,
        applying: false,
        retry_scheduled: false,
    };
    assert_eq!(snapshot.apply_posture(&status), "automatic-idle");
    assert_eq!(snapshot.apply_phase(&status), Some("prefer-output-gap"));
    snapshot.retry_scheduled = true;
    assert_eq!(snapshot.apply_posture(&status), "automatic");
    assert_eq!(snapshot.apply_phase(&status), Some("prefer-output-gap"));
    status.enabled = false;
    assert_eq!(
        snapshot.apply_posture(&status),
        "automatic",
        "turning off checks does not disable an admitted staged installation"
    );
    assert_eq!(snapshot.apply_phase(&status), Some("prefer-output-gap"));
    snapshot.applying = true;
    assert_eq!(snapshot.apply_posture(&status), "applying");
    snapshot.applying = false;
    status.enabled = true;
    status.staged_commit = status.staged_commit.map(|s| s.to_uppercase());
    status.staged_dmg_sha256 = status.staged_dmg_sha256.map(|s| s.to_uppercase());
    assert_eq!(snapshot.apply_posture(&status), "automatic");
    snapshot.posture = Some(ApplyPosture::AtNextLaunch);
    assert_eq!(snapshot.apply_posture(&status), "next-launch");
    assert_eq!(
        snapshot.apply_phase(&status),
        None,
        "a build that installs at the next launch promises no phase"
    );
    snapshot.posture = Some(ApplyPosture::ManualByConfig);
    assert_eq!(snapshot.apply_posture(&status), "manual-config");
    assert_eq!(
        snapshot.apply_phase(&status),
        None,
        "no phase is promised for a lane that will not apply by itself"
    );
    assert_eq!(
        snapshot.apply_policy_reason(&status),
        Some("update.auto_apply=false")
    );
    status.staged_dmg_sha256 = Some("c".repeat(64));
    assert_eq!(snapshot.apply_posture(&status), "unreconciled");
    assert_eq!(snapshot.apply_policy_reason(&status), None);
    status.staged_dmg_sha256 = None;
    assert_eq!(snapshot.apply_posture(&status), "unreconciled");
    let stage = snapshot.staged.as_mut().unwrap();
    stage.dmg_sha256 = crate::native_updater_service::installed_activation_digest(
        stage.build,
        stage.commit.as_deref().unwrap(),
    );
    assert_eq!(snapshot.apply_posture(&status), "manual-config");
    status.staged_commit = Some("d".repeat(40));
    assert_eq!(snapshot.apply_posture(&status), "unreconciled");
    status.staged_build = None;
    status.enabled = false;
    assert_eq!(snapshot.apply_posture(&status), "disabled");
    status.enabled = true;
    assert_eq!(snapshot.apply_posture(&status), "none");
}

#[test]
fn the_restored_agents_hold_is_named_while_it_holds() {
    let status = status();
    let mut snapshot = Snapshot {
        enabled: true,
        owner: None,
        repo: None,
        auto_apply: true,
        staged: Some(StagedUpdate {
            build: 11,
            version: "test".into(),
            commit: status.staged_commit.clone(),
            dmg_sha256: status.staged_dmg_sha256.clone().unwrap(),
            changelog: None,
            generation: 1,
        }),
        posture: Some(ApplyPosture::Automatic),
        phase: Some(ApplyPhase::Land),
        restored_hold: true,
        lane_hold: None,
        account_work_hold: false,
        applying: false,
        retry_scheduled: true,
    };
    assert_eq!(snapshot.apply_phase(&status), Some("land"));
    assert_eq!(snapshot.apply_held(&status), Some("restored-agents"));
    snapshot.restored_hold = false;
    assert_eq!(snapshot.apply_held(&status), None);
    snapshot.restored_hold = true;
    snapshot.posture = Some(ApplyPosture::ManualByConfig);
    assert_eq!(
        snapshot.apply_held(&status),
        None,
        "a lane that will not apply by itself is held by nothing"
    );

    snapshot.posture = Some(ApplyPosture::Automatic);
    snapshot.lane_hold = Some("setup-draft");
    assert_eq!(
        snapshot.apply_held(&status),
        Some("restored-agents"),
        "the restored agents' hold is said while it holds"
    );
    snapshot.restored_hold = false;
    assert_eq!(
        snapshot.apply_held(&status),
        Some("setup-draft"),
        "the hold the lane named past its ladder is the status's"
    );
    snapshot.posture = Some(ApplyPosture::ManualByConfig);
    assert_eq!(snapshot.apply_held(&status), None);
}

#[test]
fn an_accepted_apply_names_account_work_only_until_its_exact_drain_finishes() {
    use crate::app_account_work::Continuation;
    use crate::native_updater_service::{ApplyAttemptTicket, ApplyMode};

    let mut status = status();
    let mut app = App::headless_for_test();
    let attempt = ApplyAttemptTicket::for_test(
        status.staged_build.unwrap(),
        status.staged_commit.as_deref().unwrap(),
        status.staged_dmg_sha256.as_deref().unwrap(),
    );
    attempt.make_current_apply_for_test(&mut app.native_updater_service);
    assert_eq!(Snapshot::capture(&app).apply_held(&status), None);
    #[cfg(unix)]
    let next_attempt = app.next_update_handoff_id;
    let continuation = || Continuation::Handoff {
        mode: ApplyMode::Immediate,
        attempt: Some(attempt.clone()),
        same_image: None,
        #[cfg(unix)]
        next_attempt,
    };
    let work = app.account_work.enter().unwrap();
    assert!(!app.accounts_drained_before(continuation()));
    let waiting = Snapshot::capture(&app);
    assert_eq!(waiting.apply_posture(&status), "applying");
    assert_eq!(waiting.apply_held(&status), Some("account-work"));
    status.staged_dmg_sha256 = Some("c".repeat(64));
    assert_eq!(
        waiting.apply_held(&status),
        None,
        "another artifact has no matching hold"
    );
    status.staged_dmg_sha256 = Some("b".repeat(64));
    drop(work);
    assert!(app.accounts_drained_before(continuation()));
    let ready = Snapshot::capture(&app);
    assert_eq!(ready.apply_posture(&status), "applying");
    assert_eq!(ready.apply_held(&status), None);
}

fn installed_activation(build: u64) -> StagedUpdate {
    let commit = "a".repeat(40);
    StagedUpdate {
        build,
        version: format!("build {build}"),
        dmg_sha256: crate::native_updater_service::installed_activation_digest(build, &commit),
        commit: Some(commit),
        changelog: None,
        generation: 1,
    }
}

#[test]
fn only_an_activation_is_published() {
    let activation = installed_activation(12);
    assert_eq!(activation_to_publish(Some(&activation)), Some(activation));
    let download = StagedUpdate {
        dmg_sha256: "b".repeat(64),
        ..installed_activation(12)
    };
    assert_eq!(activation_to_publish(Some(&download)), None);
    assert_eq!(activation_to_publish(None), None);
}

#[test]
fn the_live_stage_outranks_the_published_one() {
    let live = |staged| Snapshot {
        enabled: true,
        owner: None,
        repo: None,
        auto_apply: true,
        staged,
        posture: None,
        phase: None,
        restored_hold: false,
        lane_hold: None,
        account_work_hold: false,
        applying: false,
        retry_scheduled: false,
    };
    let published = installed_activation(12);
    assert_eq!(status_activation(Some(&live(None)), Some(&published)), None);
    let current = live(Some(installed_activation(13)));
    assert_eq!(
        status_activation(Some(&current), Some(&published)),
        current.staged.as_ref()
    );
    assert_eq!(status_activation(None, Some(&published)), Some(&published));
}

#[test]
fn control_source_is_the_live_gui_configuration() {
    let mut app = App::headless_for_test();
    let config = app.config.update.get_or_insert_with(Default::default);
    config.owner = Some("first-owner".into());
    config.repo = Some("first-repo".into());
    config.enabled = Some(false);
    let first = Snapshot::capture(&app);
    let config = app.config.update.as_mut().unwrap();
    config.owner = Some("second-owner".into());
    config.repo = Some("second-repo".into());
    config.enabled = Some(true);
    let second = Snapshot::capture(&app);
    assert_eq!(first.owner.as_deref(), Some("first-owner"));
    assert!(!first.enabled);
    assert!(second.enabled);
    assert_eq!(first.repo.as_deref(), Some("first-repo"));
    assert_eq!(second.owner.as_deref(), Some("second-owner"));
    assert_eq!(second.repo.as_deref(), Some("second-repo"));
}

/// Windows: the install row follows what the staged-build watch last said. A build laid under
/// the window takes the row down and is said once; a withdrawn offer takes it down silently;
/// only an offered build puts it up (the control).
#[cfg(windows)]
#[test]
fn the_windows_install_row_follows_the_staged_build_notice() {
    use aterm_update::windows::{Notice, Smoke, StagedBuild};
    let staged = |build| StagedBuild {
        version: "9.9.0".to_string(),
        commit: "0123456789ab".to_string(),
        build_number: build,
        dir: std::path::PathBuf::from("C:/dist/0123456789ab"),
        staged_at: String::new(),
        smoke: Smoke::Pass,
        dirty: false,
    };
    let (build_11, build_12) = (staged(11), staged(12));

    assert_eq!(
        windows_mark_after(None, &Notice::Offered(build_11.clone())),
        (Some((11, "9.9.0".to_string())), None),
        "the control: an offered build shows its row"
    );
    assert_eq!(
        windows_mark_after(None, &Notice::Installed(build_11.clone())),
        (None, Some(&build_11)),
        "laid under the window: no row, and the restart note"
    );
    assert_eq!(
        windows_mark_after(Some(11), &Notice::Installed(build_11.clone())),
        (None, None),
        "the note is said once"
    );
    assert_eq!(
        windows_mark_after(Some(11), &Notice::Nothing),
        (None, None),
        "a withdrawn offer takes the row down"
    );
    assert_eq!(
        windows_mark_after(Some(11), &Notice::Offered(build_11)),
        (None, None),
        "what this window installed is not offered here again"
    );
    assert_eq!(
        windows_mark_after(Some(11), &Notice::Offered(build_12)),
        (Some((12, "9.9.0".to_string())), None),
        "a newer build is"
    );
}
