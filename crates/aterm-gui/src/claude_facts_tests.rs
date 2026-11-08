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
/// figure, where there is one, says the week instead, a lower bound read `at least`. A pane with
/// no quota view (agy, Muse Code, a roster not read) names the account as line D's unit does,
/// `this login` for a login the roster cannot name, its roster label where it has one, and never
/// its email (design §2.5: the title is painted in line D's lights block, on the glass).
/// NEGATIVE CONTROL: the profile does name the email, which the title said before (`⎆
/// owner@example.com · Max 20x · …`).
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
    assert_eq!(
        with(None).subscription.and_then(|sub| sub.email).as_deref(),
        Some("owner@example.com"),
        "NEGATIVE CONTROL: the profile names the email"
    );
    let (title, spoken) = said(&with(None));
    assert!(
        title.starts_with("\u{2386} this login \u{b7} Max 20x \u{b7} ")
            && title.contains("weekly \u{2013} \u{b7} until relaunch"),
        "{title}"
    );
    assert_eq!(
        spoken,
        "Account: this login, Max 20x, weekly figure unknown until aterm next relaunches this Claude Code"
    );
    let (title, spoken) = said(&with(Some("weekly 38% used")));
    assert!(
        title.contains("weekly 38% used") && !title.contains("until relaunch"),
        "{title}"
    );
    assert_eq!(spoken, "Account: this login, Max 20x, weekly 38% used");
    let (title, spoken) = said(&with(Some("weekly \u{2265}38% used")));
    assert!(title.contains("weekly \u{2265}38% used"), "{title}");
    assert_eq!(
        spoken,
        "Account: this login, Max 20x, weekly at least 38% used"
    );
    let labelled = FooterFacts {
        account: Some("main".into()),
        ..with(Some("weekly 38% used"))
    };
    let (title, spoken) = said(&labelled);
    assert!(
        title.starts_with("\u{2386} main \u{b7} Max 20x \u{b7} weekly 38% used"),
        "{title}"
    );
    assert_eq!(spoken, "Account: main, Max 20x, weekly 38% used");
    for facts in [with(None), with(Some("weekly 38% used")), labelled] {
        let (title, spoken) = said(&facts);
        for words in [&title, &spoken] {
            assert!(!words.contains("owner@example.com"), "{words}");
            assert!(!words.contains("owner"), "no local part: {words}");
        }
    }
}

/// A WATCHING ROTATION IS SAID LAST, where accounts are (design §5.1): the account's title and
/// speech say the observed week, or why there is none, then the rotation's words, and line D's
/// unit is the same with or without them. Negative control: the rotation said first (the
/// pre-S5 recovery lead) fails the order.
#[test]
fn a_watching_rotation_follows_the_observed_week_or_its_missing_reading_reason() {
    use aterm_agent::harness::account_progress::{AccountProgress, ProgressPhase};
    use aterm_agent::harness::footer::{account_unit, rotation_words};
    use aterm_agent::harness::profile::{SubscriptionDisplay, WeeklyGap};

    for (known, eligible) in [(Some(2), Some(1)), (Some(2), None), (None, None)] {
        for weekly in [Some("weekly 38% used"), None] {
            let mut progress = AccountProgress::unknown(100);
            progress.phase = ProgressPhase::Watching;
            progress.known = known;
            progress.eligible = eligible;
            // No check is due, so the words read no clock.
            let rotation = rotation_words(&progress, |_| String::new());
            let spoken_rotation = format!("R{}", &rotation[1..]);
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
            for (said, rotation, observation) in [
                (
                    &title,
                    &rotation,
                    weekly.unwrap_or(WeeklyGap::Untapped.words()),
                ),
                (
                    &spoken,
                    &spoken_rotation,
                    weekly.unwrap_or(WeeklyGap::Untapped.spoken()),
                ),
            ] {
                let rotation_at = said.find(rotation.as_str()).expect("the rotation's words");
                let quota_at = said
                    .find(observation)
                    .expect("observed quota or reason retained");
                assert!(quota_at < rotation_at, "the rotation is said last: {said}");
                assert!(
                    !said.contains("owner@example.test") && said.contains("this login"),
                    "line D's unit's name, never the email: {said}"
                );
            }
            assert!(
                title.ends_with(&format!("{rotation} \u{b7} click: accounts")),
                "the rotation is the last clause before the click's: {title}"
            );
            assert_eq!(
                account_unit(&facts),
                account_unit(&without_progress),
                "line D says no rotation"
            );
            assert_eq!(
                account_unit(&facts)
                    .first()
                    .is_some_and(|form| form.iter().any(|segment| segment.text == "38%")),
                weekly.is_some()
            );
            assert_eq!(
                facts, before,
                "disclosures read the same retained observation"
            );
        }
    }
}

