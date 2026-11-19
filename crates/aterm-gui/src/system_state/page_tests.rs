// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The setup screen as `docs/DESIGN-init-on-by-default-2026-10-25.md` §4 draws it: each state of
//! §4.3 in its words, the one place for what needs the person (§4.8), the page that never
//! scrolls (§4.1), and what VoiceOver hears (§4.7).

use std::collections::HashSet;
use std::time::Duration;

use super::lines::{self, CardItem, Line, LineView, Mark};
use super::*;
use crate::native_app::ViewMotionCx;
use crate::native_ui::{CompiledUi, LogicalRect, SemanticNode, SemanticRole, SemanticValue};

fn cx(viewport: LogicalRect) -> ViewCx<'static> {
    ViewCx {
        viewport,
        animation_phase_ms: 0,
        motion: ViewMotionCx::default(),
        terminal_font_px: 13.0,
        terminal_scale: 2.0,
        semantic_font: None,
        document: None,
    }
}

fn compile_in(app: &SystemStateApp, view: &SystemStateViewState, w: f32, h: f32) -> CompiledUi {
    let viewport = LogicalRect::new(0.0, 0.0, w, h);
    app.view(view, &cx(viewport))
        .compile(viewport)
        .expect("the page compiles")
}

fn status_of(name: &str) -> SetupStatus {
    fixture_status(name).expect("a named scenario")
}

#[test]
fn compact_password_scrollbar_keeps_the_four_point_grid() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let app = reading(&status_of("required-password"));
    let view = SystemStateViewState::default();
    let _compiled = compile_in(&app, &view, 346.0, 400.0);
    assert!(view.limit.get() > 0.0);
    let room = view.room.get();
    assert!(room > 0.0);
    assert!((room / 4.0 - (room / 4.0).round()).abs() < 0.01);
}

/// A page that reads `status` under its own plan.
fn reading(status: &SetupStatus) -> SystemStateApp {
    let mut app = SystemStateApp::new(status.plan.clone().unwrap_or_default());
    app.publish(status.clone());
    app
}

/// What the page draws for `app`: its lines in page order, the card and the hero.
fn drawn(app: &SystemStateApp) -> (Vec<LineView>, Vec<CardItem>, lines::Hero) {
    let rows = page::page_rows(app);
    let card = lines::card_items(app, &rows);
    let lines = Line::ALL
        .into_iter()
        .map(|line| lines::line_view(app, line, &rows, &card))
        .collect();
    let hero = lines::hero(app, &rows, &card);
    (lines, card, hero)
}

fn line_of(app: &SystemStateApp, line: Line) -> LineView {
    drawn(app)
        .0
        .into_iter()
        .find(|drawn| drawn.line == line)
        .expect("every line is drawn")
}

fn semantic<'a>(compiled: &'a CompiledUi, key: &str) -> &'a SemanticNode {
    compiled
        .semantic(&UiKey::new(key))
        .unwrap_or_else(|| panic!("{key} is in the tree"))
}

/// A view with `line` open.
fn open(app: &mut SystemStateApp, line: Line) -> SystemStateViewState {
    let mut view = SystemStateViewState::default();
    app.act(
        &mut view,
        &format!("system-state/line/{}/disclose", line.slug()),
        None,
    );
    view
}

#[test]
fn first_launch_says_setting_up_and_starting_before_any_step_reports() {
    // The launch's page, before the worker names the plan: the run is going.
    let mut app = SystemStateApp::opened();
    app.publish(SetupStatus {
        busy: true,
        ..SetupStatus::default()
    });
    let (_, card, hero) = drawn(&app);
    assert_eq!(
        (hero.headline.as_str(), hero.sub.as_str()),
        (lines::SETTING_UP, lines::STARTING)
    );
    assert!(
        card.is_empty(),
        "nothing asks the person before a step runs"
    );
    // The plan known and the run going with nothing reported yet: still starting.
    let app = reading(&SetupStatus {
        busy: true,
        running: Vec::new(),
        ..status_of("running")
    });
    let hero = drawn(&app).2;
    assert_eq!(hero.headline, lines::SETTING_UP);
}

#[test]
fn a_run_marks_its_active_line_working_with_its_live_phrase() {
    let app = reading(&status_of("running"));
    let (lines, card, hero) = drawn(&app);
    assert_eq!(hero.headline, lines::SETTING_UP);
    assert!(hero.stop, "Stop shows while a run goes");
    assert!(hero.sub.ends_with(" done"), "{}", hero.sub);
    let working: Vec<&LineView> = lines
        .iter()
        .filter(|line| line.mark == Mark::Working)
        .collect();
    assert!(!working.is_empty(), "{lines:#?}");
    for line in working {
        assert!(!line.phrase.is_empty() && !line.quiet, "{line:?}");
        // Its phrase is where the run stands, never a bare state word.
        assert_ne!(line.phrase, "Working", "{line:?}");
    }
    assert!(card.iter().all(|item| item.mark != Mark::Working));
}

#[test]
fn a_wait_aterm_ends_by_itself_shows_its_dots_and_its_reason() {
    let mut status = status_of("set-up");
    let reading_of = status
        .observed
        .iter_mut()
        .find(|row| row.subject == Subject::Setting(Setting::Repositories))
        .unwrap();
    reading_of.standing = Standing::Waiting;
    reading_of.note = Some("Waits for the vault to finish syncing this Mac".to_owned());
    let app = reading(&status);
    let developer = line_of(&app, Line::Developer);
    assert_eq!(developer.mark, Mark::Waiting);
    assert_eq!(
        developer.phrase, "Waiting for the vault to finish syncing this Mac",
        "it says what it waits for"
    );
    assert!(
        drawn(&app).1.is_empty(),
        "a wait asks nothing of the person"
    );
}

#[test]
fn incomplete_or_unread_repositories_never_give_developer_a_green_check() {
    for standing in [Standing::Differs, Standing::Waiting, Standing::Unknown] {
        let mut status = status_of("set-up");
        let repositories = status
            .observed
            .iter_mut()
            .find(|row| row.subject == Subject::Setting(Setting::Repositories))
            .unwrap();
        repositories.standing = standing;
        repositories.now = (standing != Standing::Unknown).then(|| "0 of 3".to_owned());
        repositories.wanted = "All 3".to_owned();
        repositories.note = Some("Waiting for repository metadata from the Secrets vault".into());
        let app = reading(&status);
        let developer = line_of(&app, Line::Developer);
        assert_ne!(developer.mark, Mark::Done, "{standing:?}: {developer:?}");
        let (rows, _, hero) = drawn(&app);
        assert!(hero.done < hero.total, "{standing:?}: {rows:?}");
    }
    // A fully read and completed inventory still earns the check.
    assert_eq!(
        line_of(&reading(&status_of("set-up")), Line::Developer).mark,
        Mark::Done
    );
}

#[test]
fn required_dependency_fixture_keeps_zero_ready_visibly_unfinished_without_a_token_action() {
    let status = status_of("required-dependencies@developer");
    let app = reading(&status);
    let developer = line_of(&app, Line::Developer);
    assert_eq!(developer.mark, Mark::NeedsYou);
    let row = row_view(
        Subject::Setting(Setting::Repositories),
        &status,
        Selection::all(),
    );
    assert_eq!(
        row.value,
        RowValue::Change {
            now: "0 of 1".into(),
            wanted: "All 1".into()
        }
    );
    assert_eq!(row.chip, Some(Chip::NeedsYou));
    assert!(!row.install_tools);
    assert!(
        row.note
            .as_deref()
            .is_some_and(|note| note.contains("~/project/vendor/library"))
    );
    assert!(!row.label.contains("Needs sign-in"));
    let (_, _, hero) = drawn(&app);
    assert!(hero.done < hero.total);
    assert_eq!(
        fixture_line("required-dependencies@developer"),
        Some(Line::Developer)
    );
}

#[test]
fn what_needs_you_is_pinned_in_the_card_once_and_the_headline_says_so() {
    let app = reading(&status_of("needs-you"));
    let (lines, card, hero) = drawn(&app);
    assert!(!card.is_empty());
    let needs = card
        .iter()
        .filter(|item| item.mark == Mark::NeedsYou)
        .count();
    assert_eq!(
        hero.headline,
        if needs == 1 {
            "1 thing needs you".to_owned()
        } else {
            format!("{needs} things need you")
        }
    );
    for item in card.iter().filter_map(|item| item.subject) {
        let line = lines
            .iter()
            .find(|line| line.line == Line::of(item))
            .unwrap();
        // The list says so too, so it never reads as all done; the card holds the words.
        let row = line
            .rows
            .iter()
            .find(|row| row.subject == item)
            .unwrap_or_else(|| panic!("{item:?} is in the card and gone from its line"));
        assert!(
            row.note.is_none() && !row.turn_on_sync && !row.install_tools,
            "{item:?}'s words and buttons are the card's: {row:?}"
        );
        assert!(
            matches!(line.mark, Mark::NeedsYou | Mark::Failed),
            "its line says it needs you: {line:?}"
        );
    }
}

#[test]
fn a_failed_step_says_setup_will_try_again_while_tries_remain_then_what_failed_and_what_aterm_does_next()
 {
    let retrying = status_of("failed-retrying");
    let app = reading(&retrying);
    let (_, card, hero) = drawn(&app);
    assert_eq!(hero.headline, "Setup will try again");
    assert!(
        card.iter().all(|item| item.mark != Mark::Failed),
        "a step aterm tries again is no failure: {card:?}"
    );
    let stopped = SetupStatus {
        retrying: false,
        ..retrying
    };
    let app = reading(&stopped);
    let (_, card, hero) = drawn(&app);
    assert_eq!(hero.headline, "1 step failed");
    let failed: Vec<&CardItem> = card
        .iter()
        .filter(|item| item.mark == Mark::Failed)
        .collect();
    assert_eq!(failed.len(), 1, "{card:?}");
    assert_eq!(failed[0].title, "GitHub repositories failed");
    let named = reading(&status_of("named-result-failed"));
    let (lines, card, hero) = drawn(&named);
    let item = card
        .iter()
        .find(|item| item.mark == Mark::Failed)
        .expect("the failed step is in the card");
    assert_eq!(item.title, "GitHub repositories failed");
    assert!(
        item.button.is_none(),
        "aterm runs it again by itself: nothing waits for a press"
    );
    let reason = item.reason.as_deref().unwrap_or_default();
    assert!(
        reason.ends_with(lines::TRIES_BY_ITSELF),
        "the card says what aterm does next: {reason:?}"
    );
    assert_eq!(hero.headline, "1 step failed");
    assert!(!hero.start, "no Set up now while aterm runs it again");
    let developer = lines
        .iter()
        .find(|line| line.line == Line::Developer)
        .expect("the Developer line");
    assert_eq!(
        (developer.mark, developer.phrase.as_str()),
        (Mark::Failed, "Failed"),
        "its line says Failed, never a row"
    );
}

/// Two rows a whole Workstation stage failed for are two card items, each with its own key and
/// what aterm does next, and no control: the page runs them again by itself.
#[test]
fn two_failed_workstation_rows_are_two_card_items_that_say_what_aterm_does_next() {
    let app = reading(&status_of("named-results-failed"));
    let (_, card, hero) = drawn(&app);
    let failed: Vec<&CardItem> = card
        .iter()
        .filter(|item| item.mark == Mark::Failed)
        .collect();
    assert_eq!(failed.len(), 2, "{card:?}");
    assert_ne!(failed[0].id, failed[1].id);
    assert_eq!(hero.headline, "2 steps failed");
    for item in &failed {
        assert!(item.button.is_none(), "{item:?}");
        assert!(
            item.reason
                .as_deref()
                .is_some_and(|reason| reason.ends_with(lines::TRIES_BY_ITSELF)),
            "{item:?}"
        );
    }
    let view = SystemStateViewState::default();
    for width in [320.0, 900.0] {
        let compiled = compile_in(&app, &view, width, 644.0);
        for item in &failed {
            let key = format!("system-state/card/{}", item.id);
            assert!(
                compiled.semantic(&UiKey::new(key.clone())).is_some(),
                "{key} at {width}"
            );
        }
    }
}

#[test]
fn a_finished_run_says_your_mac_is_ready_and_how_long_it_took() {
    let status = SetupStatus {
        ready_in: Some(Duration::from_secs(72)),
        ..status_of("set-up")
    };
    let hero = drawn(&reading(&status)).2;
    assert_eq!(hero.headline, "Your Mac is ready");
    assert_eq!(
        hero.sub,
        format!("{0} of {0} · ready in 1 min 12 s", hero.total)
    );
    assert!(!hero.stop && !hero.start);
}

