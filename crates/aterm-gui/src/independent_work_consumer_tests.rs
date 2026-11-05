// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::account_progress::{AccountProgress, ActiveSignIn, ProgressPhase};
use aterm_agent::harness::footer::{FactMask, FooterFacts};

fn progress() -> AccountProgress {
    let mut value = AccountProgress::unknown(100);
    value.phase = ProgressPhase::Waiting;
    value.account = Some("primary-A".into());
    value.known = Some(3);
    value.eligible = Some(1);
    value.next_at = Some(500);
    value.independent_work.push(ActiveSignIn {
        label: "pool-B".into(),
        route: Some("mailbox".into()),
        observed_at: 120,
    });
    value
}

fn field(line: &str, key: &str) -> String {
    line.split_whitespace()
        .find_map(|part| part.strip_prefix(&format!("{key}=")))
        .map(aterm_control::wire::pct_decode)
        .unwrap()
}

#[test]
fn pool_work_reaches_menu_copy_hover_and_spoken_text_beside_the_primary_decision() {
    let value = progress();
    let before = value.clone();
    let report = recovery_report(&value);
    assert!(report.contains("Waiting") && report.contains("primary-A"));
    assert!(report.contains("pool-B") && report.contains("mailbox"));
    assert!(report.contains(&aterm_agent::harness::usage::rfc3339_utc(120)));
    assert!(report.contains(&aterm_agent::harness::usage::rfc3339_utc(500)));
    for phase in [ProgressPhase::Waiting, ProgressPhase::Watching] {
        let mut shown = value.clone();
        shown.phase = phase;
        let facts = FooterFacts {
            account_progress: Some(shown.clone()),
            ..Default::default()
        };
        let title = crate::claude_facts::fact_title(&facts, None, Fact::Account).0;
        let spoken =
            crate::claude_facts::fact_spoken_of(&facts, None, Fact::Account, FactMask::default()).0;
        for text in [&title, &spoken] {
            assert!(
                text.contains("pool-B") && text.contains("mailbox"),
                "{text}"
            );
        }
        for cols in [60, 120] {
            let menu = rows(
                Fact::Account,
                &FactMenuFacts {
                    account_progress: Some(shown.clone()),
                    room: crate::autopilot_menu::content_room(cols),
                    ..Default::default()
                },
            );
            let notes = menu
                .iter()
                .filter_map(|row| match row {
                    MenuRow::Note { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(notes.contains("pool-B") && notes.contains("mailbox"));
            assert!(notes.contains(&aterm_agent::harness::usage::rfc3339_utc(120)));
            assert!(notes.contains(&aterm_agent::harness::usage::rfc3339_utc(500)));
        }
    }
    assert_eq!(value, before);
}

#[test]
fn painted_pool_work_keeps_the_full_account_route_and_original_time() {
    let mut value = progress();
    let label = "operations-account-01";
    assert!(aterm_agent::harness::accounts::admissible_label(label));
    value.independent_work[0].label = label.into();
    let before = value.clone();
    let stamp = aterm_agent::harness::usage::rfc3339_utc(120);
    let report = recovery_report(&value);
    for fact in [label, "mailbox", &stamp] {
        assert!(report.contains(fact));
    }
    let theme = aterm_render::Theme {
        fg: 0x00E0_E0E0,
        bg: 0x0018_1818,
        cursor: 0x00FF_FFFF,
        selection: 0x0044_4466,
    };
    for cols in [60, 120] {
        let menu = rows(
            Fact::Account,
            &FactMenuFacts {
                account_progress: Some(value.clone()),
                room: crate::autopilot_menu::content_room(cols),
                ..Default::default()
            },
        );
        let spoken = menu
            .iter()
            .filter_map(|row| match row {
                MenuRow::Note { spoken, .. } => Some(spoken.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(spoken.matches(label).count(), 1);
        assert_eq!(spoken.matches("mailbox").count(), 1);
        let rect = crate::autopilot_menu::place(&menu, (39, 0), (0..40, 0..cols), (cols, 40), 0)
            .expect("the account menu fits");
        let painted = crate::autopilot_menu::paint(rect, &menu, Default::default(), None, theme);
        let visible = painted
            .iter()
            .map(|row| row.iter().map(|cell| cell.ch).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            visible.contains(label),
            "{cols} columns hid the full account label"
        );
        assert!(
            visible.contains(&stamp),
            "{cols} columns hid the original work time"
        );
        assert!(
            visible.contains("mailbox"),
            "{cols} columns hid the current sign-in route"
        );
    }
    assert_eq!(value, before);
}

#[test]
fn painted_login_diagnostics_preserve_distinct_full_labels_without_inventing_characters() {
    use aterm_agent::harness::{
        accounts::{self, Row},
        accounts_sync::LoginDiagnostic,
        open_url::{Met, MetWord},
    };

    let diagnostic_for = |label: &str, completed_at| {
        LoginDiagnostic::from_exit_for_test(
            &Row {
                label: label.into(),
                ..Row::default()
            },
            completed_at,
            1,
            Some(Met::new(MetWord::BrowserControl, None)),
        )
    };
    let short = diagnostic_for("ops", 80);
    let mut short_rows = Vec::new();
    login_diagnostic_rows(&mut short_rows, &short, autopilot_menu::MAX_CONTENT);
    let prefix = short_rows
        .iter()
        .find_map(|row| match row {
            MenuRow::Note { spoken, .. } => spoken.strip_suffix(short.label()),
            _ => None,
        })
        .expect("the short diagnostic's full spoken identity");
    let common = "a".repeat(accounts::LABEL_MAX - 1);
    let labels = [
        short.label().to_owned(),
        "m".repeat(autopilot_menu::MAX_CONTENT - crate::tab_menu::display_cells(prefix)),
        "ops-account_01.region".to_owned(),
        format!("{common}b"),
        format!("{common}c"),
    ];
    assert_eq!(labels[3].len(), accounts::LABEL_MAX);
    assert_eq!(labels[4].len(), accounts::LABEL_MAX);
    assert_eq!(
        &labels[3][..accounts::LABEL_MAX - 1],
        &labels[4][..accounts::LABEL_MAX - 1]
    );
    assert_ne!(labels[3], labels[4]);
    assert_eq!(
        crate::tab_menu::display_cells(&format!("{prefix}{}", labels[1])),
        autopilot_menu::MAX_CONTENT
    );
    let mut value = progress();
    value.independent_work.clear();
    for (index, label) in labels.iter().enumerate() {
        assert!(accounts::admissible_label(label));
        value
            .login_diagnostics
            .push(diagnostic_for(label, 80 + i64::try_from(index).unwrap()));
    }
    let before = value.clone();
    let report = recovery_report(&value);
    let wire = crate::harness_recovery_verb::cmd("recovery", true, || Ok(Some(value.clone())));
    assert_eq!(field(&wire, "phase"), "waiting");
    assert_eq!(field(&wire, "account"), "primary-A");
    assert_eq!(field(&wire, "next_at"), "500");
    assert_eq!(field(&wire, "independent_work_count"), "0");
    assert_eq!(
        field(&wire, "login_diagnostic_count"),
        labels.len().to_string()
    );
    for (index, diagnostic) in value.login_diagnostics.iter().enumerate() {
        assert!(report.contains(diagnostic.label()));
        assert!(report.contains(&diagnostic.words()));
        assert!(report.contains(&aterm_agent::harness::usage::rfc3339_utc(
            diagnostic.completed_at
        )));
        assert_eq!(
            field(&wire, &format!("login_diagnostic_{index}_account")),
            diagnostic.label()
        );
        assert_eq!(
            field(&wire, &format!("login_diagnostic_{index}_words")),
            diagnostic.words()
        );
        assert_eq!(
            field(&wire, &format!("login_diagnostic_{index}_completed_at")),
            diagnostic.completed_at.to_string()
        );
    }

    let theme = aterm_render::Theme {
        fg: 0x00E0_E0E0,
        bg: 0x0018_1818,
        cursor: 0x00FF_FFFF,
        selection: 0x0044_4466,
    };
    let mut painted_identities = Vec::new();
    for cols in [60, 120] {
        let menu = rows(
            Fact::Account,
            &FactMenuFacts {
                account_progress: Some(value.clone()),
                room: autopilot_menu::content_room(cols),
                ..Default::default()
            },
        );
        let height = menu.len() + 3;
        let rect = autopilot_menu::place(
            &menu,
            (height - 1, 0),
            (0..height, 0..cols),
            (cols, height),
            0,
        )
        .expect("every account menu row has room to paint");
        assert_eq!(rect.h, menu.len() + 2);
        let painted = autopilot_menu::paint(rect, &menu, Default::default(), None, theme);
        assert_eq!(painted.len(), menu.len() + 2);
        let painted_row = |index: usize| {
            painted[index + 1][1..rect.w - 1]
                .iter()
                .map(|cell| cell.ch)
                .collect::<String>()
                .trim()
                .to_owned()
        };
        for diagnostic in &value.login_diagnostics {
            let words = diagnostic.words();
            let stamp = aterm_agent::harness::usage::rfc3339_utc(diagnostic.completed_at);
            let spoken_at = |text: &str, from: usize| {
                menu.iter().enumerate().skip(from).find_map(|(index, row)| {
                    matches!(row, MenuRow::Note { spoken, .. } if spoken == text).then_some(index)
                })
            };
            let titles = menu
                .iter()
                .enumerate()
                .filter_map(|(index, row)| match row {
                    MenuRow::Note { spoken, .. } if spoken.ends_with(diagnostic.label()) => {
                        Some((index, spoken.as_str()))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                titles.len(),
                1,
                "the full diagnostic identity is spoken once"
            );
            let (title_at, title) = titles[0];
            let words_at = spoken_at(&words, title_at + 1).expect("full diagnostic result speech");
            let finished_at = menu
                .iter()
                .enumerate()
                .skip(words_at + 1)
                .find_map(|(index, row)| {
                    matches!(row, MenuRow::Note { spoken, .. } if spoken.contains(&stamp))
                        .then_some(index)
                })
                .expect("original finish time in speech");
            for row in &menu[title_at + 1..words_at] {
                assert!(matches!(row, MenuRow::Note { spoken, .. } if spoken.is_empty()));
            }
            for row in &menu[words_at + 1..finished_at] {
                assert!(matches!(row, MenuRow::Note { spoken, .. } if spoken.is_empty()));
            }
            assert_eq!(
                (words_at..finished_at)
                    .map(painted_row)
                    .collect::<Vec<_>>()
                    .join(" "),
                words,
                "{cols} columns preserve the complete safe provider result"
            );
            assert!(painted_row(finished_at).contains(&stamp));
            let shown = (title_at..words_at).map(painted_row).collect::<String>();
            let caption = title.strip_suffix(diagnostic.label()).unwrap().trim_end();
            let label = shown
                .strip_prefix(caption)
                .expect("painted diagnostic caption")
                .trim_start();
            painted_identities.push((cols, diagnostic.label().to_owned(), label.to_owned()));
        }
    }
    assert_eq!(value, before);
    assert_eq!(recovery_report(&value), report);
    assert_eq!(
        crate::harness_recovery_verb::cmd("recovery", true, || Ok(Some(value.clone()))),
        wire
    );
    for (cols, expected, shown) in painted_identities {
        assert_eq!(
            shown, expected,
            "{cols} columns must preserve every account identifier character across painted rows"
        );
    }
}

#[test]
fn lab_and_owner_read_keep_primary_fields_and_independent_work_distinct() {
    let value = progress();
    let facts = crate::lab_screen::progress_facts_at(Some(&value), Some(200));
    for (key, expected) in [
        ("Recovery", "Waiting"),
        ("Account", "primary-A"),
        ("Eligible alternatives", "1"),
    ] {
        assert!(
            facts
                .iter()
                .any(|(name, words)| name == key && words == expected)
        );
    }
    assert!(facts.iter().any(|(name, words)| name == "Local pool work"
        && words.contains("pool-B")
        && words.contains("mailbox")));
    assert!(
        facts
            .iter()
            .any(|(name, words)| name == "Pool work observed"
                && words == &aterm_agent::harness::usage::rfc3339_utc(120))
    );
    let wire = crate::harness_recovery_verb::cmd("recovery", true, || Ok(Some(value.clone())));
    for (key, expected) in [
        ("phase", "waiting"),
        ("account", "primary-A"),
        ("target", "-"),
        ("route", "-"),
        ("eligible", "1"),
        ("next_at", "500"),
        ("observed_at", "100"),
        ("independent_work_count", "1"),
        ("independent_work_0_phase", "signing-in"),
        ("independent_work_0_account", "pool-B"),
        ("independent_work_0_route", "mailbox"),
        ("independent_work_0_observed_at", "120"),
    ] {
        assert_eq!(field(&wire, key), expected);
    }
    assert_eq!(value.independent_work.len(), 1);
}

#[test]
fn independent_work_fields_round_trip_without_becoming_the_primary_target() {
    let mut value = progress();
    value.independent_work[0].label = "pool B %20".into();
    value.independent_work[0].route = None;
    value.independent_work[0].observed_at = 0;
    let wire = crate::harness_recovery_verb::cmd("recovery", true, || Ok(Some(value)));
    assert_eq!(wire.lines().count(), 1);
    assert_eq!(field(&wire, "account"), "primary-A");
    assert_eq!(field(&wire, "target"), "-");
    assert_eq!(field(&wire, "independent_work_0_account"), "pool B %20");
    assert_eq!(field(&wire, "independent_work_0_route"), "-");
    assert_eq!(field(&wire, "independent_work_0_observed_at"), "-");
}
