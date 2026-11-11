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
    assert!(!row.copy_command && !row.install_tools);
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
            row.note.is_none() && !row.turn_on_sync && !row.install_tools && !row.copy_command,
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
    assert_eq!(hero.sub, "Showing last useful results");
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
        (0, 0),
        "past checks grant no current progress"
    );
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
    let held = app.reviewed_row(caps);
    assert!(matches!(held.value, RowValue::LastKnown(_)));
    assert_eq!(held.chip, Some(Chip::LastKnown));
    assert!(!held.settled(), "history supplies no current readiness");
    assert!(held.label.contains("Last read:"));
    assert!(!held.install_tools && !held.turn_on_sync && !held.copy_command);

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
    app.publish(partial);
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

    let mut other_source = complete.clone();
    other_source.observing = vec![(vault, Some("https://other.invalid/vault".into()), false)];
    let other = reading(&other_source);
    assert_eq!(
        other.reviewed_row(vault).chip,
        Some(Chip::InPlace),
        "another vault's progress cannot mark this source as checking"
    );
    other_source.observing = vec![(vault, Some(source), true)];
    assert_eq!(
        drawn(&reading(&other_source)).2.headline,
        "Waiting to read this Mac…"
    );
    app.publish(complete);
    assert_eq!(app.reviewed_row(caps).chip, Some(Chip::InPlace));
    assert_ne!(drawn(&app).2.sub, "Showing last useful results");
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
        assert!(!drawn.on && drawn.rows.is_empty() && !drawn.expandable());
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
            .semantic(&UiKey::new("system-state/line/mac/disclose"))
            .is_none(),
        "an Off line has nothing to open"
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
    for name in FIXTURES {
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

/// Collapsed, every fixture's page fits a 900 × 644 window with its list unscrolled (§4.1).
#[test]
fn every_collapsed_page_fits_900_by_644_with_no_scrolling() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    for name in FIXTURES {
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
    }
}

/// Every fixture renders at the window sizes the captures use (120 × 48 and ~46 columns of a
/// 13 pt terminal) and compact, collapsed and with each line open.
#[test]
fn every_fixture_renders_at_every_size_with_every_line_open() {
    for name in FIXTURES {
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
    assert!(crate::system_state_access::menu_line(&quiet.status, false).is_none());

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

/// The kitty's pose follows the run (§4.5): it looks up for the person, puts an ear back for a
/// failure and settles into a loaf once the Mac is ready; Serious Mode shows the words alone.
#[test]
fn the_kitty_looks_up_for_you_and_loafs_once_the_mac_is_ready() {
    use aterm_effects::pet_glyphs_gen::PetGlyphId;
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
        LockupSpec::parse(custom.paint.as_deref()).pose
    };
    assert_eq!(pose("needs-you"), PetGlyphId::PetSitLookup);
    assert_eq!(pose("failed"), PetGlyphId::PetSitEar);
    assert_eq!(pose("set-up"), PetGlyphId::PetLoaf);
    assert_eq!(pose("running"), PetGlyphId::PetStand);
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
    for name in FIXTURES {
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
    for name in FIXTURES {
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
/// Secrets line is never done: it says it is not joined and why.
#[test]
fn a_mac_in_no_vault_never_reads_secrets_done() {
    let app = reading(&status_of("non-alab"));
    let secrets = line_of(&app, Line::Secrets);
    assert_eq!(
        (secrets.mark, secrets.phrase.as_str(), secrets.word),
        (Mark::Off, lines::NO_VAULT_HERE, "not joined")
    );
}

/// §4.7: the headline is a Heading and the live region says the phase in words of its own, so a
/// person moving through the hero never hears the headline twice.
#[test]
fn the_live_region_never_repeats_the_heading() {
    let mut twice = Vec::new();
    for name in FIXTURES {
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
    for name in FIXTURES {
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
        "github-sign-in",
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
    for name in FIXTURES {
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
    for name in FIXTURES {
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
    for name in FIXTURES {
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
    for name in FIXTURES {
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
