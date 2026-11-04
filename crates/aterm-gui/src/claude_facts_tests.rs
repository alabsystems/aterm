// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for footer fact click masks, hover titles, and screen-reader labels.

use std::path::Path;

use aterm_agent::harness::footer::{FactKind, FactMask, FooterFacts};
use aterm_agent::harness::session_usage::{ModelTokens, UsageFacts};

use super::{
    default_mask_for, fact_spoken_of, fact_title, mask_after, runtime_spoken, tokens_spoken,
};
use crate::claude_lights::Fact;

fn facts() -> FooterFacts {
    FooterFacts {
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        path: Some("~/aterm".into()),
        branch: Some("main".into()),
        usage: Some(UsageFacts {
            models: vec![ModelTokens {
                label: "opus".into(),
                input: 20_000_000,
                output: 82_000,
            }],
            runtime: Some(393),
            ..UsageFacts::default()
        }),
        ..FooterFacts::default()
    }
}

fn mask(kinds: &[FactKind]) -> FactMask {
    kinds
        .iter()
        .fold(FactMask::default(), |m, k| m.with(*k, true))
}

/// A MUSE SESSION STARTS WITHOUT THE FACTS ITS OWN STATUS LINE PRINTS (model, effort,
/// folder), and with everything aterm alone knows still shown; every other program
/// starts with nothing hidden.
#[test]
fn a_muse_session_starts_without_repeating_its_own_status_line() {
    use aterm_phase::Program;
    assert_eq!(
        default_mask_for(Some(Program::Muse)),
        mask(&[FactKind::Model, FactKind::Effort, FactKind::Path])
    );
    for other in [
        None,
        Some(Program::Claude),
        Some(Program::Codex),
        Some(Program::Grok),
        Some(Program::Kimi),
        Some(Program::Agy),
        Some(Program::Generic),
    ] {
        assert_eq!(default_mask_for(other), FactMask::default(), "{other:?}");
    }
    let muse = default_mask_for(Some(Program::Muse));
    for shown in [FactKind::Branch, FactKind::Context] {
        assert!(!muse.has(shown), "{shown:?} stays shown");
    }
}

#[test]
fn a_click_hides_its_fact_and_a_stub_click_shows_its_own_again() {
    let none = FactMask::default();
    assert_eq!(
        mask_after(none, Fact::Branch, none),
        mask(&[FactKind::Branch])
    );
    assert_eq!(
        mask_after(mask(&[FactKind::Path]), Fact::Model, none),
        mask(&[FactKind::Path, FactKind::Model])
    );
    let both = mask(&[FactKind::Model, FactKind::Effort, FactKind::Branch]);
    assert_eq!(
        mask_after(
            both,
            Fact::Model,
            mask(&[FactKind::Model, FactKind::Effort])
        ),
        mask(&[FactKind::Branch]),
        "a stub of both shows the model and the effort again"
    );
    assert_eq!(
        mask_after(both, Fact::Effort, mask(&[FactKind::Effort])),
        mask(&[FactKind::Model, FactKind::Branch]),
        "an effort stub shows the effort alone"
    );
    assert_eq!(
        mask_after(mask(&[FactKind::Branch]), Fact::Branch, none),
        mask(&[FactKind::Branch])
    );
}