#[test]
fn a_check_says_checking_this_mac_and_nothing_moves() {
    let status = SetupStatus {
        busy: true,
        operation: Some(SetupOperation::Check),
        ..status_of("set-up")
    };
    let (lines, _, hero) = drawn(&reading(&status));
    assert_eq!(hero.headline, CHECKING);
    assert!(hero.sub.is_empty());
    assert!(
        lines.iter().all(|line| line.mark != Mark::Working),
        "{lines:#?}"
    );

    let complete = status_of("set-up");
    let mut app = reading(&complete);
    let view = open(&mut app, Line::Mac);
    let before = compile_in(&app, &view, 900.0, 644.0);
    let old_rows = page::page_rows(&app);
    let complete_hero = drawn(&app).2;
    let caps = Subject::Setting(Setting::CapsLock);
    let vault = Subject::Setting(Setting::Secrets);
    let source = complete.plan.as_ref().unwrap().vault_url.clone();
    let mut checking = complete.clone();
    checking.last_known = std::mem::take(&mut checking.observed);
    checking.observing = checking
        .last_known
        .iter()
        .map(|row| {
            (
                row.subject,
                row.subject.uses_vault_source().then(|| source.clone()),
                false,
            )
        })
        .collect();
    app.publish(checking.clone());
    let during = compile_in(&app, &view, 900.0, 644.0);
    let (retained_lines, _, hero) = drawn(&app);
    assert_eq!(hero.headline, CHECKING);
    assert_eq!(
        hero.sub,
        format!(
            "{} of {} as last read",
            complete_hero.done, complete_hero.total
        )
    );
    assert!(hero.as_last_read);
    for line in retained_lines {
        assert_eq!(
            line.mark,
            Mark::Done,
            "{} keeps its last useful check",
            line.line.title()
        );
        assert_eq!(line.word, "as last read");
        assert!(line.phrase.contains("checking"));
    }
    assert_eq!(
        (hero.done, hero.total),
        (complete_hero.done, complete_hero.total),
        "the display keeps confirmed history while new readings are pending"
    );
    let progress = semantic(&during, "system-state/progress");
    assert_eq!(progress.label, "Setup progress as last read");
    #[allow(clippy::cast_precision_loss, reason = "a count of rows")]
    let retained_progress = SemanticValue::Number {
        value: complete_hero.done as f64,
        minimum: 0.0,
        maximum: complete_hero.total as f64,
    };
    assert_eq!(progress.value, retained_progress);
    assert!(!app.headline().set_up);
    for old in old_rows
        .iter()
        .filter(|row| row.subject.group() == Group::Input || row.subject.group() == Group::Desktop)
    {
        let current = app.reviewed_row(old.subject);
        assert_eq!(current.note, old.note, "checking adds no per-row note");
        let key = format!("system-state/row/{}", row_slug(old.subject));
        assert_eq!(
            semantic(&during, &key).rect,
            semantic(&before, &key).rect,
            "retaining the same value keeps the open row's footprint"
        );
    }
    let mut queued = checking.clone();
    for (_, _, waiting) in &mut queued.observing {
        *waiting = true;
    }
    app.publish(queued);
    let queued_frame = compile_in(&app, &view, 900.0, 644.0);
    for old in old_rows
        .iter()
        .filter(|row| row.subject.group() == Group::Input || row.subject.group() == Group::Desktop)
    {
        let key = format!("system-state/row/{}", row_slug(old.subject));
        assert_eq!(
            semantic(&queued_frame, &key).rect,
            semantic(&before, &key).rect,
            "queuing a check preserves the same row footprint"
        );
    }
    app.publish(checking.clone());
    let held = app.reviewed_row(caps);
    assert!(matches!(held.value, RowValue::LastKnown(_)));
    assert_eq!(held.chip, Some(Chip::LastKnown));
    assert!(!held.settled(), "history supplies no current readiness");
    assert!(held.label.contains("Last read:"));
    assert!(!held.install_tools && !held.turn_on_sync);

    for (access, note) in [
        (
            Access::FullDiskAccess,
            aterm_types::macos_setup::ACCESS_OFFERED,
        ),
        (
            Access::Debugger,
            aterm_types::macos_setup::DEBUGGER_NOT_ASKED,
        ),
    ] {
        let subject = Subject::Access(access);
        let mut offered = complete.clone();
        let grant = offered
            .observed
            .iter_mut()
            .find(|row| row.subject == subject)
            .unwrap();
        grant.standing = Standing::NotNeeded;
        grant.now = None;
        grant.note = Some(note.to_owned());
        let mut offered_app = reading(&offered);
        let offered_hero = drawn(&offered_app).2;
        assert!(offered_hero.done < offered_hero.total);
        offered.last_known = std::mem::take(&mut offered.observed);
        offered.observing = offered
            .last_known
            .iter()
            .map(|row| {
                (
                    row.subject,
                    row.subject.uses_vault_source().then(|| source.clone()),
                    false,
                )
            })
            .collect();
        offered_app.publish(offered);
        let held = offered_app.reviewed_row(subject);
        assert!(matches!(held.value, RowValue::LastKnown(_)));
        assert_ne!(
            held.chip,
            Some(Chip::LastKnown),
            "an offered grant was never confirmed"
        );
        assert!(!held.settled() && held.open.is_none());
        let permissions = line_of(&offered_app, Line::Permissions);
        assert_ne!(permissions.mark, Mark::Done);
        assert!(!permissions.phrase.contains("Granted"));
        let historical = drawn(&offered_app).2;
        assert_eq!(
            (historical.done, historical.total),
            (offered_hero.done, offered_hero.total)
        );
        assert!(!offered_app.headline().set_up);
    }

    checking.observing.clear();
    app.publish(checking.clone());
    let idle = app.headline();
    assert_ne!(
        idle.text, CHECKING,
        "a completed empty/skipped batch is no running check"
    );
    assert!(
        !idle.text.is_empty() && !idle.set_up && !idle.start && idle.show.is_none(),
        "absence supplies neither success nor a fabricated attempted-read failure"
    );
    assert!(app.status.observed.is_empty() && app.status.unconfirmed_observations.is_empty());
    assert!(matches!(
        app.reviewed_row(caps).value,
        RowValue::LastKnown(_)
    ));
    for line in drawn(&app).0 {
        assert_eq!(line.mark, Mark::Done);
        assert_eq!(line.word, "as last read");
        assert!(!line.phrase.to_lowercase().contains("checking"));
    }
    checking.unconfirmed_observations = vec![caps];
    app.publish(checking.clone());
    assert!(app.headline().text.contains("Could not read"));
    assert!(
        matches!(app.reviewed_row(caps).value, RowValue::LastKnown(_)),
        "an attempted Unknown response does not erase the useful display"
    );
    assert_eq!(
        (drawn(&app).2.done, drawn(&app).2.total),
        (complete_hero.done, complete_hero.total),
        "an unanswered new reading leaves the explicitly historical count intact"
    );
    for standing in [Standing::Failed, Standing::Waiting, Standing::NeedsYou] {
        checking.unconfirmed_observations.clear();
        checking.observed = vec![Observation {
            standing,
            note: Some("current native reason".into()),
            ..Observation::unknown(caps)
        }];
        app.publish(checking.clone());
        let latest = app.reviewed_row(caps);
        assert_eq!(latest.standing, Some(standing));
        assert_ne!(latest.chip, Some(Chip::LastKnown));
        assert_eq!(latest.note.as_deref(), Some("current native reason"));
        assert!(matches!(latest.value, RowValue::LastKnown(_)));
        assert_ne!(
            drawn(&app)
                .0
                .into_iter()
                .find(|line| line.line == Line::Mac)
                .unwrap()
                .mark,
            Mark::Done,
            "a current problem outranks the last useful group check"
        );
        assert!(
            drawn(&app).2.done < complete_hero.done,
            "a current problem is not counted complete from history"
        );
    }
    let mut off = checking.clone();
    off.plan
        .as_mut()
        .unwrap()
        .selection
        .set(Setting::CapsLock, false);
    assert!(
        matches!(reading(&off).reviewed_row(caps).value, RowValue::Muted(_)),
        "a current Off choice takes precedence over history"
    );

    let mut partial = checking.clone();
    partial.observed.clear();
    partial.last_known.retain(|row| row.subject == caps);
    app.publish(partial.clone());
    let incomplete = drawn(&app)
        .0
        .into_iter()
        .find(|line| line.line == Line::Mac)
        .unwrap();
    assert_ne!(
        incomplete.mark,
        Mark::Done,
        "one old row cannot complete its whole group"
    );
    assert!(!incomplete.phrase.to_lowercase().contains("checking"));
    assert!(!app.headline().set_up);

    partial.last_known.extend(
        complete
            .observed
            .iter()
            .filter(|row| matches!(row.subject, Subject::Access(_)))
            .cloned(),
    );
    app.publish(partial.clone());
    let (before_lines, _, before_hero) = drawn(&app);
    let partial_before = compile_in(&app, &view, 900.0, 644.0);
    let granted = before_lines
        .iter()
        .find(|line| line.line == Line::Permissions)
        .unwrap();
    assert!(granted.phrase.starts_with("Granted · as last read"));
    partial.observing = complete
        .observed
        .iter()
        .map(|row| {
            (
                row.subject,
                row.subject.uses_vault_source().then(|| source.clone()),
                false,
            )
        })
        .collect();
    app.publish(partial);
    let (during_lines, _, during_hero) = drawn(&app);
    let partial_during = compile_in(&app, &view, 900.0, 644.0);
    for key in [
        "system-state/line/mac/switch",
        "system-state/line/permissions/switch",
        "system-state/row/caps-lock",
    ] {
        assert_eq!(
            semantic(&partial_during, key).rect,
            semantic(&partial_before, key).rect,
            "checking partial history preserves the header and row footprint: {key}"
        );
    }
    assert_eq!(
        (during_hero.done, during_hero.total),
        (before_hero.done, before_hero.total),
        "starting a check preserves a partial historical count"
    );
    for line in [Line::Mac, Line::Permissions] {
        let before = before_lines
            .iter()
            .find(|shown| shown.line == line)
            .unwrap();
        let during = during_lines
            .iter()
            .find(|shown| shown.line == line)
            .unwrap();
        assert_eq!((during.mark, during.word), (before.mark, before.word));
        assert!(during.phrase.ends_with("checking"));
        assert!(
            during.phrase.starts_with(
                before
                    .phrase
                    .split(" · current reading unavailable")
                    .next()
                    .unwrap()
            ),
            "checking preserves the last useful group summary: {during:?}"
        );
    }
    assert!(!app.headline().set_up);

    let mut other_source = complete.clone();
    other_source.observing = vec![(vault, Some("https://other.invalid/vault".into()), false)];
    let other = reading(&other_source);
    assert_eq!(
        other.reviewed_row(vault).chip,
        Some(Chip::InPlace),
        "another vault's progress cannot mark this source as checking"
    );
    let mut only_other_vault = complete.clone();
    only_other_vault.observed.clear();
    only_other_vault.last_known = vec![
        complete
            .observed
            .iter()
            .find(|row| row.subject == vault)
            .unwrap()
            .clone(),
    ];
    only_other_vault.observed_vault_url = Some("https://other.invalid/vault".into());
    let foreign = reading(&only_other_vault);
    let foreign_hero = drawn(&foreign).2;
    assert_eq!((foreign_hero.done, foreign_hero.total), (0, 0));
    assert!(
        !foreign_hero.as_last_read,
        "another vault's history supplies no display progress"
    );
    other_source.observing = vec![(vault, Some(source), true)];
    assert_eq!(
        drawn(&reading(&other_source)).2.headline,
        "Waiting to read this Mac…"
    );
    let trackpad = Subject::Setting(Setting::TrackpadScrolling);
    for width in [480.0, 600.0, 900.0] {
        let mut not_needed = complete.clone();
        let absent_trackpad = not_needed
            .observed
            .iter_mut()
            .find(|row| row.subject == trackpad)
            .unwrap();
        absent_trackpad.standing = Standing::NotNeeded;
        absent_trackpad.now = None;
        let mut not_needed_app = reading(&not_needed);
        let not_needed_view = open(&mut not_needed_app, Line::Mac);
        let first = compile_in(&not_needed_app, &not_needed_view, width, 644.0);
        let first_hero = drawn(&not_needed_app).2;
        not_needed.last_known = std::mem::take(&mut not_needed.observed);
        not_needed.observing = not_needed
            .last_known
            .iter()
            .map(|row| {
                (
                    row.subject,
                    row.subject
                        .uses_vault_source()
                        .then(|| complete.plan.as_ref().unwrap().vault_url.clone()),
                    false,
                )
            })
            .collect();
        not_needed_app.publish(not_needed.clone());
        let pending = compile_in(&not_needed_app, &not_needed_view, width, 644.0);
        let key = format!("system-state/row/{}", row_slug(trackpad));
        assert_eq!(
            semantic(&pending, &key).rect,
            semantic(&first, &key).rect,
            "a confirmed not-needed row reserves the same check footprint at {width}"
        );
        let pending_hero = drawn(&not_needed_app).2;
        assert_eq!(
            (pending_hero.done, pending_hero.total),
            (first_hero.done, first_hero.total)
        );
        assert!(!not_needed_app.headline().set_up);
        // One completed result may arrive while the other rows are still being read.
        let previous = not_needed
            .last_known
            .iter()
            .find(|row| row.subject == trackpad)
            .unwrap()
            .clone();
        not_needed.observed = vec![previous];
        not_needed_app.publish(not_needed);
        let partial = compile_in(&not_needed_app, &not_needed_view, width, 644.0);
        assert_eq!(
            semantic(&partial, &key).rect,
            semantic(&first, &key).rect,
            "a newly reconfirmed not-needed row keeps the same historical group footprint"
        );
        assert_eq!(line_of(&not_needed_app, Line::Mac).word, "as last read");
    }
    app.publish(complete);
    assert_eq!(app.reviewed_row(caps).chip, Some(Chip::InPlace));
    assert!(!drawn(&app).2.as_last_read);
}

