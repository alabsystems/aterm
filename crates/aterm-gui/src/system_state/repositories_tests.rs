// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_repository_progress::{FailureKind, Phase, PreservationReason};

#[test]
fn expanded_rows_keep_each_outcome_with_its_stage_and_honest_retry_information() {
    let mut app = SystemStateApp::new(aterm_types::macos_setup::Plan::default());
    let mut failed = Progress::new("failed", "/home/person/failed".into());
    failed.enter(Phase::Dependencies);
    failed.fail(FailureKind::Network);
    let mut deferred = Progress::new("deferred", "/home/person/deferred".into());
    deferred.usable = true;
    deferred.placed = true;
    deferred.enter(Phase::Checking);
    deferred.defer(PreservationReason::LocalChanges);
    let mut old = Progress::new("old", "/home/person/old".into());
    old.enter(Phase::Failed);
    app.repositories = vec![failed, deferred, old];
    let mut view = SystemStateViewState {
        repositories_open: true,
        ..SystemStateViewState::default()
    };
    view.repository_open.extend(
        app.repositories
            .iter()
            .map(|row| identity(&row.destination)),
    );
    let (tree, _) = table(&app, &view, 240.0, None);
    for row in &app.repositories {
        let prefix = format!(
            "system-state/repositories/repo/{}/detail/",
            identity(&row.destination)
        );
        let details = tree
            .children
            .iter()
            .filter(|node| node.key.as_str().starts_with(&prefix))
            .filter_map(|node| match &node.content {
                UiContent::Text(spec) => Some((node, spec.text.as_str())),
                UiContent::Group(spec) => spec.label.as_deref().map(|text| (node, text)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let text = details
            .iter()
            .map(|(_, text)| *text)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains("Retry timing: not recorded"));
        assert!(!text.contains("automatic") && !text.contains("://"));
        if let Some(outcome) = &row.outcome {
            assert!(text.contains(&format!("Last stage: {}", outcome.stage.label())));
            assert!(text.contains(outcome.reason.label()));
            let reason = details
                .iter()
                .find(|(_, text)| *text == outcome.reason.label())
                .unwrap()
                .0;
            assert!(matches!(reason.layout.height, Length::Fixed(height) if height > 24.0));
            if row.usable {
                assert!(text.contains("local checkout or configuration changes"));
                assert!(!text.contains("network repository operation failed"));
            } else {
                assert!(!text.contains("uncommitted changes"));
            }
        } else {
            assert!(text.contains("Outcome detail: not recorded"));
        }
    }
    assert_eq!(app.repositories.iter().filter(|row| row.usable).count(), 1);
}

#[test]
fn long_destinations_have_bounded_distinct_disclosure_actions() {
    let mut app = SystemStateApp::new(aterm_types::macos_setup::Plan::default());
    let mut view = SystemStateViewState {
        repositories_open: true,
        ..SystemStateViewState::default()
    };
    let destination = std::path::PathBuf::from(format!(
        "/home/person/{}project checkout",
        "research/".repeat(40)
    ));
    app.repositories
        .push(Progress::new("project", destination.clone()));
    let (tree, _) = table(&app, &view, 400.0, None);
    let row = tree
        .children
        .iter()
        .find(|node| node.key.as_str().ends_with("/row"))
        .unwrap();
    let key = row.children[0].key.as_str();
    let wire = format!("app/v1 view 1 {key} {key}");
    assert!(
        aterm_types::app_inspection::parse_act(&wire).is_ok(),
        "{wire}"
    );
    assert_ne!(
        identity(&destination),
        identity(&destination.with_file_name("other checkout"))
    );
    assert!(act(&mut view, key));
    let saved = view.repository_open.clone();
    app.repositories[0].enter(Phase::Downloading);
    app.repositories[0].attempt = "next-attempt".into();
    let (tree, _) = table(&app, &view, 400.0, None);
    assert_eq!(view.repository_open, saved);
    assert!(
        tree.children
            .iter()
            .any(|node| node.key.as_str().contains("/detail/"))
    );
}

#[test]
fn sorting_and_live_changes_preserve_expansion_and_unknown_totals() {
    let mut app = SystemStateApp::new(aterm_types::macos_setup::Plan::default());
    let mut view = SystemStateViewState::default();
    let mut downloading = Progress::new("repo", "/home/person/repo".into());
    downloading.enter(Phase::Downloading);
    app.repositories.push(downloading);
    assert!(act(&mut view, "system-state/repositories/disclose"));
    assert!(act(
        &mut view,
        &format!(
            "system-state/repositories/repo/{}",
            identity(std::path::Path::new("/home/person/repo"))
        )
    ));
    assert!(act(&mut view, "system-state/repositories/sort/2"));
    let (node, _) = table(&app, &view, 900.0, None);
    let bar = node
        .children
        .iter()
        .find(|node| node.key.as_str().ends_with("/bar"))
        .unwrap();
    assert!(matches!(&bar.content, UiContent::Custom(spec) if spec.progress.is_none()));
    app.repositories[0].enter(Phase::Ready);
    app.repositories[0].usable = true;
    let (node, _) = table(&app, &view, 900.0, None);
    assert!(
        view.repository_open
            .contains(&identity(std::path::Path::new("/home/person/repo")))
    );
    assert!(
        !node
            .children
            .iter()
            .any(|node| node.key.as_str().ends_with("/bar"))
    );
    let mut reveal = Vec::new();
    controls(&node, 100.0, &mut reveal);
    assert!(reveal.iter().any(|(key, y, _)| {
        key.ends_with(&identity(std::path::Path::new("/home/person/repo"))) && *y > 100.0
    }));

    // A retained acquisition can describe a formerly usable destination while the
    // current inventory has no complete checkout. Its record is not that inventory.
    let mut status = super::super::fixture_status("required-dependencies@developer").unwrap();
    let current = status.observed.clone();
    assert!(current.iter().any(|row| {
        row.subject
            == aterm_types::macos_setup::Subject::Setting(
                aterm_types::macos_setup::Setting::Repositories,
            )
            && row.now.as_deref() == Some("0 of 1")
    }));
    let recorded_at = 1_700_000_000_123;
    app.repositories[0].updated_ms = recorded_at;
    status.repositories = app.repositories.clone();
    app.publish(status);
    let (node, _) = table(&app, &view, 900.0, None);
    let overall = node
        .children
        .iter()
        .find(|node| node.key.as_str().ends_with("/overall"))
        .unwrap();
    assert!(matches!(&overall.content, UiContent::Custom(spec)
        if spec.progress == Some((1, 1))));
    assert_eq!(app.status.observed, current);
    assert_eq!(app.repositories[0].updated_ms, recorded_at);
    assert!(
        view.repository_open
            .contains(&identity(std::path::Path::new("/home/person/repo")))
    );
}

#[test]
fn progress_sort_compares_only_within_a_phase_and_keeps_unknown_last() {
    let mut app = SystemStateApp::new(aterm_types::macos_setup::Plan::default());
    for (name, phase, percent) in [
        ("unknown", Phase::Downloading, None),
        ("low", Phase::Downloading, Some(10)),
        ("later-stage", Phase::Resolving, Some(1)),
        ("high", Phase::Downloading, Some(90)),
    ] {
        let mut row = Progress::new(name, format!("/home/person/{name}").into());
        row.phase = phase;
        row.percent = percent;
        app.repositories.push(row);
    }
    let mut view = SystemStateViewState::default();
    act(&mut view, "system-state/repositories/disclose");
    for expected in [
        ["high", "low", "unknown", "later-stage"],
        ["later-stage", "low", "high", "unknown"],
    ] {
        act(&mut view, "system-state/repositories/sort/2");
        let (tree, _) = table(&app, &view, 900.0, None);
        let names = tree
            .children
            .iter()
            .filter(|node| node.key.as_str().ends_with("/row"))
            .map(|row| match &row.children[0].content {
                UiContent::Button(control) => control.spec.label.as_str(),
                _ => panic!("repository row begins with its disclosure button"),
            })
            .collect::<Vec<_>>();
        assert_eq!(names, expected);
    }
    app.repositories[0].enter(Phase::Failed);
    assert!(compare(&app.repositories[0], &app.repositories[3], 0, false).is_lt());
}

#[test]
fn elapsed_time_includes_queueing_and_stops_at_terminal_outcomes() {
    let mut row = Progress::new("repo", "/home/person/repo".into());
    row.started_ms = 10_000;
    row.updated_ms = 20_000;
    for phase in [Phase::Queued, Phase::WaitingForSpace, Phase::Downloading] {
        row.phase = phase;
        assert_eq!(elapsed_seconds(&row, 70_000), 60, "{phase:?}");
        assert_eq!(elapsed_seconds(&row, 130_000), 120, "{phase:?}");
    }
    for phase in [
        Phase::Ready,
        Phase::Failed,
        Phase::Deferred,
        Phase::Interrupted,
    ] {
        row.phase = phase;
        assert_eq!(elapsed_seconds(&row, 70_000), 10, "{phase:?}");
        assert_eq!(elapsed_seconds(&row, 130_000), 10, "{phase:?}");
    }
}

#[test]
fn report_age_advances_without_a_new_report_and_rejects_unknown_or_future_clocks() {
    let updated = 10_000;
    assert_eq!(report_age_seconds(updated, 10_999), Some(0));
    assert_eq!(report_age_seconds(updated, 70_000), Some(60));
    assert_eq!(report_age_seconds(updated, 130_000), Some(120));
    assert_eq!(report_age_seconds(0, 130_000), None);
    assert_eq!(report_age_seconds(130_001, 130_000), None);
}

#[test]
fn queued_repository_clocks_request_refresh_without_new_snapshots() {
    let mut app = SystemStateApp::new(aterm_types::macos_setup::Plan::default());
    app.publish(super::super::fixture_status("set-up").unwrap());
    let mut row = Progress::new("repo", "/home/person/repo".into());
    row.phase = Phase::Ready;
    app.repositories.push(row);
    let now = aterm_time::Instant::now();
    assert!(
        !app.tick_at(now),
        "a finished repository owes no clock repaint"
    );
    for phase in [Phase::Queued, Phase::WaitingForSpace] {
        app.repositories[0].phase = phase;
        assert!(app.tick_at(now), "{phase:?} elapsed time needs a repaint");
    }
    app.repositories[0].phase = Phase::Failed;
    assert!(
        !app.tick_at(now),
        "a failed outcome stops the clock repaint"
    );
}

#[test]
fn stale_transfer_rates_are_not_presented_as_current_speed() {
    let mut row = Progress::new("repo", "/home/person/repo".into());
    assert!(row.git_line("Receiving objects: 75% (3/4), 1 GiB | 8 MiB/s"));
    assert_eq!(speed_words(&row, row.updated_ms + 1000), "8 MiB/s");
    assert_eq!(speed_words(&row, row.updated_ms + 35000), "quiet 35s");
    let mut view = SystemStateViewState::default();
    act(&mut view, "system-state/repositories/sort/2");
    assert_eq!(sort_label(&view, 2, "Progress"), "Progress ↓");
    act(&mut view, "system-state/repositories/sort/2");
    assert_eq!(sort_label(&view, 2, "Progress"), "Progress ↑");
}