#[test]
fn a_title_names_the_value_and_the_click() {
    let shown = facts();
    let cwd = Path::new("/Users//someone/aterm");
    let title = |f: &FooterFacts, fact| fact_title(f, Some(cwd), fact);
    assert_eq!(
        title(&shown, Fact::Model),
        (
            "Opus 5.5 \u{b7} click: hide \u{b7} right-click: model menu".into(),
            "click: hide".into()
        )
    );
    assert_eq!(
        title(&shown, Fact::Effort).0,
        "effort xhigh \u{b7} click: hide \u{b7} right-click: effort menu"
    );
    assert_eq!(
        title(&shown, Fact::Path).0,
        "/Users//someone/aterm \u{b7} click: hide \u{b7} right-click: copy, new tab",
        "the absolute working directory, not the rule's `~/aterm`"
    );
    assert_eq!(
        fact_title(&shown, None, Fact::Path).0,
        "~/aterm \u{b7} click: hide \u{b7} right-click: copy, new tab",
        "with no side published, the rule's own path"
    );
    assert_eq!(
        title(&shown, Fact::Branch).0,
        "\u{2387} main \u{b7} click: hide \u{b7} right-click: copy"
    );
    assert_eq!(
        title(&shown, Fact::Usage),
        (
            "\u{03a3} 6m33s working \u{b7} \u{2191}20M \u{2193}82k \u{b7} click: graph over time"
                .into(),
            "click: graph".into()
        )
    );
    let hidden = FooterFacts {
        off: mask(&[FactKind::Branch]),
        ..facts()
    };
    assert_eq!(
        title(&hidden, Fact::Branch),
        (
            "branch hidden \u{b7} click shows it".into(),
            "click: show".into()
        )
    );
    let both = FooterFacts {
        off: mask(&[FactKind::Model, FactKind::Effort]),
        ..facts()
    };
    assert_eq!(
        title(&both, Fact::Model).0,
        "model hidden \u{b7} click shows it"
    );
    assert_eq!(
        title(&both, Fact::Effort).0,
        "effort hidden \u{b7} click shows it"
    );
    let model = FooterFacts {
        off: mask(&[FactKind::Model]),
        ..facts()
    };
    assert_eq!(
        title(&model, Fact::Model).0,
        "model hidden \u{b7} click shows it"
    );
}

#[test]
fn a_reader_hears_each_fact_in_words() {
    let shown = facts();
    let cwd = Path::new("/Users//someone/aterm");
    let none = FactMask::default();
    let spoken = |fact, restores| fact_spoken_of(&shown, Some(cwd), fact, restores);
    assert_eq!(
        spoken(Fact::Model, none),
        ("Model: Opus 5.5".into(), "Activate to hide it.".into())
    );
    assert_eq!(spoken(Fact::Effort, none).0, "Effort: xhigh");
    assert_eq!(
        spoken(Fact::Path, none),
        (
            "Working directory: /Users//someone/aterm".into(),
            "Activate to hide it. Its menu copies it or opens a new tab there.".into()
        )
    );
    assert_eq!(
        spoken(Fact::Branch, none),
        (
            "Branch: main".into(),
            "Activate to hide it. Its menu copies it.".into()
        )
    );
    assert_eq!(
        spoken(Fact::Usage, none),
        (
            "Token traffic: 6 minutes 33 seconds working, 20 million in, 82 thousand out".into(),
            "Activate for a graph over time.".into()
        )
    );
    assert_eq!(
        spoken(Fact::Branch, mask(&[FactKind::Branch])),
        (
            "Branch: main off".into(),
            "Activate to show it again.".into()
        ),
        "a stub is named as it is drawn: `\u{2387} main off`"
    );
    assert_eq!(
        spoken(Fact::Model, mask(&[FactKind::Model])).0,
        "Model: Opus 5.5 off"
    );
    assert_eq!(
        spoken(Fact::Effort, mask(&[FactKind::Effort])).0,
        "Effort: xhigh off"
    );
    assert_eq!(tokens_spoken(940), "940");
    assert_eq!(tokens_spoken(1_200), "1.2 thousand");
    assert_eq!(tokens_spoken(3_000_000), "3 million");
    assert_eq!(tokens_spoken(3_100_000_000), "3.1 billion");
    assert_eq!(runtime_spoken(1), "1 second");
    assert_eq!(runtime_spoken(42), "42 seconds");
    assert_eq!(runtime_spoken(7 * 3600 + 52 * 60), "7 hours 52 minutes");
    assert_eq!(runtime_spoken(3600 + 60), "1 hour 1 minute");
    assert_eq!(runtime_spoken(60), "1 minute", "no `0 seconds`");
    assert_eq!(runtime_spoken(2 * 3600), "2 hours", "no `0 minutes`");
}

