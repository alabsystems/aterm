// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[test]
fn the_notice_body_hands_back_the_date_it_was_written_with() {
    for (class, count) in [
        ("apply", 3),
        ("pipeline", 20),
        ("stage", 4),
        ("manifest", 5),
    ] {
        let ledger = super::health::Health {
            apply_since: "2026-09-14T05:42:50Z".to_string(),
            pipeline_since: "2026-09-14T05:42:50Z".to_string(),
            stage_since: "2026-09-14T05:42:50Z".to_string(),
            manifest_since: "2026-09-14T05:42:50Z".to_string(),
            last_error: "signature did not verify".to_string(),
            last_apply_error: "the successor died".to_string(),
            ..super::health::Health::default()
        };
        let (title, body) = super::persistent_failure_notice(class, count, &ledger, 7);
        assert_eq!(title, super::health_failing_title(class));
        assert_eq!(
            super::health_notice_since(&body),
            Some("2026-09-14T05:42:50Z"),
            "{class}: {body}"
        );
    }
    assert_eq!(super::health_notice_since("no date here"), None);
    assert_eq!(
        super::health_notice_since("broken since yesterday: it failed"),
        None
    );
}

#[test]
fn a_failing_title_reads_back_to_its_class_old_words_and_new() {
    for class in ["apply", "pipeline", "stage", "manifest", "future"] {
        let title = super::health_failing_title(class);
        assert!(title.starts_with("Couldn't "), "{title}");
        let read = super::health_failing_class(title).expect("today's title");
        assert_eq!(super::health_failing_title(read), title, "{class}");
    }
    for (old, class) in [
        ("aterm can't install updates", "apply"),
        ("aterm can't download updates", "pipeline"),
        ("aterm can't check for updates", "manifest"),
    ] {
        assert_eq!(super::health_failing_class(old), Some(class), "{old}");
    }
    for other in [
        super::HEALTH_RESTATED_TITLE,
        super::HEALTH_RECOVERED_TITLE,
        super::STRANDED_TITLE,
        "aterm's update check stopped",
        "Couldn't install update",
        "",
    ] {
        assert_eq!(super::health_failing_class(other), None, "{other:?}");
    }
}

#[test]
fn an_overdue_pending_build_is_announced_once() {
    use super::health::Health;
    let mut announcer = super::HealthAnnouncer::new("2026-09-22T00:00:00Z".to_string());
    let stranded = Health {
        apply_failures: 2,
        apply_since: "2026-09-22T00:11:30Z".to_string(),
        last_failure_at: "2026-09-22T00:11:31Z".to_string(),
        kind: "apply".to_string(),
        last_apply_error: "overlap handoff failed safely: handoff proof ended \
                           AdoptionMismatch"
            .to_string(),
        last_apply_failure_build: 1790019739,
        last_apply_failure_target_build: 1790120495,
        apply_failures_for_target: 2,
        last_apply_refusal: "automatic apply of build 1790120495 is out of retries".to_string(),
        last_apply_refusal_at: "2026-09-22T00:11:32Z".to_string(),
        last_apply_refusal_build: 1790019739,
        pending_build: 1790120495,
        pending_since: "2026-09-22T00:10:45Z".to_string(),
        ..Health::default()
    };
    assert!(stranded.persistent_class().is_none());
    assert!(
        announcer
            .tick_overdue(&stranded, "2026-09-25T00:00:00Z", 1790019739, false)
            .is_none(),
        "auto_apply = false is not a failure"
    );
    assert!(
        announcer
            .tick_overdue(&stranded, "2026-09-22T01:10:00Z", 1790019739, true)
            .is_none(),
        "fifty-nine minutes is not overdue"
    );
    let (title, _) = announcer
        .tick_overdue(&stranded, "2026-09-22T01:11:00Z", 1790019739, true)
        .expect("past the hour the stranded machine is announced");
    assert_eq!(
        title,
        super::health_failing_title("apply"),
        "an update that will not land is the install half failing"
    );
    assert!(
        announcer
            .tick_overdue(&stranded, "2026-09-22T09:00:00Z", 1790019739, true)
            .is_none(),
        "once per build: the row stands, the notice does not repeat"
    );
    assert!(
        announcer
            .tick_overdue(&stranded, "2026-09-23T00:00:00Z", 1790120495, true)
            .is_none()
    );
    let unattempted = Health {
        pending_build: 1790300000,
        pending_since: "2026-09-23T00:00:00Z".to_string(),
        ..Health::default()
    };
    assert!(
        announcer
            .tick_overdue(&unattempted, "2026-09-23T02:00:00Z", 1790120495, true)
            .is_some()
    );
}