#[test]
fn profile_lines_say_their_scope_and_checking_while_the_alab_answer_is_pending() {
    let mut status = status_of("non-alab");
    status.provisional = true;
    if let Some(vault) = status
        .observed
        .iter_mut()
        .find(|row| row.subject == Subject::Setting(Setting::Secrets))
    {
        vault.standing = Standing::Waiting;
    }
    assert!(lines::scope_pending(&status));
    let app = reading(&status);
    for line in [Line::Mac, Line::Accounts] {
        let drawn = line_of(&app, line);
        assert_eq!(
            (drawn.mark, drawn.word, drawn.phrase.as_str(), drawn.quiet),
            (Mark::Pending, lines::CHECKING_WORD, line.scope(), true),
            "{line:?}"
        );
        assert_eq!(drawn.switch, None, "a pending answer never reads as off");
    }
}

#[test]
fn an_off_line_reads_left_as_is_quietly_and_opens_to_nothing() {
    let mut app = reading(&status_of("non-alab"));
    for (line, phrase) in [
        (Line::Mac, lines::LEFT_AS_IS),
        (Line::Accounts, lines::LEAVES_ACCOUNTS),
    ] {
        let drawn = line_of(&app, line);
        assert_eq!(
            (drawn.mark, drawn.phrase.as_str(), drawn.quiet),
            (Mark::Off, phrase, true)
        );
        assert!(!drawn.on);
        // An Off line shows none of the rows its switch governs: Mac settings keeps only
        // Spotlight indexing, which no switch governs (FL-188).
        let rows: Vec<Subject> = drawn.rows.iter().map(|row| row.subject).collect();
        if line == Line::Mac {
            assert_eq!(rows, [Subject::Access(Access::SpotlightScope)]);
        } else {
            assert!(rows.is_empty() && !drawn.expandable());
        }
    }
    // Its switch still says where it stands and turns it on.
    let compiled = compile_in(&app, &SystemStateViewState::default(), 900.0, 644.0);
    let switch = semantic(&compiled, "system-state/line/mac/switch");
    assert_eq!(
        (switch.label.as_str(), &switch.value),
        ("Mac settings: Left as is", &SemanticValue::Bool(false))
    );
    assert!(
        compiled
            .semantic(&UiKey::new("system-state/line/accounts/disclose"))
            .is_none(),
        "an Off line with no row has nothing to open"
    );
    let mut view = SystemStateViewState::default();
    app.act(
        &mut view,
        "system-state/line/mac/switch",
        Some(SemanticInput::Bool(true)),
    );
    assert!(
        Line::Mac
            .settings()
            .iter()
            .all(|bit| app.plan.selection.contains(*bit))
    );
}

#[test]
fn a_lines_switch_and_its_phrase_say_both_where_it_stands_and_what_is_wanted() {
    let app = reading(&status_of("mixed"));
    let compiled = compile_in(&app, &SystemStateViewState::default(), 900.0, 644.0);
    for line in drawn(&app).0 {
        let slug = line.line.slug();
        // Never a bare switch: VoiceOver hears where the line stands now, its phrase, and in the
        // switch's value what is wanted.
        let switch = semantic(&compiled, &format!("system-state/line/{slug}/switch"));
        assert_eq!(switch.label, line.label);
        // The state is said once: put in front, or in the phrase already.
        assert!(
            switch
                .label
                .starts_with(&format!("{}, {}: ", line.line.title(), line.word))
                || (switch
                    .label
                    .starts_with(&format!("{}: ", line.line.title()))
                    && switch.label.to_lowercase().contains(line.word)),
            "{}",
            switch.label
        );
        assert_eq!(switch.value, SemanticValue::Bool(line.on));
        assert!(!line.phrase.is_empty(), "{line:?}");
        if line.expandable() {
            let disclose = semantic(&compiled, &format!("system-state/line/{slug}/disclose"));
            assert_eq!(
                disclose.label, "Details",
                "the line's name and state are said once, on the switch"
            );
        }
    }
    // A line whose rows differ names how many and the first, now beside wanted.
    let changing = drawn(&app)
        .0
        .into_iter()
        .find(|line| line.phrase.contains(" → "))
        .expect("a mixed Mac has a row to change");
    assert!(changing.label.contains(" to "), "{}", changing.label);
}

#[test]
fn only_one_line_is_open_at_a_time() {
    let mut app = reading(&status_of("mixed"));
    let mut view = SystemStateViewState::default();
    app.act(&mut view, "system-state/line/mac/disclose", None);
    assert_eq!(view.open_line(), Some("mac"));
    app.act(&mut view, "system-state/line/secrets/disclose", None);
    assert_eq!(view.open_line(), Some("secrets"));
    app.act(&mut view, "system-state/line/secrets/disclose", None);
    assert_eq!(view.open_line(), None);
}

/// Every key a compiled page names a row or a card item by, for each fixture with each line open
/// in turn: no row is drawn twice, and none is both a line's row and the card's.
#[test]
fn no_row_appears_twice_on_any_page() {
    for &name in FIXTURES {
        let status = status_of(name);
        let mut app = reading(&status);
        let (_, card, _) = drawn(&app);
        // Every item is some row's, which stays on its line once.
        assert!(
            card.iter().all(|item| item.subject.is_some()),
            "{name}: a card item names no row: {card:?}"
        );
        let carded: HashSet<Subject> = card.iter().filter_map(|item| item.subject).collect();
        assert_eq!(
            carded.len(),
            card.len(),
            "{name}: a row in the card twice: {card:?}"
        );
        for line in Line::ALL {
            let view = open(&mut app, line);
            let compiled = compile_in(&app, &view, 900.0, 4000.0);
            let mut seen = HashSet::new();
            for node in &compiled.semantics {
                let key = node.key.as_str();
                let Some(rest) = key.strip_prefix("system-state/row/") else {
                    continue;
                };
                if rest.contains('/') {
                    continue;
                }
                assert!(seen.insert(rest.to_owned()), "{name}: {key} drawn twice");
            }
        }
    }
}

/// The worst case §4.1 names: Secrets & identity open, with its vault, the Sync rows that ride it
/// and the fields of another vault, beside a card, at 900 × 644. The page never scrolls: every
/// node outside the list sits inside the window, and the list scrolls inside itself.
#[test]
fn the_worst_case_secrets_line_open_at_900_by_644_never_scrolls_the_page() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    for name in [
        "needs-you",
        "vault-unreached",
        "vault-not-syncing",
        "sync-off",
    ] {
        let mut app = reading(&status_of(name));
        let mut view = open(&mut app, Line::Secrets);
        app.act(&mut view, "system-state/row/secrets/disclose", None);
        let compiled = compile_in(&app, &view, 900.0, 644.0);
        let list = semantic(&compiled, "system-state/groups").rect;
        assert!(
            list.bottom() <= 644.0 + 0.01,
            "{name}: the list ends at {}",
            list.bottom()
        );
        for node in &compiled.semantics {
            if node.offscreen
                || node.key.as_str().starts_with("system-state/row/")
                || node.key.as_str().starts_with("system-state/line/")
                || node.key.as_str().starts_with("system-state/vault")
                || matches!(
                    node.key.as_str(),
                    "system-state/url" | "system-state/password"
                )
            {
                continue;
            }
            assert!(
                node.rect.y >= -0.01 && node.rect.bottom() <= 644.0 + 0.01,
                "{name}: {} at {:?} is outside the window",
                node.key.as_str(),
                node.rect
            );
        }
        // The open line's head stays in view, and the hero and the card above it.
        for key in ["system-state/progress", "system-state/headline"] {
            let node = semantic(&compiled, key);
            assert!(
                !node.offscreen && node.rect.bottom() <= list.y + 0.01,
                "{name}: {key}"
            );
        }
    }
}

/// Collapsed, every fixture's page fits a 900 × 644 window with its list unscrolled (§4.1): the
/// window's measures reserve the room of the page's fullest ordinary moment, a card and a note
/// beside the footer included. NEGATIVE CONTROL: the fixtures hold cards, notes and footers, so
/// the room is used.
#[test]
fn every_collapsed_page_fits_900_by_644_with_no_scrolling() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let (mut carded, mut noted) = (0, 0);
    for &name in FIXTURES {
        let app = reading(&status_of(name));
        let view = SystemStateViewState::default();
        let compiled = compile_in(&app, &view, 900.0, 644.0);
        assert!(
            view.limit.get() < 0.5,
            "{name}: the collapsed list scrolls in 900 × 644"
        );
        let list = semantic(&compiled, "system-state/groups").rect;
        assert!(
            list.bottom() <= 644.0 + 0.01,
            "{name}: the list ends at {}",
            list.bottom()
        );
        let has = |key: &str| compiled.paint.iter().any(|node| node.key.as_str() == key);
        carded += usize::from(has("system-state/card"));
        noted += usize::from(has("system-state/notes"));
    }
    assert!(carded > 3 && noted > 3, "the fixtures hold cards and notes");
}

/// Every fixture renders at the window sizes the captures use (120 × 48 and ~46 columns of a
/// 13 pt terminal) and compact, collapsed and with each line open.
#[test]
fn every_fixture_renders_at_every_size_with_every_line_open() {
    for &name in FIXTURES {
        let mut app = reading(&status_of(name));
        for (w, h) in [
            (900.0, 644.0),
            (999.0, 900.0),
            (380.0, 900.0),
            (900.0, 360.0),
        ] {
            let _ = compile_in(&app, &SystemStateViewState::default(), w, h);
            for line in Line::ALL {
                let view = open(&mut app, line);
                let _ = compile_in(&app, &view, w, h);
            }
        }
    }
}

#[test]
fn voiceover_hears_the_heading_its_live_phase_the_progress_the_lines_and_the_card() {
    let app = reading(&status_of("walk"));
    let compiled = compile_in(&app, &SystemStateViewState::default(), 900.0, 644.0);
    let (_, card, hero) = drawn(&app);
    let heading = semantic(&compiled, "system-state/headline");
    assert_eq!(
        (heading.role, heading.label.as_str()),
        (SemanticRole::Heading, hero.headline.as_str())
    );
    let phase = semantic(&compiled, "system-state/phase");
    assert_eq!(phase.role, SemanticRole::Status);
    assert_eq!(phase.label, hero.phase);
    let walk = card.first().expect("the walk's page is in the card");
    assert_eq!(
        phase.label, walk.title,
        "the live region says what the card asks"
    );
    assert!(
        !phase.label.chars().any(|c| c.is_ascii_digit()),
        "a count is never announced: {}",
        phase.label
    );
    let progress = semantic(&compiled, "system-state/progress");
    assert_eq!(
        (progress.role, progress.label.as_str()),
        (SemanticRole::ProgressIndicator, "Setup progress")
    );
    #[allow(clippy::cast_precision_loss, reason = "a count of rows")]
    let expected = SemanticValue::Number {
        value: hero.done as f64,
        minimum: 0.0,
        maximum: hero.total as f64,
    };
    assert_eq!(progress.value, expected);
    assert_eq!(
        progress.value_text.as_deref(),
        Some(lines::progress_words(hero.done, hero.total).as_str())
    );
    let group = semantic(&compiled, "system-state/card");
    assert_eq!(
        (group.role, group.label.as_str()),
        (SemanticRole::Group, "Needs you")
    );
    assert_eq!(walk.step.as_deref(), Some("Step 1 of 3"));
    let expected_button = walk.button.as_ref().expect("it opens System Settings");
    let button = semantic(&compiled, &expected_button.key);
    assert_eq!(
        (button.role, button.label.as_str()),
        (SemanticRole::Button, expected_button.label.as_str())
    );
    assert_eq!(
        expected_button.label,
        "Open System Settings for Full Disk Access"
    );
    assert_eq!(
        button.description, None,
        "the reason is heard once, as its own text"
    );
    // The kitty is never announced: nothing but the progress node speaks for the lockup.
    assert!(
        compiled
            .semantics
            .iter()
            .all(|node| !node.label.to_lowercase().contains("kitty")),
        "the kitty is announced"
    );
}

#[test]
fn the_default_join_stays_automatic_after_its_initial_observation_interval() {
    assert_eq!(JOIN_QUIET, Duration::from_secs(30));
    let quiet = reading(&status_of("vault-asking"));
    assert!(quiet.status.joins_quietly());
    let (_, card, hero) = drawn(&quiet);
    assert!(
        card.iter()
            .all(|item| item.subject != Some(Subject::Setting(Setting::Secrets))),
        "no card while the join asks by itself: {card:?}"
    );
    let secrets = line_of(&quiet, Line::Secrets);
    assert_eq!(
        (secrets.mark, secrets.phrase.as_str()),
        (Mark::Working, lines::JOINING_ALAB)
    );
    assert_eq!(hero.headline, lines::SETTING_UP);
    assert!(
        !quiet.needs_attention(),
        "the tab is not lit for a calm wait"
    );
    assert!(crate::system_state_access::signal_line(&quiet.status).is_none());

    let long = reading(&status_of("vault-asking-long"));
    assert!(long.status.joins_quietly());
    let (_, card, hero) = drawn(&long);
    assert!(
        card.iter()
            .all(|item| item.subject != Some(Subject::Setting(Setting::Secrets)))
    );
    assert_eq!(hero.headline, lines::SETTING_UP);
    assert!(!long.needs_attention());
}