/// AN ACCOUNT WITH NO WEEKLY FIGURE says why in its title, in the footer's own words
/// (`profile::WeeklyGap::words`), and to a reader in full (`WeeklyGap::spoken`, no glyph); a
/// figure, where there is one, says the week instead, its age after a comma.
#[test]
fn an_account_with_no_weekly_figure_names_why_in_its_title_and_to_a_reader() {
    use aterm_agent::harness::profile::{SubscriptionDisplay, WeeklyGap};
    let with = |weekly: Option<&str>| FooterFacts {
        subscription: Some(SubscriptionDisplay {
            email: Some("owner@example.com".into()),
            plan: Some("Max 20x".into()),
            weekly: weekly.map(str::to_owned),
            weekly_gap: Some(WeeklyGap::Untapped),
            ..SubscriptionDisplay::default()
        }),
        ..facts()
    };
    let said = |facts: &FooterFacts| {
        (
            fact_title(facts, None, Fact::Account).0,
            fact_spoken_of(facts, None, Fact::Account, FactMask::default()).0,
        )
    };
    let (title, spoken) = said(&with(None));
    assert!(
        title.contains("weekly \u{2013} \u{b7} until relaunch"),
        "{title}"
    );
    assert_eq!(
        spoken,
        "Account: owner@example.com, Max 20x, weekly figure unknown until aterm next relaunches this Claude Code"
    );
    let (title, spoken) = said(&with(Some("weekly 62% left")));
    assert!(
        title.contains("weekly 62% left") && !title.contains("until relaunch"),
        "{title}"
    );
    assert_eq!(
        spoken,
        "Account: owner@example.com, Max 20x, weekly 62% left"
    );
    let (title, spoken) = said(&with(Some("weekly 62% left \u{b7} 40m ago")));
    assert!(title.contains("weekly 62% left \u{b7} 40m ago"), "{title}");
    assert_eq!(
        spoken,
        "Account: owner@example.com, Max 20x, weekly 62% left, 40m ago"
    );
}

#[test]
fn a_watching_inventory_keeps_the_observed_week_or_its_missing_reading_reason() {
    use aterm_agent::harness::account_progress::{AccountProgress, ProgressPhase};
    use aterm_agent::harness::footer::{WEEKLY_MARK, account_progress_text, segments};
    use aterm_agent::harness::profile::{SubscriptionDisplay, WeeklyGap};

    for (known, eligible) in [(Some(2), Some(1)), (Some(2), None), (None, None)] {
        for weekly in [Some("weekly 62% left"), None] {
            let mut progress = AccountProgress::unknown(100);
            progress.phase = ProgressPhase::Watching;
            progress.known = known;
            progress.eligible = eligible;
            let inventory = account_progress_text(&progress, false);
            let facts = FooterFacts {
                subscription: Some(SubscriptionDisplay {
                    email: Some("owner@example.test".into()),
                    plan: Some("Max 20x".into()),
                    weekly: weekly.map(str::to_owned),
                    weekly_gap: Some(WeeklyGap::Untapped),
                    ..Default::default()
                }),
                account_progress: Some(progress),
                ..facts()
            };
            let before = facts.clone();
            let mut without_progress = facts.clone();
            without_progress.account_progress = None;
            let title = fact_title(&facts, None, Fact::Account).0;
            let spoken = fact_spoken_of(&facts, None, Fact::Account, FactMask::default()).0;
            for (said, observation) in [
                (&title, weekly.unwrap_or(WeeklyGap::Untapped.words())),
                (&spoken, weekly.unwrap_or(WeeklyGap::Untapped.spoken())),
            ] {
                let recovery_at = said.find(&inventory).expect("recovery inventory retained");
                let quota_at = said
                    .find(observation)
                    .expect("observed quota or reason retained");
                assert!(recovery_at < quota_at, "recovery leads: {said}");
                assert!(said.contains("owner@example.test"));
            }
            let week = |facts: &FooterFacts| {
                segments(facts)
                    .into_iter()
                    .find(|segment| segment.mark == WEEKLY_MARK)
            };
            assert_eq!(week(&facts), week(&without_progress));
            assert_eq!(week(&facts).is_some(), weekly.is_some());
            assert_eq!(
                facts, before,
                "disclosures read the same retained observation"
            );
        }
    }
}