#[test]
fn a_withdrawn_build_is_never_announced_as_waiting() {
    use super::health::Health;
    let started = "2026-09-22T00:00:00Z".to_string();
    let later = "2026-09-22T05:00:00Z";
    let backdate = |s: &super::paths::Staging| {
        let mut h = Health::read(&s.health());
        h.pending_since = "2026-09-22T01:00:00Z".to_string();
        std::fs::write(s.health(), aterm_toml::to_string(&h).unwrap()).unwrap();
    };
    for withdrawal in ["none", "quarantine", "yank", "revocation"] {
        let s = super::paths::Staging::scratch("pending-withdrawn");
        super::note_pending_update(&s, 10, Some(11), None);
        backdate(&s);
        match withdrawal {
            "quarantine" => super::manifest::quarantine_artifact(
                &s,
                11,
                &"ab".repeat(32),
                aterm_durable::Flush::Durable,
            ),
            "yank" => super::manifest::Floor::bump_and_write(&s.floor(), 12, 0, 0),
            "revocation" => super::github::retire_revoked_stage(&s, 11),
            _ => {}
        }
        let h = super::note_pending_update(&s, 10, None, None);
        let said = super::HealthAnnouncer::new(started.clone()).tick_overdue(&h, later, 10, true);
        if withdrawal == "none" {
            assert!(said.is_some(), "a stranded machine must still be told");
        } else {
            assert!(
                said.is_none(),
                "{withdrawal}: build 11 is withdrawn, not waiting: {said:?}"
            );
            assert_eq!(h.pending_build, 0, "{withdrawal}");
        }
        let _ = std::fs::remove_dir_all(&s.root);
    }
}

#[test]
fn a_yanked_installed_bundle_is_never_announced_as_waiting() {
    use super::health::Health;
    let started = "2026-09-22T00:00:00Z".to_string();
    let later = "2026-09-22T05:00:00Z";
    let backdate = |s: &super::paths::Staging| {
        let mut h = Health::read(&s.health());
        h.pending_since = "2026-09-22T01:00:00Z".to_string();
        std::fs::write(s.health(), aterm_toml::to_string(&h).unwrap()).unwrap();
    };
    for yanked in [false, true] {
        let s = super::paths::Staging::scratch("pending-installed-yanked");
        super::note_pending_update(&s, 10, None, Some(11));
        backdate(&s);
        if yanked {
            super::manifest::Floor::bump_and_write(&s.floor(), 12, 0, 0);
        }
        let h = super::note_pending_update(&s, 10, None, Some(11));
        let said = super::HealthAnnouncer::new(started.clone()).tick_overdue(&h, later, 10, true);
        if yanked {
            assert_eq!(h.pending_build, 0, "a yanked build is never pending");
            assert!(said.is_none(), "build 11 is yanked, not waiting: {said:?}");
        } else {
            assert_eq!(h.pending_build, 11);
            assert!(said.is_some(), "a stranded machine must still be told");
        }
        let h = super::note_pending_update(&s, 10, Some(12), Some(11));
        assert_eq!(h.pending_build, 12, "yanked: {yanked}");
        assert!(
            super::HealthAnnouncer::new(started.clone())
                .tick_overdue(&h, &super::install::now_rfc3339(), 10, true)
                .is_none(),
            "the successor's clock starts now"
        );
    }
}