#[test]
fn the_tools_password_goes_to_its_tab_from_the_card() {
    let app = reading(&status_of("clt-password"));
    let (_, card, _) = drawn(&app);
    let item = card
        .iter()
        .find(|item| item.subject == Some(Subject::Setting(Setting::Repositories)))
        .expect("the tools' password is in the card");
    assert_eq!(
        item.button.as_ref().map(|button| (
            button.action.as_str(),
            button.label.as_str(),
            button.face.as_str()
        )),
        Some((
            GO_TO_TOOLS_TAB,
            "Go to tab: Command Line Tools",
            "Go to tab"
        ))
    );
}

#[test]
fn apples_installer_window_is_found_from_the_card() {
    let app = reading(&status_of("clt-apple-window"));
    let (_, card, _) = drawn(&app);
    let item = card
        .iter()
        .find(|item| item.subject == Some(Subject::Setting(Setting::Repositories)))
        .expect("Apple's window is in the card");
    assert_eq!(item.title, APPLE_WINDOW_NOTE);
    assert_eq!(
        item.button.as_ref().map(|button| (
            button.action.as_str(),
            button.label.as_str(),
            button.face.as_str()
        )),
        Some((
            GO_TO_TOOLS_INSTALLER,
            "Go to installer: Apple's Command Line Tools",
            "Go to installer"
        ))
    );
}

/// The cat's pose follows the run (§4.5): it walks while a step runs, looks up for the person,
/// puts its ears back for a failure and settles into a loaf once the Mac is ready. The lockup
/// spans the page's inner width, the width the view bakes its images ahead at.
#[test]
fn the_cat_looks_up_for_you_and_loafs_once_the_mac_is_ready() {
    use aterm_effects::rainbow_road::CatPose;
    let pose = |name: &str| {
        let app = reading(&status_of(name));
        let compiled = compile_in(&app, &SystemStateViewState::default(), 900.0, 644.0);
        let node = compiled
            .paint
            .iter()
            .find(|node| node.key.as_str() == "system-state/progress")
            .expect("the lockup is painted");
        let crate::native_ui::UiContent::Custom(custom) = &node.content else {
            panic!("the lockup is a custom node");
        };
        // A regular page's side pad is 24 points, a compact one's 16.
        assert!(
            [24.0, 16.0]
                .map(|pad| 900.0 - 2.0 * pad)
                .contains(&node.rect.width),
            "{name}: the page's inner width, not {}",
            node.rect.width
        );
        LockupPaint::parse(custom.paint.as_deref()).spec.pose
    };
    assert_eq!(pose("needs-you"), CatPose::LookUp);
    assert_eq!(pose("failed"), CatPose::Worried);
    assert_eq!(pose("set-up"), CatPose::Loaf);
    assert_eq!(pose("running"), CatPose::Walk);
}

/// The lockup says `❯ aterm · ALab` on an ALab Mac and `❯ aterm` elsewhere (§4.5).
#[test]
fn the_lockup_names_alab_only_on_an_alab_mac() {
    let alab = |name: &str| {
        let status = status_of(name);
        let plan = status.plan.clone().unwrap_or_default();
        lines::alab_lockup(&status, plan.selection)
    };
    assert!(alab("set-up"));
    assert!(!alab("non-alab"));
    assert!(!alab("non-alab-absent"));

    // Isolate vault evidence from the profile selection's independent ALab signal.
    let mut selection = Selection::none();
    selection.set(Setting::Secrets, true);
    let mut status = status_of("set-up");
    status.plan = Some(Plan::from(selection));
    status
        .observed
        .retain(|row| row.subject == Subject::Setting(Setting::Secrets));
    let vault = status
        .observed
        .first_mut()
        .expect("the fixture reads the vault");
    vault.standing = Standing::InPlace;
    vault.now = Some(Setting::Secrets.wanted().to_owned());
    vault.wanted = Setting::Secrets.wanted().to_owned();
    assert!(lines::alab_lockup(&status, selection));
    for now in [Some(aterm_types::macos_setup::SYNC_OFF), None] {
        status.observed[0].now = now.map(str::to_owned);
        assert!(!lines::alab_lockup(&status, selection), "{now:?}");
    }
}

/// A short window (a split pane, a small screen) keeps the headline, the card's buttons, the
/// sub-line's button and the footer whole in view: the list shrinks to one line and scrolls, and
/// below that the lockup goes, its count still in the sub-line, before any control is cut;
/// shorter still, the footer rides at the list's end, where scrolling reaches it.
#[test]
fn a_short_window_keeps_the_headline_the_card_and_the_footer_in_view() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let controls = [
        "system-state/headline",
        "system-state/stop",
        "system-state/headline/start",
        "system-state/check",
        "system-state/restore",
    ];
    for name in [
        "running",
        "needs-you",
        "failed",
        "set-up",
        "vault",
        "check-failed",
        "walk",
    ] {
        let app = reading(&status_of(name));
        let card: Vec<String> = drawn(&app)
            .1
            .iter()
            .filter_map(|item| item.button.as_ref().map(|button| button.key.clone()))
            .collect();
        for width in [320.0_f32, 720.0, 900.0] {
            for height in (360..=644).step_by(4) {
                let h = height as f32;
                let view = SystemStateViewState::default();
                let compiled = compile_in(&app, &view, width, h);
                for key in controls
                    .iter()
                    .map(|key| (*key).to_owned())
                    .chain(card.clone())
                {
                    if view.spans.borrow().iter().any(|span| *span.key == *key) {
                        assert!(
                            compiled
                                .semantic(&crate::native_ui::UiKey::new(key.clone()))
                                .is_some(),
                            "{name}: {key} is in the list at {width}×{height}"
                        );
                        continue;
                    }
                    let part = format!("{key}/");
                    let nodes: Vec<_> = compiled
                        .paint
                        .iter()
                        .filter(|node| {
                            node.key.as_str() == key || node.key.as_str().starts_with(&part)
                        })
                        .collect();
                    assert!(
                        nodes.iter().all(|node| {
                            !node.clipped()
                                && node.rect.y >= -0.01
                                && node.rect.bottom() <= h + 0.01
                        }),
                        "{name}: {key} cut in a {width}×{height} window"
                    );
                }
            }
        }
    }
}

/// A CARD'S TITLES START WHERE THE LIST'S DO (§4.1): the card's mark and title sit on the same
/// columns as a line's, so the two surfaces read as one page. NEGATIVE CONTROL: the card's
/// title is not at its mark's column, so the test reads titles, not the cards' edges.
#[test]
fn a_cards_titles_line_up_with_the_lists() {
    let app = reading(&status_of("needs-you"));
    let compiled = compile_in(&app, &SystemStateViewState::default(), 900.0, 644.0);
    let left = |pick: &dyn Fn(&str) -> bool| -> Vec<f32> {
        compiled
            .paint
            .iter()
            .filter(|node| pick(node.key.as_str()))
            .map(|node| node.rect.x)
            .collect()
    };
    let card_titles = left(&|key| key.starts_with("system-state/card/") && key.ends_with("/title"));
    let line_titles = left(&|key| key.starts_with("system-state/line/") && key.ends_with("/title"));
    let card_marks = left(&|key| key.starts_with("system-state/card/") && key.ends_with("/mark"));
    let line_marks = left(&|key| key.starts_with("system-state/line/") && key.ends_with("/mark"));
    assert!(!card_titles.is_empty() && !line_titles.is_empty());
    for (titles, marks) in [(&card_titles, &card_marks), (&line_titles, &line_marks)] {
        assert!(titles.iter().chain(marks.iter()).all(|x| x.is_finite()));
    }
    for x in card_titles.iter().chain(&line_titles) {
        assert!(
            (x - line_titles[0]).abs() < 0.01,
            "{card_titles:?} {line_titles:?}"
        );
    }
    for x in card_marks.iter().chain(&line_marks) {
        assert!(
            (x - line_marks[0]).abs() < 0.01,
            "{card_marks:?} {line_marks:?}"
        );
    }
    assert!(card_titles[0] > card_marks[0] + marks::MARK_SLOT);
}

/// §4.3 "Checking": nothing has been read yet, so the page claims no count: the sub-line says
/// no "0 of 16 done" and the progress (the lockup, and VoiceOver's "Setup progress") is unset.
#[test]
fn an_unread_mac_claims_no_count_and_no_progress() {
    let app = reading(&status_of("checking"));
    let hero = drawn(&app).2;
    assert_eq!(hero.headline, CHECKING);
    assert!(
        !hero.sub.contains(" done"),
        "before any reading the sub-line claims a count: {:?}",
        hero.sub
    );
    assert_eq!(
        (hero.done, hero.total),
        (0, 0),
        "no progress before a reading"
    );
    let compiled = compile_in(&app, &SystemStateViewState::default(), 900.0, 644.0);
    let progress = semantic(&compiled, "system-state/progress");
    assert_eq!(progress.value, SemanticValue::None, "no value to hear yet");
}

/// §4.3 "Checking": the headline says it once. An unread line shows its scope, calm, with its
/// ring, and VoiceOver hears "checking", never "to do" for work nobody has found yet.
#[test]
fn checking_is_said_once_and_no_unread_line_is_to_do() {
    let app = reading(&status_of("checking"));
    let (lines, _, hero) = drawn(&app);
    for line in &lines {
        assert_ne!(line.phrase, hero.headline, "{line:?}");
        assert!(!line.label.contains("to do"), "{}", line.label);
    }
    let unread = lines
        .iter()
        .filter(|line| line.word == lines::CHECKING_WORD)
        .count();
    assert!(unread > 0, "an unread line says checking: {lines:#?}");
}

/// §4.8: a vault's approval code is one step from the card: "Approve this Mac from an admin
/// Mac", the code alone in the mono face, and Copy Command, which copies the whole command.
#[test]
fn an_approval_code_item_says_admin_mac_once_and_copies_its_command() {
    let mut app = reading(&status_of("vault-approve"));
    let card = drawn(&app).1;
    let item = card
        .iter()
        .find(|item| item.subject == Some(Subject::Setting(Setting::Secrets)))
        .expect("the vault's code is in the card");
    assert_eq!(item.title, "Approve this Mac from an admin Mac");
    assert_eq!(item.reason.as_deref(), Some("7QK2-M9WD-4HTX-PL3B"));
    assert!(item.code, "the code is set in the mono face");
    let said = format!("{} {}", item.title, item.reason.clone().unwrap_or_default()).to_lowercase();
    assert_eq!(said.matches("admin mac").count(), 1, "{said}");
    let button = item.button.clone().expect("it copies its command");
    assert_eq!(
        (button.action.as_str(), button.label.as_str()),
        (lines::COPY_APPROVE_COMMAND, "Copy Command")
    );
    let mut view = SystemStateViewState::default();
    app.act(&mut view, &button.action, None);
    assert!(
        matches!(
            app.take_action(),
            Some(SystemStateAction::CopyCommand { command, .. })
                if command == "aterm secrets approve 7QK2-M9WD-4HTX-PL3B"
        ),
        "Copy Command copies the whole command"
    );
}

/// §4.1/§4.8: while the window holds the walk's page (its helper panel up), the card keeps
/// "Turn on Full Disk Access · Step 1 of 3" and its button, never a dialog that does not exist.
#[test]
fn the_walks_current_page_keeps_its_step_and_button_while_its_panel_is_up() {
    let mut status = status_of("walk");
    status.access_step = Some(Access::FullDiskAccess);
    let app = reading(&status);
    let card = drawn(&app).1;
    let item = card
        .iter()
        .find(|item| item.subject == Some(Subject::Access(Access::FullDiskAccess)))
        .expect("Full Disk Access is in the card");
    assert_eq!(item.title, "Turn on Full Disk Access");
    assert_eq!(item.step.as_deref(), Some("Step 1 of 3"));
    assert!(
        item.button.is_some(),
        "its button brings System Settings back"
    );
}

/// §4.3 "Waiting (aterm retries itself)": the line says what it waits for, never a past failure
/// under the waiting mark.
#[test]
fn a_waiting_line_says_what_it_waits_for_not_what_failed() {
    let app = reading(&status_of("running"));
    let secrets = line_of(&app, Line::Secrets);
    assert_eq!(
        (secrets.mark, secrets.phrase.as_str()),
        (Mark::Waiting, "Waiting for the vault on this network")
    );
}

/// Revision 4: no question aterm can answer itself. A sync of aterm's own that stopped is
/// aterm's to start again: no Set up now for the person, and the page's next tick runs it.
#[test]
fn aterms_own_stopped_sync_asks_no_press_and_is_mended_by_itself() {
    for name in ["vault-not-syncing", "vault-not-set-up"] {
        let mut app = reading(&status_of(name));
        let hero = drawn(&app).2;
        assert!(!hero.start, "{name}: Set up now for the person to press");
        assert_eq!(hero.headline, lines::SETTING_UP, "{name}");
        app.tick();
        assert!(
            matches!(
                app.take_action(),
                Some(SystemStateAction::Apply(_, Admission::Automatic))
            ),
            "{name}: the page starts the run by itself"
        );
    }
}