/// A view whose title and speech say `words`, as the resolver's projected (`quota`) or
/// observed (`quota_observed`) view does.
fn view_saying(words: &str) -> aterm_agent::harness::quota_view::QuotaView {
    use aterm_agent::harness::quota_view::{CLICK, QuotaView};
    QuotaView {
        title: format!("main \u{b7} {words} \u{b7} {CLICK}"),
        spoken: format!("Accounts. main, {words}."),
        ..QuotaView::sample()
    }
}

/// EVERY RECOVERY PHASE SAYS THE OBSERVATION, THEN THE ROTATION (design §5.1): whatever the
/// phase, a busy read or a transcript wall, the account's title and speech say the quota view's
/// words (its observations alone while the recovery refuses forecasts) and end with the
/// rotation's; a transcript wall leads them. Negative control: a recovery that refuses
/// forecasts handed the projected view says the synthetic movement.
#[test]
fn every_recovery_phase_says_the_observation_then_the_rotation() {
    use aterm_agent::harness::account_progress::{AccountProgress, ProgressPhase};
    use aterm_agent::harness::profile::SubscriptionDisplay;
    use aterm_agent::harness::session_usage::Wall;

    let (future, observed) = ("synthetic future movement", "38% of its week used");
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
                weekly: Some("weekly 38% used".into()),
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
            quota: Some(view_saying(future)),
            quota_observed: Some(view_saying(observed)),
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
        let refused = facts.account_forecast_refusal().is_some();
        let what = format!(
            "{:?}, busy={busy}, transcript_wall={transcript_wall}",
            view.phase
        );
        let title = fact_title(&facts, None, Fact::Account).0;
        let spoken = fact_spoken_of(&facts, None, Fact::Account, FactMask::default()).0;
        assert_eq!(
            title.starts_with("limit until Thu 09:00 \u{b7} "),
            transcript_wall,
            "{what}: {title}"
        );
        assert_eq!(
            spoken.starts_with("Account: Usage limit until Thu 09:00. "),
            transcript_wall,
            "{what}: {spoken}"
        );
        for (said, rotation) in [(&title, "rotation: "), (&spoken, "Rotation: ")] {
            let shown = if refused { observed } else { future };
            let unshown = if refused { future } else { observed };
            let quota_at = said.find(shown).unwrap_or_else(|| panic!("{what}: {said}"));
            let rotation_at = said
                .find(rotation)
                .unwrap_or_else(|| panic!("{what}: {said}"));
            assert!(quota_at < rotation_at, "{what}: the rotation last: {said}");
            assert!(!said.contains(unshown), "{what}: {said}");
        }
        assert!(
            title.ends_with(" \u{b7} click: accounts"),
            "{what}: {title}"
        );
    }
}

/// A PANE WITH NO PROFILE SAYS ITS ROSTER LABEL ALONE: while the profile is present the
/// account's words are its quota view's, its movement unsaid while the recovery is paused; a
/// transient profile miss keeps the view as history and says none of it, nor the email, under
/// the label the pane last named. Negative control: the view read without the profile check
/// (`facts.quota_view()`) says the old movement.
#[test]
fn watching_disclosures_use_the_panes_quota_view_without_inventing_a_profile() {
    use aterm_agent::harness::account_progress::{AccountProgress, ProgressPhase};
    use aterm_agent::harness::profile::SubscriptionDisplay;

    let (future, observed) = ("synthetic future movement", "38% of its week used");
    for paused in [false, true] {
        let mut progress = AccountProgress::unknown(100);
        progress.phase = ProgressPhase::Watching;
        progress.forecasts_paused = paused;
        progress.known = Some(2);
        let mut facts = FooterFacts {
            subscription: Some(SubscriptionDisplay {
                email: Some("owner@example.test".into()),
                weekly: Some("weekly 38% used".into()),
                ..Default::default()
            }),
            account_progress: Some(progress),
            quota: Some(view_saying(&format!("{observed} \u{b7} {future}"))),
            quota_observed: Some(view_saying(observed)),
            ..facts()
        };
        for said in [
            fact_title(&facts, None, Fact::Account).0,
            fact_spoken_of(&facts, None, Fact::Account, FactMask::default()).0,
        ] {
            assert!(said.contains(observed), "{said}");
            assert_eq!(said.contains(future), !paused, "{said}");
        }
        facts.subscription = None;
        facts.account = Some("retained-login".into());
        let retained = (facts.quota.clone(), facts.quota_observed.clone());
        for with_progress in [true, false] {
            if !with_progress {
                facts.account_progress = None;
            }
            for said in [
                fact_title(&facts, None, Fact::Account).0,
                fact_spoken_of(&facts, None, Fact::Account, FactMask::default()).0,
            ] {
                assert!(said.contains("retained-login"), "{said}");
                for absent in ["owner@example.test", "weekly 38% used", observed, future] {
                    assert!(!said.contains(absent), "{said}");
                }
            }
            assert_eq!(
                (facts.quota.clone(), facts.quota_observed.clone()),
                retained,
                "raw history remains retained"
            );
            assert!(
                facts
                    .quota_view()
                    .is_some_and(|view| view.title.contains(observed)),
                "CONTROL: the retained view still says its words"
            );
        }
    }
}