#[test]
fn the_announcer_restates_a_moved_count_quietly_and_re_announces_only_a_new_class() {
    use super::health::Health;
    let started = "2026-09-13T22:00:00Z".to_string();
    let mut announcer = super::HealthAnnouncer::new(started);
    let ledger = |apply_failures: u32, last: &str| Health {
        apply_failures,
        apply_since: "2026-09-13T22:40:00Z".to_string(),
        last_failure_at: last.to_string(),
        kind: "apply".to_string(),
        last_apply_error: "the successor died".to_string(),
        ..Health::default()
    };
    assert!(
        announcer
            .tick(
                &ledger(3, "2026-09-13T22:50:00Z"),
                "2026-09-13T23:00:00Z",
                7
            )
            .is_none()
    );
    let (title, _) = announcer
        .tick(
            &ledger(3, "2026-09-13T23:12:50Z"),
            "2026-09-13T23:13:44Z",
            7,
        )
        .expect("the first escalation is announced");
    assert_eq!(title, super::health_failing_title("apply"));
    assert!(
        announcer
            .tick(
                &ledger(3, "2026-09-13T23:12:50Z"),
                "2026-09-13T23:37:45Z",
                7
            )
            .is_none()
    );
    let (title, body) = announcer
        .tick(
            &ledger(6, "2026-09-14T00:10:00Z"),
            "2026-09-14T00:12:34Z",
            7,
        )
        .expect("a moved count is restated");
    assert_eq!(title, super::HEALTH_RESTATED_TITLE);
    assert!(body.starts_with('6'), "the new number: {body}");
    let mut fresh = super::HealthAnnouncer::new("2026-09-14T01:00:00Z".to_string());
    assert!(
        fresh
            .tick(
                &ledger(6, "2026-09-14T00:10:00Z"),
                "2026-09-14T01:05:00Z",
                7
            )
            .is_none()
    );
    let healed = Health::default();
    let (title, body) = announcer
        .tick(&healed, "2026-09-14T02:00:00Z", 7)
        .expect("a failure this process reported gets its recovery reported");
    assert_eq!(title, super::HEALTH_RECOVERED_TITLE);
    assert_eq!(body, "apply", "the heal names the class it proves");
    assert!(announcer.tick(&healed, "2026-09-14T02:01:00Z", 7).is_none());
    let (title, _) = announcer
        .tick(
            &ledger(3, "2026-09-14T03:30:00Z"),
            "2026-09-14T03:31:00Z",
            7,
        )
        .expect("a new episode is news");
    assert_eq!(title, super::health_failing_title("apply"));
}

#[test]
fn a_quick_streak_raises_no_notice_and_a_spanning_one_does() {
    let started = "2026-10-15T09:00:00Z".to_string();
    let streak = |newest: &str| super::health::Health {
        pipeline_failures: 3,
        pipeline_since: "2026-10-15T10:00:00Z".to_string(),
        last_diagnosis_at: newest.to_string(),
        last_diagnosis_error: "release manifests exist but could not be fetched".to_string(),
        network_failures: 1,
        network_since: "2026-10-15T18:00:00Z".to_string(),
        last_failure_at: "2026-10-15T18:00:00Z".to_string(),
        kind: "network".to_string(),
        last_error: "curl: (6) Could not resolve host: github.com".to_string(),
        ..super::health::Health::default()
    };
    let woke = "2026-10-15T18:00:01Z";
    let mut announcer = super::HealthAnnouncer::new(started.clone());
    assert_eq!(
        announcer.tick(&streak("2026-10-15T10:06:00Z"), woke, 7),
        None,
        "three failures inside six minutes"
    );
    let mut announcer = super::HealthAnnouncer::new(started);
    let (title, _) = announcer
        .tick(&streak("2026-10-15T10:30:00Z"), woke, 7)
        .expect("three failures spanning half an hour are news");
    assert_eq!(title, super::health_failing_title("pipeline"));
}

fn failing_checks(last: &str) -> super::health::Health {
    super::health::Health {
        manifest_failures: 20,
        manifest_since: "2026-09-14T22:04:36Z".to_string(),
        last_failure_at: last.to_string(),
        last_diagnosis_at: last.to_string(),
        kind: "manifest".to_string(),
        last_error: "the channel's manifest would not verify".to_string(),
        ..super::health::Health::default()
    }
}