/// §4.9: no clipped or ellipsised text. Every fixture, collapsed and with each line open, at the
/// widths a window takes, sets every text node whole and no ink over other ink.
#[test]
fn no_text_is_cut_at_any_real_window_size() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut cut = Vec::new();
    for &name in FIXTURES {
        let mut app = reading(&status_of(name));
        for width in [320.0_f32, 400.0, 480.0, 560.0, 640.0, 720.0, 800.0, 900.0] {
            for height in [420.0_f32, 644.0] {
                let mut views = vec![("collapsed", SystemStateViewState::default())];
                for line in Line::ALL {
                    views.push((line.slug(), open(&mut app, line)));
                }
                for (open, view) in &views {
                    let compiled = compile_in(&app, view, width, height);
                    let what: Vec<String> = compiled
                        .fit_failures()
                        .into_iter()
                        .filter(|line| !line.starts_with("paint-node"))
                        .take(3)
                        .collect();
                    if !what.is_empty() {
                        cut.push(format!("{name} {width}x{height} open={open}: {what:?}"));
                    }
                }
            }
        }
    }
    assert!(
        cut.is_empty(),
        "{} cut frames:\n{}",
        cut.len(),
        cut.join("\n")
    );
}

/// §4.2 and decision 3: the sign-outs' line says what it does ("Sign out of Messages &
/// FaceTime"), and on a Mac outside ALab it is out of scope: its switch is off and not offered,
/// its phrase says aterm leaves them alone, and a flip sends nothing.
#[test]
fn the_sign_outs_line_says_sign_out_and_is_out_of_scope_outside_alab() {
    assert!(Line::Accounts.title().starts_with("Sign out of"));
    let mut app = reading(&status_of("non-alab"));
    let accounts = line_of(&app, Line::Accounts);
    assert!(!accounts.on && !accounts.offered, "{accounts:?}");
    assert_eq!(accounts.phrase, lines::LEAVES_ACCOUNTS);
    let compiled = compile_in(&app, &SystemStateViewState::default(), 900.0, 644.0);
    let switch = semantic(&compiled, "system-state/line/accounts/switch");
    assert_eq!(switch.value, SemanticValue::Bool(false));
    let mut view = SystemStateViewState::default();
    app.act(
        &mut view,
        "system-state/line/accounts/switch",
        Some(SemanticInput::Bool(true)),
    );
    assert!(
        !app.plan.selection.contains(Setting::Messages),
        "a flip outside ALab signs nothing out"
    );
    assert!(app.take_action().is_none(), "and sends nothing");
    // On an ALab Mac the line is offered.
    let alab = reading(&status_of("set-up"));
    assert!(line_of(&alab, Line::Accounts).offered);
}

/// §4.8: the walk is one self-advancing walk: one card item at its current page, with what it
/// reaches next under its button, and the headline counts it once.
#[test]
fn the_walk_is_one_card_item_at_its_current_page() {
    let app = reading(&status_of("walk"));
    let (_, card, hero) = drawn(&app);
    let grants: Vec<&CardItem> = card
        .iter()
        .filter(|item| matches!(item.subject, Some(Subject::Access(_))))
        .collect();
    assert_eq!(grants.len(), 1, "{grants:#?}");
    assert_eq!(hero.headline, "1 thing needs you");
    let page = grants[0];
    assert_eq!(page.step.as_deref(), Some("Step 1 of 3"));
    assert!(
        page.next
            .as_deref()
            .is_some_and(|next| next.starts_with("Next: ")),
        "{:?}",
        page.next
    );
}

/// One progress indicator that agrees with the page: where the lockup and the sub-line say every
/// step is done, no line still waits, needs the person, failed or is to do.
#[test]
fn full_progress_means_every_line_is_done() {
    let mut disagree = Vec::new();
    for &name in FIXTURES {
        let app = reading(&status_of(name));
        let (lines, _, hero) = drawn(&app);
        if hero.total == 0 || hero.done < hero.total {
            continue;
        }
        let open: Vec<String> = lines
            .iter()
            .filter(|line| !matches!(line.mark, Mark::Done | Mark::Off))
            .map(|line| format!("{} {:?} {:?}", line.line.title(), line.mark, line.phrase))
            .collect();
        if !open.is_empty() {
            disagree.push(format!(
                "{name}: {:?} / {:?} with {open:?}",
                hero.headline, hero.sub
            ));
        }
    }
    assert!(disagree.is_empty(), "{}", disagree.join("\n"));
}

/// §4.4 "success only for done": a Mac in no vault syncs no secret and no identity, so its
/// Secrets line is never done: it says it is not joined and why. Where the vault's address did
/// not answer, nothing says there is no vault: the line says aterm asks again by itself (H15).
#[test]
fn a_mac_in_no_vault_never_reads_secrets_done() {
    for (name, phrase, word) in [
        ("non-alab-absent", lines::NO_VAULT_HERE, "not joined"),
        ("non-alab", lines::VAULT_ASKS_AGAIN, "asking"),
    ] {
        let app = reading(&status_of(name));
        let secrets = line_of(&app, Line::Secrets);
        assert_eq!(
            (
                secrets.mark,
                secrets.phrase.as_str(),
                secrets.word,
                secrets.quiet
            ),
            (Mark::Off, phrase, word, true),
            "{name}"
        );
    }
}

/// §4.7: the headline is a Heading and the live region says the phase in words of its own, so a
/// person moving through the hero never hears the headline twice.
#[test]
fn the_live_region_never_repeats_the_heading() {
    let mut twice = Vec::new();
    for &name in FIXTURES {
        let app = reading(&status_of(name));
        let compiled = compile_in(&app, &SystemStateViewState::default(), 900.0, 644.0);
        let heading = semantic(&compiled, "system-state/headline");
        let phase = semantic(&compiled, "system-state/phase");
        if !phase.label.is_empty() && phase.label == heading.label {
            twice.push(format!("{name}: {:?}", heading.label));
        }
    }
    assert!(twice.is_empty(), "{}", twice.join("\n"));
}

/// §4.7: counts are never announced.
#[test]
fn the_live_region_never_carries_a_count() {
    let mut counted = Vec::new();
    for &name in FIXTURES {
        let app = reading(&status_of(name));
        let compiled = compile_in(&app, &SystemStateViewState::default(), 900.0, 644.0);
        let phase = semantic(&compiled, "system-state/phase");
        if phase.label.chars().any(|c| c.is_ascii_digit()) {
            counted.push(format!("{name}: {:?}", phase.label));
        }
    }
    assert!(counted.is_empty(), "{}", counted.join("\n"));
}

/// §4.7: a card item's reason is heard once: as its own text, not again on its button.
#[test]
fn a_card_reason_is_heard_once() {
    let mut twice = Vec::new();
    for name in [
        "walk",
        "needs-you",
        "failed",
        "github-account",
        "clt-recovered",
        "vault-approve",
    ] {
        let app = reading(&status_of(name));
        let compiled = compile_in(&app, &SystemStateViewState::default(), 900.0, 644.0);
        for item in drawn(&app).1 {
            let Some(reason) = item.reason.as_deref() else {
                continue;
            };
            let heard = compiled
                .semantics
                .iter()
                .filter(|node| {
                    node.label.trim() == reason.trim()
                        || node.description.as_deref().map(str::trim) == Some(reason.trim())
                })
                .count();
            if heard != 1 {
                twice.push(format!("{name}/{}: {heard}× {reason:?}", item.id));
            }
        }
    }
    assert!(twice.is_empty(), "{}", twice.join("\n"));
}

/// §2.2: "Off" appears only once the answer is definitive. While the plan in force, or whether
/// a line applies here, is still read, its switch has no value to say, so a pending answer never
/// reads as the person's choice.
#[test]
fn a_pending_line_never_reads_as_off() {
    let mut scope = status_of("non-alab");
    scope.provisional = true;
    if let Some(vault) = scope
        .observed
        .iter_mut()
        .find(|row| row.subject == Subject::Setting(Setting::Secrets))
    {
        vault.standing = Standing::Waiting;
    }
    assert!(lines::scope_pending(&scope));
    let mut checked = 0;
    for (name, status) in [
        ("scope pending", scope),
        ("plan-unread", status_of("plan-unread")),
    ] {
        let app = reading(&status);
        let compiled = compile_in(&app, &SystemStateViewState::default(), 900.0, 644.0);
        for line in drawn(&app).0 {
            if line.word != lines::CHECKING_WORD {
                continue;
            }
            checked += 1;
            let switch = compiled.semantic(&UiKey::new(format!(
                "system-state/line/{}/switch",
                line.line.slug()
            )));
            assert!(
                switch.is_none_or(|switch| switch.value != SemanticValue::Bool(false)),
                "{name}: {:?} reads off while it is checked",
                line.line
            );
        }
    }
    assert!(checked > 0, "a pending line was drawn");
}

/// Keyboard: Return in the vault's password field joins, as Join does and as macOS's own forms
/// do: in the card's "Join your vault" and in the Secrets line's "Join a different vault…".
#[test]
fn return_in_the_vault_password_joins_as_join_does() {
    let mut card = status_of("set-up");
    let vault = card
        .observed
        .iter_mut()
        .find(|row| row.subject == Subject::Setting(Setting::Secrets))
        .expect("a vault reading");
    vault.standing = Standing::NeedsYou;
    vault.note = None;
    assert!(
        drawn(&reading(&card)).1.iter().any(|item| item.vault),
        "the card holds the vault's fields"
    );
    for (name, status) in [("card", card), ("another vault", status_of("set-up"))] {
        let mut pressed = reading(&status);
        let mut view = SystemStateViewState::default();
        pressed.act(&mut view, "system-state/password", None);
        pressed.act(&mut view, "system-state/vault/join", None);
        assert!(
            matches!(pressed.take_action(), Some(SystemStateAction::Apply(..))),
            "{name}: Join applies"
        );
        let mut app = reading(&status);
        let mut view = SystemStateViewState::default();
        app.act(&mut view, "system-state/password", None);
        assert!(
            app.takes_words(&view),
            "{name}: the password field takes words"
        );
        for c in ["s", "e", "c"] {
            app.type_into(&mut view, TextInputEvent::Commit(c.to_owned()));
        }
        app.type_into(&mut view, TextInputEvent::Submit);
        assert!(
            matches!(app.take_action(), Some(SystemStateAction::Apply(..))),
            "{name}: Return in the password field joins"
        );
    }
}

/// VoiceOver reading order: the headline is the page's H1 and the first thing a person reading
/// from the top hears; the lockup's progress follows it.
#[test]
fn the_headline_is_heard_before_the_progress() {
    let app = reading(&status_of("running"));
    let compiled = compile_in(&app, &SystemStateViewState::default(), 900.0, 644.0);
    let at = |key: &str| {
        compiled
            .semantics
            .iter()
            .position(|node| node.key.as_str() == key)
            .unwrap_or_else(|| panic!("{key} is in the tree"))
    };
    assert!(at("system-state/headline") < at("system-state/progress"));
}

/// §4.7: the progress indicator says "11 of 16 done", and nothing while nothing is managed:
/// never "0 of 0 done".
#[test]
fn progress_never_says_zero_of_zero() {
    let mut zero = Vec::new();
    for &name in FIXTURES {
        let app = reading(&status_of(name));
        let compiled = compile_in(&app, &SystemStateViewState::default(), 900.0, 644.0);
        if semantic(&compiled, "system-state/progress")
            .value_text
            .as_deref()
            == Some("0 of 0 done")
        {
            zero.push(name);
        }
    }
    assert!(zero.is_empty(), "{zero:?}");
}

/// A line's name is said once, by its switch: its group and its disclosure do not repeat it.
#[test]
fn a_lines_name_is_said_once() {
    let app = reading(&status_of("running"));
    let compiled = compile_in(&app, &SystemStateViewState::default(), 900.0, 644.0);
    for line in Line::ALL {
        let said: Vec<String> = compiled
            .semantics
            .iter()
            .filter(|node| {
                node.key
                    .as_str()
                    .starts_with(&format!("system-state/line/{}", line.slug()))
            })
            .filter(|node| node.label.starts_with(line.title()))
            .map(|node| format!("{:?} {:?}", node.role, node.label))
            .collect();
        assert!(
            said.len() <= 1,
            "{} is named {} times: {said:?}",
            line.title(),
            said.len()
        );
    }
}

/// §4.9: the state is said once: a line whose phrase already says its state ("Full Disk Access
/// needs you") is not given the state word in front again.
#[test]
fn a_line_label_says_its_state_once() {
    let mut twice = Vec::new();
    for &name in FIXTURES {
        let app = reading(&status_of(name));
        for line in drawn(&app).0 {
            if line.label.to_lowercase().matches(line.word).count() > 1 {
                twice.push(format!("{name}: {:?}", line.label));
            }
        }
    }
    assert!(twice.is_empty(), "{}", twice.join("\n"));
}