#[test]
fn recovery_work_limits_and_busy_reads_keep_their_existing_disclosure_priority() {
    use aterm_agent::harness::account_progress::{AccountProgress, ProgressPhase};
    use aterm_agent::harness::outlook::Glance;
    use aterm_agent::harness::profile::SubscriptionDisplay;
    use aterm_agent::harness::session_usage::Wall;

    for (phase, busy, transcript_wall) in [
        (ProgressPhase::Unknown, false, false),
        (ProgressPhase::Waiting, false, false),
        (ProgressPhase::Paused, false, false),
        (ProgressPhase::Ready, false, false),
        (ProgressPhase::Switching { to: "alt1".into() }, false, false),
        (ProgressPhase::Verifying { to: "alt1".into() }, false, false),
        (
            ProgressPhase::SigningIn {
                label: "alt1".into(),
                route: Some("mailbox".into()),
            },
            false,
            false,
        ),
        (ProgressPhase::Watching, true, false),
        (ProgressPhase::Watching, false, true),
    ] {
        let mut progress = AccountProgress::unknown(100);
        progress.phase = phase;
        progress.known = Some(2);
        progress.eligible = Some(1);
        progress.next_at = Some(200);
        let facts = FooterFacts {
            subscription: Some(SubscriptionDisplay {
                email: Some("owner@example.test".into()),
                weekly: Some("weekly 62% left".into()),
                ..Default::default()
            }),
            account_progress: Some(progress),
            account_progress_busy: busy,
            usage: Some(UsageFacts {
                wall: transcript_wall.then(|| Wall {
                    label: "7d",
                    resets: "Thu 09:00".into(),
                }),
                ..Default::default()
            }),
            glance: Some(Glance {
                title_verdict: Some("synthetic future movement".into()),
                spoken: Some("synthetic future movement".into()),
                ..Default::default()
            }),
            ..facts()
        };
        let view = facts.account_progress_view().expect("recovery observation");
        if busy {
            assert_eq!(view.phase, ProgressPhase::Unknown);
            assert_eq!(view.eligible, None);
            assert_eq!(view.next_at, None);
        }
        if transcript_wall {
            assert_eq!(view.phase, ProgressPhase::Waiting);
            assert!(view.wall.is_some());
        }
        let mut recovery_only = facts.clone();
        recovery_only.subscription = None;
        recovery_only.glance = None;
        recovery_only.account = None;
        assert_eq!(
            fact_title(&facts, None, Fact::Account),
            fact_title(&recovery_only, None, Fact::Account),
            "{:?}, busy={busy}, transcript_wall={transcript_wall}",
            view.phase,
        );
        assert_eq!(
            fact_spoken_of(&facts, None, Fact::Account, FactMask::default()),
            fact_spoken_of(&recovery_only, None, Fact::Account, FactMask::default()),
            "{:?}, busy={busy}, transcript_wall={transcript_wall}",
            view.phase,
        );
    }
}

#[test]
fn watching_disclosures_use_the_panes_glance_without_inventing_a_profile() {
    use aterm_agent::harness::account_progress::{AccountProgress, ProgressPhase};
    use aterm_agent::harness::outlook::Glance;
    use aterm_agent::harness::profile::SubscriptionDisplay;

    let future = "synthetic future movement";
    for paused in [false, true] {
        let mut progress = AccountProgress::unknown(100);
        progress.phase = ProgressPhase::Watching;
        progress.forecasts_paused = paused;
        progress.known = Some(2);
        let mut facts = FooterFacts {
            subscription: Some(SubscriptionDisplay {
                email: Some("owner@example.test".into()),
                weekly: Some("weekly 62% left".into()),
                ..Default::default()
            }),
            account_progress: Some(progress),
            glance: Some(Glance {
                reset: Some("Thu 09:00".into()),
                five: Some("5h 62% left".into()),
                title_verdict: Some(future.into()),
                spoken: Some(future.into()),
                ..Default::default()
            }),
            ..facts()
        };
        for said in [
            fact_title(&facts, None, Fact::Account).0,
            fact_spoken_of(&facts, None, Fact::Account, FactMask::default()).0,
        ] {
            assert!(said.contains("weekly 62% left"), "{said}");
            assert_eq!(said.contains(future), !paused, "{said}");
        }
        facts.subscription = None;
        facts.account = Some("retained-login".into());
        let retained_glance = facts.glance.clone();
        for with_progress in [true, false] {
            if !with_progress {
                facts.account_progress = None;
            }
            for said in [
                fact_title(&facts, None, Fact::Account).0,
                fact_spoken_of(&facts, None, Fact::Account, FactMask::default()).0,
            ] {
                assert!(said.contains("retained-login"), "{said}");
                for absent in [
                    "owner@example.test",
                    "weekly 62% left",
                    "Thu 09:00",
                    "5h 62% left",
                    future,
                ] {
                    assert!(!said.contains(absent), "{said}");
                }
            }
            assert_eq!(
                facts.glance, retained_glance,
                "raw history remains retained"
            );
        }
    }
}