#[test]
fn a_successor_sends_the_recovery_of_the_class_its_parent_announced() {
    let started = "2026-09-29T10:00:00Z".to_string();
    let process = super::AnnouncedHealth::new();
    process.adopt("manifest");
    let mut announcer = super::HealthAnnouncer::new(started.clone());
    let (title, _) = announcer
        .tick_for_process(
            &process,
            &failing_checks("2026-09-29T10:02:00Z"),
            "2026-09-29T10:02:01Z",
            7,
        )
        .expect("the adopted streak's count is restated");
    assert_eq!(
        title,
        super::HEALTH_RESTATED_TITLE,
        "NEGATIVE: the parent's class is not announced a second time"
    );
    assert_eq!(process.class(), Some("manifest"));

    let process = super::AnnouncedHealth::new();
    let mut announcer = super::HealthAnnouncer::new(started.clone());
    assert!(
        announcer
            .tick_for_process(
                &process,
                &super::health::Health::default(),
                "2026-09-29T10:02:01Z",
                7
            )
            .is_none()
    );

    let process = super::AnnouncedHealth::new();
    process.adopt("manifest");
    let mut announcer = super::HealthAnnouncer::new(started.clone());
    let healed = super::health::Health::default();
    let (title, body) = announcer
        .tick_for_process(&process, &healed, "2026-09-29T10:02:01Z", 7)
        .expect("the adopted class's healing is sent");
    assert_eq!(title, super::HEALTH_RECOVERED_TITLE);
    assert_eq!(body, "manifest", "naming the class it proves");
    assert_eq!(process.class(), None, "said once");
    assert!(
        announcer
            .tick_for_process(&process, &healed, "2026-09-29T10:03:00Z", 7)
            .is_none()
    );
    let mut later = failing_checks("2026-09-29T15:40:00Z");
    later.manifest_since = "2026-09-29T15:00:00Z".to_string();
    let (title, _) = announcer
        .tick_for_process(&process, &later, "2026-09-29T15:40:01Z", 7)
        .expect("a new episode is announced");
    assert_eq!(title, super::health_failing_title("manifest"));
}

#[test]
fn a_replacement_generation_sends_the_recovery_its_predecessor_owed() {
    let started = "2026-09-29T10:00:00Z".to_string();
    let process = super::AnnouncedHealth::new();
    let mut stalled = super::HealthAnnouncer::new(started.clone());
    let (title, _) = stalled
        .tick_for_process(
            &process,
            &failing_checks("2026-09-29T10:40:00Z"),
            "2026-09-29T10:40:01Z",
            7,
        )
        .expect("announced");
    assert_eq!(title, super::health_failing_title("manifest"));
    let healed = super::health::Health::default();
    let mut replacement = super::HealthAnnouncer::new("2026-09-29T11:00:00Z".to_string());
    assert!(
        replacement
            .tick(&healed, "2026-09-29T11:05:00Z", 7)
            .is_none(),
        "NEGATIVE: a fresh announcer alone owes nothing"
    );
    let mut replacement = super::HealthAnnouncer::new("2026-09-29T11:00:00Z".to_string());
    let (title, body) = replacement
        .tick_for_process(&process, &healed, "2026-09-29T11:05:00Z", 7)
        .expect("the replacement heals what the process announced");
    assert_eq!(title, super::HEALTH_RECOVERED_TITLE);
    assert_eq!(body, "manifest");
}

#[test]
fn an_adopted_class_keeps_the_one_whose_recovery_answers_most() {
    let process = super::AnnouncedHealth::new();
    process.adopt("manifest");
    process.adopt("pipeline");
    assert_eq!(process.class(), Some("pipeline"));
    process.adopt("manifest");
    assert_eq!(process.class(), Some("pipeline"));
    let process = super::AnnouncedHealth::new();
    process.adopt("stage");
    assert_eq!(process.class(), Some("pipeline"), "one title, one class");
    process.adopt("future");
    assert_eq!(process.class(), Some("pipeline"));
}

#[test]
fn a_taken_up_download_warning_is_not_said_again_for_its_sibling_class() {
    let started = "2026-09-29T10:00:00Z".to_string();
    let stage = super::health::Health {
        stage_failures: 4,
        stage_since: "2026-09-29T09:00:00Z".to_string(),
        last_failure_at: "2026-09-29T10:02:00Z".to_string(),
        last_diagnosis_at: "2026-09-29T10:02:00Z".to_string(),
        kind: "stage".to_string(),
        last_error: "the DMG would not verify".to_string(),
        ..super::health::Health::default()
    };
    let process = super::AnnouncedHealth::new();
    process.adopt("pipeline");
    let mut announcer = super::HealthAnnouncer::new(started.clone());
    let (title, _) = announcer
        .tick_for_process(&process, &stage, "2026-09-29T10:02:01Z", 7)
        .expect("the count is restated");
    assert_eq!(title, super::HEALTH_RESTATED_TITLE);
    let process = super::AnnouncedHealth::new();
    let mut announcer = super::HealthAnnouncer::new(started);
    let (title, _) = announcer
        .tick_for_process(&process, &stage, "2026-09-29T10:02:01Z", 7)
        .expect("announced");
    assert_eq!(title, super::health_failing_title("stage"));
}