/// §4.7: where one thing needs the person, the live region says what the card asks.
#[test]
fn the_live_region_names_what_the_card_asks() {
    let mut wrong = Vec::new();
    for &name in FIXTURES {
        let app = reading(&status_of(name));
        let (_, card, hero) = drawn(&app);
        let needs: Vec<&CardItem> = card.iter().filter(|i| i.mark == Mark::NeedsYou).collect();
        if let [item] = needs.as_slice()
            && hero.phase != item.title
        {
            wrong.push(format!(
                "{name}: live {:?} vs card {:?}",
                hero.phase, item.title
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// §4.3/§4.8: "needs you" is said only with the card holding the thing (the Command Line Tools
/// an administrator installs among them).
#[test]
fn a_needs_you_headline_has_its_card_item() {
    let mut bare = Vec::new();
    for &name in FIXTURES {
        let app = reading(&status_of(name));
        let (_, card, hero) = drawn(&app);
        let says = hero.headline.to_lowercase().contains("needs you")
            || hero.headline.to_lowercase().contains("need you");
        if says && !card.iter().any(|item| item.mark == Mark::NeedsYou) {
            bare.push(format!("{name}: {:?}", hero.headline));
        }
    }
    assert!(bare.is_empty(), "{}", bare.join("\n"));
}

/// The lockup node's rect and paint in `compiled`.
fn lockup_of(compiled: &CompiledUi) -> (LogicalRect, LockupPaint) {
    let node = compiled
        .paint
        .iter()
        .find(|node| node.key.as_str() == "system-state/progress")
        .expect("the lockup is drawn");
    let crate::native_ui::UiContent::Custom(custom) = &node.content else {
        panic!("the lockup is a custom node");
    };
    (node.rect, LockupPaint::parse(custom.paint.as_deref()))
}

/// What fixes the page's frame in a window: its side pad, the lockup's rect (none where the
/// window is too short for it) and the headline's type step.
fn measures(compiled: &CompiledUi) -> (f32, Option<LogicalRect>, crate::native_ui::StyleRef) {
    let hero = compiled
        .paint
        .iter()
        .find(|node| node.key.as_str() == "system-state/hero")
        .expect("the hero")
        .rect;
    let lockup = compiled
        .paint
        .iter()
        .find(|node| node.key.as_str() == "system-state/progress")
        .map(|node| node.rect);
    let style = compiled
        .paint
        .iter()
        .find_map(|node| match &node.content {
            crate::native_ui::UiContent::Text(text)
                if node.key.as_str() == "system-state/headline"
                    || node.key.as_str().starts_with("system-state/headline/line-") =>
            {
                Some(text.style)
            }
            _ => None,
        })
        .expect("the headline");
    (hero.x, lockup, style)
}

/// THE PAGE NEVER REFLOWS DURING A RUN (§4.1): its measures are the window's alone, so in each
/// window, the design's 900 × 644, narrow panes, and mid widths whose lines' phrases wrap in some
/// moments and not others, every moment a run passes through (every fixture, with the line its
/// host opens: running, a line opened, a note, a card that comes, the footer once it stops, the
/// end) keeps the same side pad, the same lockup rect and the same headline step. A lockup that
/// shows keeps its words legible, at Caption 11 or more, even where the list scrolls. NEGATIVE
/// CONTROL: a larger window sets the lockup larger, so the rect read is the window's.
#[test]
fn a_windows_measures_hold_through_every_moment_of_a_run() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut lockups = Vec::new();
    let mut windows = vec![
        (900.0, 644.0),
        (346.0, 642.0),
        (900.0, 520.0),
        (900.0, 800.0),
        (1200.0, 800.0),
        (346.0, 900.0),
        (900.0, 420.0),
        (640.0, 768.0),
        (346.0, 750.0),
    ];
    for w in [480.0, 560.0, 640.0, 720.0, 800.0] {
        windows.extend([(w, 700.0), (w, 756.0)]);
    }
    for (w, h) in windows {
        // A run's moments share its plan: the fixtures of one selection and one set of lines
        // are one run's moments (a plan with every group left as is runs nothing).
        let mut runs: Vec<(Vec<String>, &str, _)> = Vec::new();
        for &name in FIXTURES {
            let app = reading(&status_of(name));
            let view = SystemStateViewState::with_line_open(fixture_line(name));
            let compiled = compile_in(&app, &view, w, h);
            if let Some(px) = compiled.paint.iter().find_map(|node| {
                (node.key.as_str() == "system-state/progress")
                    .then(|| lockup_of(&compiled).1.spec.px)
            }) {
                assert!(px >= 11.0, "{w}×{h}: {name} set the lockup at Mono {px}");
            }
            let mut lines: Vec<String> = compiled
                .paint
                .iter()
                .map(|node| node.key.as_str().to_owned())
                .filter(|key| {
                    key.strip_prefix("system-state/line/")
                        .is_some_and(|slug| !slug.contains('/'))
                })
                .collect();
            lines.sort();
            lines.dedup();
            lines.push(format!("{:?}", app.plan.selection));
            let now = measures(&compiled);
            match runs.iter().find(|(run, _, _)| *run == lines) {
                None => runs.push((lines, name, now)),
                Some((_, was, then)) => assert_eq!(
                    &now, then,
                    "{w}×{h}: {name} reflowed the page {was} drew (pad, lockup, headline step)"
                ),
            }
        }
        assert!(
            runs.iter().any(|(lines, _, _)| lines.len() > 2),
            "the fixtures hold runs of several lines"
        );
        lockups.push(runs[0].2.1);
    }
    let height = |at: usize| lockups[at].map_or(0.0, |rect| rect.height);
    assert!(height(4) > height(0), "1200 × 800 sets a larger lockup");
}

/// A TALLER WINDOW NEVER SETS A SMALLER LOCKUP (§4.1): down each width, from a window too short
/// for any lockup to one past the design's, each step taller sets the lockup at least as large
/// (and a lockup that shows is legible, Caption 11 or more), so the fallback for a window too
/// short for the fullest moment never outgrows the size the budget sets just above it.
/// NEGATIVE CONTROL: the lockup grows down each sweep, so the sizes read are the window's.
#[test]
fn a_taller_window_never_sets_a_smaller_lockup() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let app = reading(&status_of("running"));
    for w in [346.0, 640.0, 900.0] {
        let mut sizes = Vec::new();
        let mut h: f32 = 380.0;
        while h <= 960.0 {
            let compiled = compile_in(&app, &SystemStateViewState::default(), w, h);
            let px = compiled
                .paint
                .iter()
                .any(|node| node.key.as_str() == "system-state/progress")
                .then(|| lockup_of(&compiled).1.spec.px);
            if let Some(px) = px {
                assert!(px >= 11.0, "{w}×{h}: Mono {px}");
            }
            sizes.push((h, px.unwrap_or(0.0)));
            h += 4.0;
        }
        for pair in sizes.windows(2) {
            assert!(
                pair[1].1 >= pair[0].1,
                "{w} wide: {} tall sets Mono {}, {} tall Mono {}",
                pair[0].0,
                pair[0].1,
                pair[1].0,
                pair[1].1
            );
        }
        assert!(
            sizes[sizes.len() - 1].1 > sizes[0].1,
            "{w} wide: the lockup grows"
        );
    }
}

/// A PAGE THAT COMES BACK DRAWS THE MOMENT IT LEFT (§4.5): once no page is on screen (its tab
/// behind another, its window minimised or covered) each view keeps the moment it last drew, so
/// the first pass after it shows again draws that moment, not the words alone while a bake
/// runs. NEGATIVE CONTROL: with every bake released, the same pass draws the words alone.
#[test]
fn a_page_shown_again_draws_the_moment_it_left_at_once() {
    let app = reading(&status_of("running"));
    let view = SystemStateViewState::default();
    let (_, shown) = lockup_of(&compile_in(&app, &view, 900.0, 644.0));
    let moment = shown.images.expect("its moment, baked");
    assert_eq!(moment.spec, shown.spec);
    super::prebake::hold(true);
    super::prebake::set_shown(false);
    let (_, hidden) = lockup_of(&compile_in(&app, &view, 900.0, 644.0));
    assert_eq!(hidden.images, Some(moment), "hidden, it keeps what it drew");
    super::prebake::set_shown(true);
    let (_, back) = lockup_of(&compile_in(&app, &view, 900.0, 644.0));
    assert_eq!(back.images, Some(moment), "back, its first pass draws it");
    assert!(!back.pending);
    super::prebake::release();
    let (_, released) = lockup_of(&compile_in(&app, &view, 900.0, 644.0));
    assert_eq!(released.images, None, "control: released, the words alone");
    assert!(released.pending);
    super::prebake::hold(false);
}

/// A VIEW WHOSE LOCKUP DRAWS NO ROAD KEEPS NO IMAGES (§4.5): a view that drew the road and then
/// draws the words alone (here a pane too narrow for the road; Serious Mode, High Contrast and a
/// window too short for the lockup take the same path) keeps none, and showing the page again
/// bakes none for it. NEGATIVE CONTROL: at the wide pane the road is kept.
#[test]
fn a_view_whose_lockup_draws_no_road_keeps_no_images() {
    let app = reading(&status_of("running"));
    let view = SystemStateViewState::default();
    let (_, wide) = lockup_of(&compile_in(&app, &view, 900.0, 644.0));
    assert!(wide.images.is_some());
    assert!(
        crate::widget::images_kept_bytes() > 0,
        "control: the road kept"
    );
    let (_, narrow) = lockup_of(&compile_in(&app, &view, 200.0, 644.0));
    assert!(
        !narrow.spec.road,
        "too narrow for the road: {:?}",
        narrow.spec
    );
    assert_eq!(crate::widget::images_kept_bytes(), 0, "nothing kept");
    super::prebake::set_shown(false);
    super::prebake::set_shown(true);
    super::prebake::settle();
    assert_eq!(crate::widget::images_kept_bytes(), 0, "nothing baked again");
}

/// THE LOCKUP ASKS ONLY FOR WHAT IT DRAWS (§4.5): a compact page measures itself at regular
/// metrics first, and measuring asks for no image; the one request is the compact lockup's own
/// images. A new moment then keeps painting the moment last drawn while its bake is pending,
/// and paints its own once it lands. NEGATIVE CONTROL: the compact lockup is smaller than the
/// regular one, so a regular request would differ.
#[test]
fn a_compact_page_asks_only_for_its_own_images_and_holds_the_last_moment_meanwhile() {
    super::prebake::take_requests();
    let mut app = reading(&status_of("running"));
    let view = SystemStateViewState::default();
    let compiled = compile_in(&app, &view, 900.0, 520.0);
    let (rect, paint) = lockup_of(&compiled);
    // A compact page's side pad is 16 points, a regular one's 24 (a narrow pane's 12).
    let (pad, _, _) = measures(&compiled);
    assert_eq!(pad, 16.0, "a compact page: {:?}", paint.spec);
    let requests = super::prebake::take_requests();
    assert_eq!(
        requests,
        vec![paint.spec.wanted(rect.width).keys().to_vec()],
        "one request, the compact lockup's"
    );
    let regular = LockupSpec {
        px: 22.0,
        ..paint.spec
    };
    assert!(!requests[0].contains(&regular.wanted(rect.width).keys()[0]));
    let walking = paint.images.expect("its own images, baked");
    assert_eq!(walking.spec, paint.spec);
    // The run comes to need the person while bakes are held: the moment drawn last stays.
    super::prebake::hold(true);
    app.publish(status_of("needs-you"));
    let (_, waiting) = lockup_of(&compile_in(&app, &view, 900.0, 520.0));
    assert_eq!(
        waiting.spec.pose,
        aterm_effects::rainbow_road::CatPose::LookUp
    );
    assert_eq!(waiting.images, Some(walking), "the last moment stays drawn");
    assert!(waiting.pending, "and the moment's own images are pending");
    // Its bake lands: the new moment draws.
    super::prebake::settle();
    let (rect, landed) = lockup_of(&compile_in(&app, &view, 900.0, 520.0));
    assert_eq!(landed.images, Some(landed.spec.wanted(rect.width)));
    assert!(!landed.pending);
    super::prebake::hold(false);
}

#[test]
fn independent_password_steps_have_visible_actions_without_missing_tools() {
    let mut status = status_of("set-up");
    for access in [
        Access::NameRule,
        Access::SpotlightScope,
        Access::NetworkHelper,
    ] {
        let mut row = Observation::unknown(Subject::Access(access));
        row.standing = Standing::NeedsYou;
        row.now = Some("Incomplete".into());
        row.note = Some("Your Mac password is required".into());
        status.observed.retain(|seen| seen.subject != row.subject);
        status.observed.push(row);
    }
    let app = reading(&status);
    let (_, card, _) = drawn(&app);
    let item = card
        .iter()
        .find(|item| item.id == "mac-password")
        .expect("one shared password episode");
    assert!(item.title.contains("Mac password"));
    let reason = item.reason.as_deref().unwrap();
    for access in [
        Access::NameRule,
        Access::SpotlightScope,
        Access::NetworkHelper,
    ] {
        assert!(reason.contains(access.name()), "{reason}");
    }
    assert!(!reason.contains("Your Mac password is required"));
    assert_eq!(item.button.as_ref().unwrap().face, "Finish setup…");
    status.mac_setup = Some(ToolsInstall::Password);
    let app = reading(&status);
    assert!(app.headline().go_to_tab);
    let (_, card, _) = drawn(&app);
    assert_eq!(
        card.iter()
            .find(|item| item.id == "mac-password")
            .unwrap()
            .button
            .as_ref()
            .unwrap()
            .action,
        GO_TO_TOOLS_TAB
    );
    compile_in(&app, &SystemStateViewState::default(), 900.0, 644.0);
}

#[test]
fn a_check_keeps_the_last_card_seated_without_keeping_its_authority() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let spotlight = Subject::Access(Access::SpotlightScope);
    let mut status = status_of("set-up");
    status.mac_setup = Some(ToolsInstall::Password);
    let row = status
        .observed
        .iter_mut()
        .find(|row| row.subject == spotlight)
        .unwrap();
    row.standing = Standing::NeedsYou;
    row.now = Some(aterm_types::macos_setup::SPOTLIGHT_OWED.into());
    row.note = Some("Your Mac password excludes the new home folders from Spotlight.".into());
    let mut app = reading(&status);
    let mut view = SystemStateViewState::default();
    let before = compile_in(&app, &view, 900.0, 824.0);
    let compact_before = compile_in(&app, &view, 346.0, 824.0);
    let card = drawn(&app).1;
    assert_eq!(card.len(), 1);
    let button = card[0].button.as_ref().unwrap().clone();
    assert!(
        semantic(&before, &button.key)
            .state
            .as_ref()
            .unwrap()
            .enabled
    );
    let developer = format!("system-state/line/{}/switch", Line::Developer.slug());

    app.check_again(crate::CheckBy::Press);
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::Check(..))
    ));
    let mut pending = status.clone();
    pending.busy = true;
    pending.operation = Some(SetupOperation::Check);
    pending.last_known = std::mem::take(&mut pending.observed);
    pending.observing = pending
        .last_known
        .iter()
        .map(|row| {
            (
                row.subject,
                row.subject
                    .uses_vault_source()
                    .then(|| app.plan.vault_url.clone()),
                true,
            )
        })
        .collect();
    for queued in [true, false] {
        for (_, _, waiting) in &mut pending.observing {
            *waiting = queued;
        }
        app.publish(pending.clone());
        let during = compile_in(&app, &view, 900.0, 824.0);
        for key in [
            "system-state/card",
            button.key.as_str(),
            developer.as_str(),
            "system-state/progress",
            "system-state/headline",
        ] {
            assert_eq!(
                semantic(&during, key).rect,
                semantic(&before, key).rect,
                "{key}"
            );
        }
        let compact = compile_in(&app, &view, 346.0, 824.0);
        for key in [
            "system-state/card",
            developer.as_str(),
            "system-state/headline",
        ] {
            assert_eq!(
                semantic(&compact, key).rect,
                semantic(&compact_before, key).rect,
                "compact {key}"
            );
        }
        assert!(
            semantic(&during, "system-state/card")
                .label
                .contains("last read")
        );
        assert!(
            semantic(&during, "system-state/phase")
                .label
                .contains(if queued { "queued" } else { "checking" })
        );
        assert!(
            !semantic(&during, &button.key)
                .state
                .as_ref()
                .unwrap()
                .enabled
        );
        assert!(
            !during
                .focus_order
                .contains(&UiKey::new(button.key.as_str()))
        );
        assert!(
            during
                .default_action
                .as_ref()
                .is_none_or(|(key, _)| key.as_str() != button.key)
        );
        assert!(!app.card_buttons().contains(&button.key));
        assert_eq!(
            app.reviewed_row(spotlight).standing,
            Some(Standing::Unknown)
        );
        app.act(&mut view, &button.action, None);
        assert!(
            app.take_action().is_none(),
            "a retained control has no dispatch authority"
        );
    }

    let mut changed_need = pending.clone();
    let mut latest = status
        .observed
        .iter()
        .find(|row| row.subject == spotlight)
        .unwrap()
        .clone();
    latest.note = Some("A newly observed folder needs exclusion".into());
    changed_need.observed = vec![latest];
    changed_need
        .observing
        .retain(|(subject, _, _)| *subject != spotlight);
    app.publish(changed_need);
    assert!(
        app.check_card.is_none(),
        "a new current requirement replaces its old presentation"
    );
    assert!(drawn(&app).1.iter().any(|item| {
        !item.retained
            && item
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("newly observed"))
    }));
    app.publish(status.clone());
    app.check_again(crate::CheckBy::Press);
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::Check(..))
    ));
    app.publish(pending.clone());

    let mut failed_stage = pending.clone();
    failed_stage.rows = vec![SetupStatusRow {
        name: Setting::Repositories.name().into(),
        status: "error".into(),
        detail: "The repository stage failed".into(),
    }];
    app.publish(failed_stage);
    assert!(
        app.check_card.is_none(),
        "a current failed card supersedes history even without a failed reading"
    );
    let cards = drawn(&app).1;
    assert!(cards.iter().any(|item| item.mark == Mark::Failed));
    assert!(cards.iter().all(|item| !item.retained));
    app.publish(status.clone());
    app.check_again(crate::CheckBy::Press);
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::Check(..))
    ));
    app.publish(pending.clone());

    let mut failure = pending.clone();
    let mut failed = status
        .observed
        .iter()
        .find(|row| row.subject == spotlight)
        .unwrap()
        .clone();
    failed.standing = Standing::Failed;
    failed.note = Some("The new read failed".into());
    failure.observed = vec![failed];
    app.publish(failure);
    assert!(
        app.check_card.is_none(),
        "a current failure supersedes the retained card"
    );
    assert_eq!(app.reviewed_row(spotlight).standing, Some(Standing::Failed));
    assert!(drawn(&app).1.iter().all(|item| !item.retained));

    app.publish(status.clone());
    let settled = compile_in(&app, &view, 900.0, 824.0);
    for key in ["system-state/card", button.key.as_str(), developer.as_str()] {
        assert_eq!(
            semantic(&settled, key).rect,
            semantic(&before, key).rect,
            "{key}"
        );
    }
    assert!(
        semantic(&settled, &button.key)
            .state
            .as_ref()
            .unwrap()
            .enabled
    );
    app.act(&mut view, &button.action, None);
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::GoToToolsTab)
    ));

    app.check_again(crate::CheckBy::Press);
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::Check(..))
    ));
    let mut other_source = pending.clone();
    other_source.observed_vault_url = Some("https://other.invalid/vault".into());
    app.publish(other_source);
    assert!(
        app.check_card.is_none(),
        "another source cannot inherit the card"
    );
    app.publish(status);
    app.check_again(crate::CheckBy::Press);
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::Check(..))
    ));
    pending.stopped_wait = true;
    app.publish(pending);
    assert!(
        app.check_card.is_none(),
        "a current hold clears the retained card"
    );
}

