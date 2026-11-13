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

/// A Claude Code card over `value` in a pane `cols` wide, with one account to switch to.
fn card_facts(value: &AccountProgress, cols: usize) -> FactMenuFacts {
    FactMenuFacts {
        account_progress: Some(value.clone()),
        cli: Some(aterm_agent::harness::accounts::Cli::Claude),
        product: "Claude Code",
        now: 200,
        room: crate::autopilot_menu::content_room(cols),
        accounts: vec![AccountMenuRow {
            account: AccountLabel::new("primary").unwrap(),
            name: "primary@example.test".into(),
            trail: "not read".into(),
            spoken: "primary@example.test, not read".into(),
            active: true,
            enabled: true,
        }],
        ..Default::default()
    }
}

/// The card's automatic-switching sentence: its wrapped rows as shown, and as spoken.
fn sentence(menu: &[MenuRow]) -> (String, String) {
    let at = menu
        .iter()
        .position(|row| matches!(row, MenuRow::Section(title) if title.ends_with("witching")))
        .expect("the switching section");
    let notes: Vec<(&str, &str)> = menu[at + 1..]
        .iter()
        .map_while(|row| match row {
            MenuRow::Note { text, spoken, .. } => Some((text.as_str(), spoken.as_str())),
            _ => None,
        })
        .collect();
    (
        notes
            .iter()
            .map(|note| note.0)
            .collect::<Vec<_>>()
            .join(" "),
        notes.iter().map(|note| note.1).collect::<String>(),
    )
}

/// POOL WORK IS THE SENTENCE'S LAST CLAUSE: independent sign-in work reaches the card as the
/// closing clause of its one automatic-switching sentence, beside the primary decision and
/// never in its place; its route and the time it was observed are clipboard evidence, and the
/// hover and the spoken fact keep both.
#[test]
fn pool_work_reaches_menu_copy_hover_and_spoken_text_beside_the_primary_decision() {
    let value = progress();
    let before = value.clone();
    let report = details_report(&card_facts(&value, 60));
    assert!(report.contains("Waiting"), "{report}");
    assert!(report.contains("pool-B") && report.contains("mailbox"));
    assert!(report.contains(&aterm_agent::harness::usage::rfc3339_utc(120)));
    assert!(report.contains(&aterm_agent::harness::usage::rfc3339_utc(500)));
    for (phase, primary) in [
        (
            ProgressPhase::Waiting,
            "At its limit. aterm looks again 00:08.",
        ),
        (
            ProgressPhase::Watching,
            "On: 1 of this Mac's 3 accounts can take over.",
        ),
    ] {
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
        for cols in [40, 60, 120] {
            let menu = rows(Fact::Account, &card_facts(&shown, cols));
            let (text, said) = sentence(&menu);
            let want = format!("{primary} Signing in pool-B in the background.");
            assert_eq!(text, want, "{cols} columns");
            assert_eq!(said, want, "spoken once, whole");
            assert!(
                !text.contains(&aterm_agent::harness::usage::rfc3339_utc(120)),
                "no instant on the card"
            );
        }
    }
    assert_eq!(value, before);
}

/// The painted card keeps the whole account name of the pool work, however long, at every
/// width; the route and the original time stay in the copied report.
#[test]
fn painted_pool_work_keeps_the_full_account_and_copy_keeps_route_and_time() {
    let mut value = progress();
    let label = "operations-account-01";
    assert!(aterm_agent::harness::accounts::admissible_label(label));
    value.independent_work[0].label = label.into();
    let before = value.clone();
    let stamp = aterm_agent::harness::usage::rfc3339_utc(120);
    let report = details_report(&card_facts(&value, 60));
    for fact in [label, "mailbox", &stamp] {
        assert!(report.contains(fact));
    }
    let theme = aterm_render::Theme {
        fg: 0x00E0_E0E0,
        bg: 0x0018_1818,
        cursor: 0x00FF_FFFF,
        selection: 0x0044_4466,
    };
    for cols in [40, 60, 120] {
        let menu = rows(Fact::Account, &card_facts(&value, cols));
        let (_, spoken) = sentence(&menu);
        assert_eq!(spoken.matches(label).count(), 1);
        let rect = crate::autopilot_menu::place(&menu, (39, 0), (0..40, 0..cols), (cols, 40), 0)
            .expect("the account menu fits");
        let painted =
            crate::autopilot_menu::paint(rect, &menu, Default::default(), None, theme, None);
        let visible = painted
            .iter()
            .map(|row| row.iter().map(|cell| cell.ch).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            visible.contains(label),
            "{cols} columns hid the full account label"
        );
        assert!(!visible.contains('\u{2026}') || visible.matches('\u{2026}').count() == 2);
    }
    assert_eq!(value, before);
}

/// Sign-in diagnostics are clipboard and wire evidence: every account's full label, however
/// alike two are, its result's words and when the attempt finished, with the observation
/// unchanged. The card draws none of them.
#[test]
fn login_diagnostics_keep_distinct_full_labels_in_copy_and_on_the_wire() {
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
    let common = "a".repeat(accounts::LABEL_MAX - 1);
    let labels = [
        "ops".to_owned(),
        "m".repeat(40),
        "ops-account_01.region".to_owned(),
        format!("{common}b"),
        format!("{common}c"),
    ];
    assert_eq!(labels[3].len(), accounts::LABEL_MAX);
    assert_ne!(labels[3], labels[4]);
    let mut value = progress();
    value.independent_work.clear();
    for (index, label) in labels.iter().enumerate() {
        assert!(accounts::admissible_label(label));
        value
            .login_diagnostics
            .push(diagnostic_for(label, 80 + i64::try_from(index).unwrap()));
    }
    let before = value.clone();
    let facts = card_facts(&value, 60);
    let report = details_report(&facts);
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
        assert!(report.contains(&format!(
            "Last local sign-in for {}: {}",
            diagnostic.label(),
            diagnostic.words()
        )));
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
    let menu = rows(Fact::Account, &facts);
    for row in &menu {
        if let MenuRow::Note { text, spoken, .. } = row {
            assert!(!text.contains("sign-in for") && !spoken.contains("sign-in for"));
        }
    }
    assert_eq!(value, before);
    assert_eq!(details_report(&facts), report);
    assert_eq!(
        crate::harness_recovery_verb::cmd("recovery", true, || Ok(Some(value.clone()))),
        wire
    );
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