#[test]
fn an_ordinary_partial_read_keeps_its_password_card_without_a_check_action() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let password_steps = [
        Access::NameRule,
        Access::SpotlightScope,
        Access::NetworkHelper,
    ];
    let helper = Subject::Access(Access::NetworkHelper);
    let spotlight = Subject::Access(Access::SpotlightScope);
    let repositories = Subject::Setting(Setting::Repositories);
    let mut prior = status_of("set-up");
    prior.mac_setup = Some(ToolsInstall::Password);
    for access in password_steps {
        let row = prior
            .observed
            .iter_mut()
            .find(|row| row.subject == Subject::Access(access))
            .unwrap();
        row.standing = Standing::NeedsYou;
        row.note = Some(format!("{} is incomplete", access.name()));
    }
    let developer = format!("system-state/line/{}/switch", Line::Developer.slug());
    let mut queued = prior.clone();
    queued.observing = password_steps
        .map(|access| (Subject::Access(access), None, true))
        .to_vec();
    queued
        .observing
        .push((repositories, prior.observed_vault_url.clone(), true));
    for coalesced in [false, true] {
        let mut app = reading(&prior);
        let view = SystemStateViewState::default();
        let before = compile_in(&app, &view, 900.0, 824.0);
        let compact_before = compile_in(&app, &view, 346.0, 824.0);
        let old = drawn(&app).1;
        let button = old[0].button.as_ref().unwrap();

        let mut unrelated = prior.clone();
        unrelated.observing = vec![(repositories, prior.observed_vault_url.clone(), false)];
        app.publish(unrelated);
        assert!(
            app.check_card.is_none(),
            "a repository-only read is unrelated"
        );
        assert!(drawn(&app).1.iter().all(|item| !item.retained));
        assert!(
            semantic(&compile_in(&app, &view, 900.0, 824.0), &button.key)
                .state
                .unwrap()
                .enabled
        );
        // The unrelated read is still active when focus broadens the observation.
        if !coalesced {
            app.publish(queued.clone());
            assert!(drawn(&app).1.iter().all(|item| item.retained));
            assert!(
                semantic(&compile_in(&app, &view, 900.0, 824.0), "system-state/phase")
                    .label
                    .contains("queued")
            );
        }

        // The early Access batch is already over when the GUI next receives status. One,
        // then two members are unread while the repository batch continues: the surviving
        // requests regroup from three to two to one without becoming different requests.
        let mut early = queued.clone();
        early.observing = vec![(repositories, prior.observed_vault_url.clone(), false)];
        early.last_known = prior.observed.clone();
        for unread in [helper, spotlight] {
            early.observed.retain(|row| row.subject != unread);
            early.observed.push(Observation::unknown(unread));
            early.unconfirmed_observations.push(unread);
            app.publish(early.clone());
            assert!(!app.status.busy && app.status.operation.is_none());
            for (width, expected) in [(900.0, &before), (346.0, &compact_before)] {
                let during = compile_in(&app, &view, width, 824.0);
                for key in [
                    "system-state/card",
                    button.key.as_str(),
                    developer.as_str(),
                    "system-state/headline",
                    "system-state/progress",
                ] {
                    assert_eq!(
                        semantic(&during, key).rect,
                        semantic(expected, key).rect,
                        "{width} {key}; coalesced={coalesced}"
                    );
                }
                assert_eq!(
                    semantic(&during, "system-state/headline").label,
                    semantic(expected, "system-state/headline").label
                );
                assert!(
                    semantic(&during, "system-state/card")
                        .label
                        .contains("last read")
                );
                assert!(
                    semantic(&during, "system-state/phase")
                        .label
                        .contains("checking")
                );
                assert!(!semantic(&during, &button.key).state.unwrap().enabled);
                assert!(
                    !during
                        .focus_order
                        .contains(&UiKey::new(button.key.as_str()))
                );
                assert!(
                    during
                        .default_action
                        .as_ref()
                        .is_none_or(|(key, _)| { key.as_str() != button.key })
                );
            }
            assert!(drawn(&app).1.iter().all(|item| item.retained));
            assert_eq!(app.reviewed_row(unread).standing, Some(Standing::Unknown));
            assert!(!app.reviewed_row(unread).settled());
            assert!(app.card_buttons().is_empty());
            assert!(
                app.take_action().is_none(),
                "no Check or setup was submitted"
            );
        }

        app.publish(prior.clone());
        assert!(
            app.check_card.is_none(),
            "completion retires presentation history"
        );
        let settled = compile_in(&app, &view, 900.0, 824.0);
        assert_eq!(
            semantic(&settled, "system-state/card").rect,
            semantic(&before, "system-state/card").rect
        );
        assert!(semantic(&settled, &button.key).state.unwrap().enabled);
        assert!(app.card_buttons().contains(&button.key));
        assert!(app.take_action().is_none());

        // A fresh requirement, failure, source, plan, hold or newer record still wins. None
        // may reseed a reduced card during the remainder of this same observation episode.
        let mut changed_need = early.clone();
        changed_need
            .observed
            .iter_mut()
            .find(|row| row.subject == Subject::Access(Access::NameRule))
            .unwrap()
            .note = Some("A different current naming requirement".into());
        let mut failed = early.clone();
        let row = failed
            .observed
            .iter_mut()
            .find(|row| row.subject == helper)
            .unwrap();
        row.standing = Standing::Failed;
        row.note = Some("The new helper reading failed".into());
        failed
            .unconfirmed_observations
            .retain(|subject| *subject != helper);
        let mut other_source = early.clone();
        other_source.observed_vault_url = Some("https://other.invalid/vault".into());
        let mut held = early.clone();
        held.stopped_wait = true;
        let mut other_plan = early.clone();
        other_plan
            .plan
            .as_mut()
            .unwrap()
            .selection
            .set(Setting::Permissions, false);
        let mut newer = status_of("newer");
        newer.observing = early.observing.clone();
        for invalidated in [changed_need, failed, other_source, held, other_plan, newer] {
            let mut guarded = reading(&prior);
            guarded.publish(early.clone());
            assert!(guarded.check_card.is_some());
            guarded.publish(invalidated.clone());
            assert!(guarded.check_card.is_none());
            assert!(drawn(&guarded).1.iter().all(|item| !item.retained));
            guarded.publish(invalidated);
            assert!(
                guarded.check_card.is_none(),
                "the same read cannot recapture"
            );
            assert!(guarded.take_action().is_none());
        }
    }

    // A view can first receive the still-current requests after their read has started. It
    // has no prior card to capture on that publication; the later unread edge must preserve
    // the card actually shown, without needing a Check action or a new reading episode.
    let mut first_seen = reading(&queued);
    assert!(first_seen.check_card.is_none());
    let view = SystemStateViewState::default();
    let before = compile_in(&first_seen, &view, 346.0, 824.0);
    let mut partial = queued;
    partial.last_known = prior.observed;
    partial.observed.retain(|row| row.subject != helper);
    partial.observed.push(Observation::unknown(helper));
    partial.unconfirmed_observations = vec![helper];
    partial.observing = vec![(repositories, partial.observed_vault_url.clone(), false)];
    first_seen.publish(partial);
    let during = compile_in(&first_seen, &view, 346.0, 824.0);
    for key in [
        "system-state/card",
        developer.as_str(),
        "system-state/headline",
        "system-state/progress",
    ] {
        assert_eq!(
            semantic(&before, key).rect,
            semantic(&during, key).rect,
            "first reading {key}"
        );
    }
    assert!(drawn(&first_seen).1.iter().all(|item| item.retained));
    assert_eq!(
        first_seen.reviewed_row(helper).standing,
        Some(Standing::Unknown)
    );
    assert!(first_seen.card_buttons().is_empty());
    assert!(first_seen.take_action().is_none());
}

#[test]
fn optional_app_network_settings_require_a_current_diagnostic() {
    let subject = Subject::Access(Access::LocalNetwork);
    let status = status_of("set-up");
    let mut app = reading(&status);
    let row = app.reviewed_row(subject);
    assert!(!row.managed);
    assert_eq!(row.standing, Some(Standing::NotNeeded));
    assert!(app.can_open_access(Access::LocalNetwork));
    let mut pending = status.clone();
    pending.last_known = std::mem::take(&mut pending.observed);
    pending.observing = vec![(subject, None, true)];
    app.publish(pending);
    assert!(!app.can_open_access(Access::LocalNetwork));
    app.publish(status);
    assert!(app.can_open_access(Access::LocalNetwork));
}

#[test]
fn an_observed_failure_keeps_its_card_until_the_new_read_supplies_a_current_result() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let dock = Subject::Setting(Setting::Dock);
    let mut prior = status_of("set-up");
    let failed = prior
        .observed
        .iter_mut()
        .find(|row| row.subject == dock)
        .unwrap();
    failed.standing = Standing::Failed;
    failed.note = Some("The Dock reading failed".into());
    let original_failure = failed.clone();
    assert!(
        prior.rows.is_empty() && prior.check.is_none(),
        "only the observation supplies this failure"
    );
    let mut app = reading(&prior);
    let mut view = SystemStateViewState::default();
    let before = compile_in(&app, &view, 900.0, 824.0);
    let compact_before = compile_in(&app, &view, 346.0, 824.0);
    assert!(drawn(&app).1.iter().any(|item| item.mark == Mark::Failed));
    app.check_again(crate::CheckBy::Press);
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::Check(..))
    ));

    let mut pending = prior.clone();
    pending.busy = true;
    pending.operation = Some(SetupOperation::Check);
    app.publish(pending.clone());
    assert!(
        drawn(&app)
            .1
            .iter()
            .any(|item| item.mark == Mark::Failed && !item.retained)
    );
    pending.last_known = pending
        .observed
        .iter()
        .filter(|row| row.standing != Standing::Failed)
        .cloned()
        .collect();
    pending.observing = pending
        .observed
        .iter()
        .map(|row| {
            (
                row.subject,
                row.subject
                    .uses_vault_source()
                    .then(|| app.plan.vault_url.clone()),
                true,
            )
        })
        .collect();
    pending.observed = vec![Observation::unknown(dock)];
    pending.unconfirmed_observations = vec![dock];
    let developer = format!("system-state/line/{}/switch", Line::Developer.slug());
    for queued in [true, false] {
        for (_, _, waiting) in &mut pending.observing {
            *waiting = queued;
        }
        app.publish(pending.clone());
        let frame = compile_in(&app, &view, 900.0, 824.0);
        let compact = compile_in(&app, &view, 346.0, 824.0);
        for key in [
            "system-state/card",
            developer.as_str(),
            "system-state/headline",
            "system-state/progress",
        ] {
            assert_eq!(
                semantic(&frame, key).rect,
                semantic(&before, key).rect,
                "{key}"
            );
            assert_eq!(
                semantic(&compact, key).rect,
                semantic(&compact_before, key).rect,
                "compact {key}"
            );
        }
        let cards = drawn(&app).1;
        assert!(
            cards
                .iter()
                .any(|item| item.mark == Mark::Failed && item.retained)
        );
        assert!(
            semantic(&frame, "system-state/card")
                .label
                .contains("last read")
        );
        assert!(
            semantic(&frame, "system-state/phase")
                .label
                .contains(if queued { "queued" } else { "checking" })
        );
        assert_eq!(app.reviewed_row(dock).standing, Some(Standing::Unknown));
        assert!(!app.reviewed_row(dock).settled());
        assert!(app.card_buttons().is_empty());
        app.act(&mut view, "system-state/headline/start", None);
        assert!(
            app.take_action().is_none(),
            "failure history cannot start setup"
        );
    }

    let mut fresh = pending.clone();
    fresh.observed = vec![original_failure.stamped(1_700_000_001)];
    fresh.unconfirmed_observations.clear();
    fresh.observing.retain(|(subject, _, _)| *subject != dock);
    app.publish(fresh.clone());
    assert_eq!(app.reviewed_row(dock).standing, Some(Standing::Failed));
    assert!(
        drawn(&app)
            .1
            .iter()
            .any(|item| item.mark == Mark::Failed && !item.retained)
    );
    fresh.observed[0].note = Some("The new Dock reading failed differently".into());
    app.publish(fresh);
    assert!(
        app.check_card.is_none(),
        "a changed current failure supersedes the prior card"
    );
    assert!(drawn(&app).1.iter().any(|item| {
        !item.retained
            && item
                .reason
                .as_deref()
                .is_some_and(|note| note.contains("failed differently"))
    }));
}

/// The `set-up` reading with Spotlight indexing read as `standing`, `now`, `note` (FL-188).
fn with_spotlight(name: &str, standing: Standing, now: &str, note: Option<&str>) -> SetupStatus {
    let mut status = status_of(name);
    let row = status
        .observed
        .iter_mut()
        .find(|row| row.subject == Subject::Access(Access::SpotlightScope))
        .expect("every fixture reads the scope");
    row.standing = standing;
    row.now = Some(now.to_owned());
    row.note = note.map(str::to_owned);
    status
}

/// THE MAC LINE ENDS WITH SPOTLIGHT INDEXING READ FROM THE SCOPE (FL-188): each state FL-127's
/// scope reads in (applied, owed, a person's removal, off, newcomers) is the Mac settings line's
/// last row, in its words and mark, and on no other line: applied is the scope's words with the
/// check; owed is Needs your password → the scope, its card item, and the line saying it needs
/// the person; a folder taken back reads Kept as you set it, changed since aterm set it, and the
/// line says so; off is left as found. Newcomers read in place: no card, no attention, the
/// headline and hero as with the scope applied. The page
/// lays the row under its own key with its words, and an Off Mac line shows it alone and says
/// Needs you over an owed scope rather than Left as is.
/// CONTROL: the Mac line as main reads it, which has no such row: main puts the scope on
/// Permissions (`Line::of`), so the Mac line's last row is the Dock.
#[test]
fn the_mac_line_ends_with_spotlight_indexing_read_from_the_scope() {
    use aterm_types::macos_setup::{
        SPOTLIGHT_KEPT_AS_SET, SPOTLIGHT_OFF, SPOTLIGHT_OWED, SPOTLIGHT_SCOPED, spotlight_newcomers,
    };
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let spotlight = Subject::Access(Access::SpotlightScope);
    let owed_note = "Your Mac password is required to exclude: trust.";
    let last = |app: &SystemStateApp| {
        let mac = line_of(app, Line::Mac);
        let row = mac
            .rows
            .last()
            .expect("CONTROL: the Mac line as main reads it, which has no such row")
            .clone();
        assert_eq!(
            row.subject, spotlight,
            "CONTROL: the Mac line as main reads it, which has no such row"
        );
        assert!(
            !line_of(app, Line::Permissions)
                .rows
                .iter()
                .any(|row| row.subject == spotlight),
            "Permissions no longer holds the scope"
        );
        (mac, row)
    };
    let mark = |row: &RowView| page::row_mark(row);

    // Applied: the scope's words, with the check.
    let mut app = reading(&with_spotlight(
        "set-up",
        Standing::InPlace,
        SPOTLIGHT_SCOPED,
        None,
    ));
    let (_, row) = last(&app);
    assert_eq!(row.value, RowValue::One(SPOTLIGHT_SCOPED.into()));
    assert_eq!(mark(&row), Mark::Done);
    let view = open(&mut app, Line::Mac);
    let compiled = compile_in(&app, &view, 900.0, 4000.0);
    let info = semantic(&compiled, "system-state/row/spotlight-indexing/info");
    assert!(
        info.label.contains("Spotlight indexing") && info.label.contains(SPOTLIGHT_SCOPED),
        "{}",
        info.label
    );

    // Owed: Needs your password → the scope, held by the card, named by the line, whose Finish
    // setup… goes by the row's own slug.
    let mut app = reading(&with_spotlight(
        "set-up",
        Standing::NeedsYou,
        SPOTLIGHT_OWED,
        Some(owed_note),
    ));
    let (mac, row) = last(&app);
    assert_eq!(
        row.value,
        RowValue::Change {
            now: SPOTLIGHT_OWED.into(),
            wanted: SPOTLIGHT_SCOPED.into()
        }
    );
    assert_eq!(mark(&row), Mark::NeedsYou);
    assert!(
        drawn(&app)
            .1
            .iter()
            .any(|item| item.subject == Some(spotlight))
    );
    assert_eq!(mac.phrase, "Needs you: Spotlight indexing");
    let mut view = SystemStateViewState::default();
    app.act(&mut view, "system-state/row/spotlight-indexing/open", None);
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::OpenAccess {
            access: Access::SpotlightScope,
            ..
        })
    ));

    // A person took a folder back: theirs, changed since aterm set it, no check.
    let mut status = with_spotlight("set-up", Standing::InPlace, SPOTLIGHT_SCOPED, None);
    status.spotlight_taken_back = vec!["$HOME/trust".into()];
    let app = reading(&status);
    let (mac, row) = last(&app);
    assert_eq!(row.value, RowValue::One(SPOTLIGHT_KEPT_AS_SET.into()));
    assert!(row.changed_since);
    assert_eq!(mark(&row), Mark::Off);
    assert!(
        row.note
            .as_deref()
            .is_some_and(|note| note.contains("$HOME/trust"))
    );
    assert_eq!(
        mac.phrase,
        format!("Spotlight indexing {}", super::CHANGED_SINCE_WORDS)
    );

    // Off, whoever turned it off: left as found.
    let app = reading(&with_spotlight(
        "set-up",
        Standing::InPlace,
        SPOTLIGHT_OFF,
        None,
    ));
    let (_, row) = last(&app);
    assert_eq!(row.chip, Some(Chip::LeftAsIs));
    assert_eq!(mark(&row), Mark::Off);
    assert!(row.label.contains("left as found"), "{}", row.label);

    // Newcomers read in place (FL-127: a folder made since is a person's request, not a Mac left
    // unfinished): no card, no attention, and the headline and hero read as with the scope applied.
    let applied = reading(&with_spotlight(
        "set-up",
        Standing::InPlace,
        SPOTLIGHT_SCOPED,
        None,
    ));
    let app = reading(&with_spotlight(
        "set-up",
        Standing::InPlace,
        &spotlight_newcomers(2),
        Some("Made since setup scoped Spotlight: a, b."),
    ));
    let (_, row) = last(&app);
    assert_eq!(row.value, RowValue::One(spotlight_newcomers(2)));
    assert_eq!(mark(&row), Mark::Done);
    let (_, card, hero) = drawn(&app);
    assert!(!card.iter().any(|item| item.subject == Some(spotlight)));
    assert!(!app.needs_attention());
    let (_, _, calm) = drawn(&applied);
    assert_eq!(app.headline().text, applied.headline().text);
    assert_eq!(
        (
            hero.headline.as_str(),
            hero.sub.as_str(),
            hero.done,
            hero.total
        ),
        (
            calm.headline.as_str(),
            calm.sub.as_str(),
            calm.done,
            calm.total
        ),
        "a newcomer leaves the hero as the applied scope does"
    );

    // An Off Mac line shows the row alone, and never reads calm over an owed scope.
    let app = reading(&with_spotlight(
        "non-alab",
        Standing::InPlace,
        SPOTLIGHT_SCOPED,
        None,
    ));
    let mac = line_of(&app, Line::Mac);
    assert!(!mac.on);
    assert_eq!(
        mac.rows.iter().map(|row| row.subject).collect::<Vec<_>>(),
        [spotlight]
    );
    assert_eq!(mac.phrase, lines::LEFT_AS_IS);
    let app = reading(&with_spotlight(
        "non-alab",
        Standing::NeedsYou,
        SPOTLIGHT_OWED,
        Some(owed_note),
    ));
    let mac = line_of(&app, Line::Mac);
    assert_eq!(
        (mac.mark, mac.phrase.as_str()),
        (Mark::NeedsYou, "Needs you: Spotlight indexing")
    );
}
