// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for footer fact click masks, hover titles, and screen-reader labels.

use std::path::Path;

use aterm_agent::harness::footer::{FactKind, FactMask, FooterFacts};
use aterm_agent::harness::session_usage::{ModelTokens, UsageFacts};

use super::{
    Catalogs, EFFORT_SETTLE, EffortWish, ModelFamily, default_mask_for, effort_stops,
    fact_spoken_of, fact_spoken_with, fact_title, fact_title_with, mask_after, model_picks,
    runtime_spoken, tokens_spoken,
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

/// A MUSE OR KIMI SESSION STARTS WITHOUT THE FACTS ITS OWN STATUS ROWS PRINT, and with
/// everything aterm alone knows still shown; every other program starts with nothing hidden.
#[test]
fn a_session_starts_without_repeating_its_own_status_rows() {
    use aterm_phase::Program;
    assert_eq!(
        default_mask_for(Some(Program::Muse)),
        mask(&[FactKind::Model, FactKind::Effort, FactKind::Path])
    );
    assert_eq!(
        default_mask_for(Some(Program::Kimi)),
        mask(&[
            FactKind::Model,
            FactKind::Effort,
            FactKind::Path,
            FactKind::Branch,
            FactKind::Context,
        ])
    );
    for other in [
        None,
        Some(Program::Claude),
        Some(Program::Codex),
        Some(Program::Grok),
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
            "Opus 5.5 \u{b7} click: choose model".into(),
            "click: choose model".into()
        )
    );
    assert_eq!(
        title(&shown, Fact::Effort),
        (
            "effort xhigh \u{b7} click: effort slider".into(),
            "click: effort slider".into()
        )
    );
    assert_eq!(
        fact_title_with(&shown, Some(cwd), Fact::Effort, false),
        (
            "effort xhigh \u{b7} click: hide \u{b7} right-click: effort menu".into(),
            "click: hide".into()
        ),
        "no Claude Code: nothing types `/effort`, and a click hides it"
    );
    assert_eq!(
        title(&shown, Fact::Path).0,
        "/Users//someone/aterm \u{b7} right-click: copy, new tab",
        "the absolute working directory, not the rule's `~/aterm`, and no click: it has none"
    );
    assert_eq!(
        fact_title(&shown, None, Fact::Path).0,
        "~/aterm \u{b7} right-click: copy, new tab",
        "with no side published, the rule's own path"
    );
    assert_eq!(
        title(&shown, Fact::Branch),
        (
            "\u{2387} main \u{b7} right-click: copy".into(),
            "right-click: copy".into()
        )
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
        (
            "Model: Opus 5.5".into(),
            "Activate to choose the model.".into()
        )
    );
    let sonnet = FooterFacts {
        model: Some("Sonnet 4.5".into()),
        effort: Some("high".into()),
        ..facts()
    };
    assert_eq!(
        fact_spoken_with(&sonnet, None, Fact::Effort, none, true).1,
        "Activate for the effort slider."
    );
    assert_eq!(
        fact_spoken_with(&sonnet, None, Fact::Effort, none, false).1,
        "Activate to hide it.",
        "no Claude Code: nothing types `/model`, and a press hides the effort"
    );
    assert_eq!(
        fact_title_with(&sonnet, None, Fact::Model, false).0,
        "Sonnet 4.5 \u{b7} click: hide \u{b7} right-click: model menu"
    );
    assert_eq!(spoken(Fact::Effort, none).0, "Effort: xhigh");
    assert_eq!(
        spoken(Fact::Path, none),
        (
            "Working directory: /Users//someone/aterm".into(),
            "Activate for its menu: copy it or open a new tab there.".into()
        )
    );
    assert_eq!(
        spoken(Fact::Branch, none),
        (
            "Branch: main".into(),
            "Activate for its menu: copy it.".into()
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
/// no quota view (agy, Muse Code, a roster not read) names the account as line D's unit does:
/// by the email its profile names, whether or not the roster names the login (design §2.5, owner
/// ruling 2026-11-09), else by its roster label.
/// NEGATIVE CONTROL: the roster label, which the title said before the ruling, is on no title or
/// speech whose profile names an email.
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
        title.starts_with("\u{2386} owner@example.com \u{b7} Max 20x \u{b7} ")
            && title.contains("weekly \u{2013} \u{b7} until relaunch"),
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
    let (title, spoken) = said(&with(Some("weekly \u{2264}62% left")));
    assert!(title.contains("weekly \u{2264}62% left"), "{title}");
    assert_eq!(
        spoken,
        "Account: owner@example.com, Max 20x, weekly at most 62% left"
    );
    let labelled = FooterFacts {
        account: Some("main".into()),
        ..with(Some("weekly 62% left"))
    };
    let (title, spoken) = said(&labelled);
    assert!(
        title.starts_with("\u{2386} owner@example.com \u{b7} Max 20x \u{b7} weekly 62% left"),
        "{title}"
    );
    assert_eq!(
        spoken,
        "Account: owner@example.com, Max 20x, weekly 62% left"
    );
    for words in [&title, &spoken] {
        assert!(!words.contains("main"), "NEGATIVE CONTROL: {words}");
    }
    // A profile that names no email is named by its roster label.
    let unmailed = FooterFacts {
        subscription: labelled
            .subscription
            .clone()
            .map(|sub| SubscriptionDisplay { email: None, ..sub }),
        ..labelled
    };
    let (title, spoken) = said(&unmailed);
    assert!(
        title.starts_with("\u{2386} main \u{b7} Max 20x \u{b7} weekly 62% left"),
        "{title}"
    );
    assert_eq!(spoken, "Account: main, Max 20x, weekly 62% left");
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
        for weekly in [Some("weekly 62% left"), None] {
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
                    said.contains("owner@example.test") && !said.contains("this login"),
                    "line D's unit's name is the email the profile names: {said}"
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
                    .is_some_and(|form| form.iter().any(|segment| segment.text == "62% left")),
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
                weekly: Some("weekly 62% left".into()),
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
                for absent in ["owner@example.test", "weekly 62% left", observed, future] {
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

use aterm_agent::harness::profile::{FablePlan, SubscriptionDisplay};
use aterm_agent::harness::switch_top::{Baked, BakedModel};

fn on(model: &str, effort: &str) -> FooterFacts {
    FooterFacts {
        model: Some(model.into()),
        effort: Some(effort.into()),
        ..FooterFacts::default()
    }
}

/// `facts` on a plan that bills Fable as `fable`, its usage tap naming the model `id`.
fn subscribed(facts: FooterFacts, id: Option<&str>, fable: Option<FablePlan>) -> FooterFacts {
    FooterFacts {
        subscription: Some(SubscriptionDisplay {
            model_id: id.map(str::to_owned),
            fable,
            ..SubscriptionDisplay::default()
        }),
        ..facts
    }
}

const ALL: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// A build's catalog: `(id, display name, native 1M context, efforts)` a model, the first of
/// each family its newest.
fn baked(models: &[(&str, &str, bool, &[&str])]) -> Baked {
    let family = |id: &str| {
        aterm_agent::harness::claude_model::family_version(id)
            .expect("a model id")
            .0
    };
    let mut latest: Vec<(String, String)> = Vec::new();
    for (id, ..) in models {
        if !latest.iter().any(|(f, _)| *f == family(id)) {
            latest.push((family(id), (*id).to_owned()));
        }
    }
    Baked {
        latest_per_family: latest,
        models: models
            .iter()
            .map(|(id, name, native_1m, efforts)| BakedModel {
                id: (*id).to_owned(),
                family: family(id),
                display_name: (*name).to_owned(),
                native_1m: *native_1m,
                efforts: efforts.iter().map(|l| (*l).to_owned()).collect(),
            })
            .collect(),
    }
}

/// THE MODEL CARD'S TWO CHOICES ARE THE BUILD'S NEWEST FABLE AND NEWEST OPUS, each under the
/// catalog's own name and typed by its id, the session's own checked. No version is aterm's:
/// another build's catalog gives other models under the same two families.
#[test]
fn the_model_card_offers_the_builds_newest_fable_and_newest_opus() {
    let build = baked(&[
        ("claude-fable-5-1", "Fable 5.1", true, ALL),
        ("claude-opus-5-5", "Opus 5.5", true, ALL),
        ("claude-opus-5", "Opus 5", false, &ALL[..3]),
        ("claude-sonnet-5", "Sonnet 5", false, ALL),
    ]);
    let included = Some(FablePlan::Included);
    let picks = model_picks(
        &subscribed(on("Opus 5.5", "high"), None, included.clone()),
        Some(&build),
        0,
    );
    let said: Vec<_> = picks
        .iter()
        .map(|pick| {
            (
                pick.family,
                pick.label.as_str(),
                pick.arg.map(|arg| arg.as_str().to_owned()),
                pick.current,
                pick.note.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        said,
        [
            (
                ModelFamily::Fable,
                "Fable 5.1",
                Some("claude-fable-5-1".to_owned()),
                false,
                None
            ),
            (
                ModelFamily::Opus,
                "Opus 5.5",
                Some("claude-opus-5-5".to_owned()),
                true,
                None
            ),
        ]
    );
    // A session on an older Opus is on neither choice: the Opus choice is the newest.
    let older = model_picks(
        &subscribed(on("Opus 5", "high"), None, included.clone()),
        Some(&build),
        0,
    );
    assert!(older.iter().all(|pick| !pick.current));
    // A newer build's catalog moves both choices with no word of aterm's changed.
    let newer = baked(&[
        ("claude-fable-6", "Fable 6", true, ALL),
        ("claude-opus-6", "Opus 6", true, ALL),
    ]);
    let labels: Vec<_> = model_picks(&on("Fable 6", "high"), Some(&newer), 0)
        .into_iter()
        .map(|pick| (pick.label, pick.current))
        .collect();
    assert_eq!(
        labels,
        [("Fable 6".to_owned(), true), ("Opus 6".to_owned(), false)]
    );
}

/// With no catalog read for the session's build a choice is the family's own name and types the
/// family's alias; a read catalog that offers no model of a family shows that choice and types
/// nothing for it.
#[test]
fn a_build_with_no_catalog_types_the_family_s_alias() {
    let picks = model_picks(&on("Opus 5.5", "high"), None, 0);
    let said: Vec<_> = picks
        .iter()
        .map(|pick| {
            (
                pick.label.as_str(),
                pick.arg.map(|arg| arg.as_str().to_owned()),
                pick.current,
            )
        })
        .collect();
    assert_eq!(
        said,
        [
            ("Fable", Some("fable".to_owned()), false),
            ("Opus", Some("opus".to_owned()), true),
        ],
        "the session's family is the checked one"
    );
    let no_fable = baked(&[("claude-opus-5-5", "Opus 5.5", true, ALL)]);
    let picks = model_picks(&on("Opus 5.5", "high"), Some(&no_fable), 0);
    assert_eq!(
        (
            picks[0].label.as_str(),
            picks[0].arg,
            picks[0].note.as_deref()
        ),
        ("Fable", None, Some("not offered by this Claude Code build"))
    );
    assert_eq!(
        picks[1].label, "Opus 5.5",
        "CONTROL: the family it does offer"
    );
}

/// A session in the million-token context variant keeps it across a pick: the id is typed with
/// the suffix, except for a model whose context is that size natively.
#[test]
fn a_model_pick_keeps_the_session_s_context_variant() {
    let build = baked(&[
        ("claude-fable-5-1", "Fable 5.1", true, ALL),
        ("claude-opus-5-5", "Opus 5.5", false, ALL),
    ]);
    let long = subscribed(on("Opus 4.6", "high"), Some("claude-opus-4-6[1m]"), None);
    let typed = |facts: &FooterFacts, baked: Option<&Baked>| -> Vec<String> {
        model_picks(facts, baked, 0)
            .into_iter()
            .map(|pick| pick.arg.expect("an argument").as_str().to_owned())
            .collect()
    };
    assert_eq!(
        typed(&long, Some(&build)),
        ["claude-fable-5-1", "claude-opus-5-5[1m]"],
        "the suffix where the model needs it, none where its context is native"
    );
    assert_eq!(typed(&long, None), ["fable[1m]", "opus[1m]"]);
    let short = subscribed(on("Opus 4.6", "high"), Some("claude-opus-4-6"), None);
    assert_eq!(
        typed(&short, Some(&build)),
        ["claude-fable-5-1", "claude-opus-5-5"],
        "CONTROL: a session in the standard context types no suffix"
    );
    // A tap reading of another model than the one shown says nothing of the one shown.
    let stale = subscribed(on("Sonnet 4.5", "high"), Some("claude-opus-4-6[1m]"), None);
    assert_eq!(typed(&stale, None), ["fable", "opus"]);
}

/// The Fable choice says how the plan bills it wherever a pick could spend usage credits; a
/// plan that includes Fable says nothing, and the Opus choice never carries the line.
#[test]
fn the_fable_choice_says_when_it_is_billed_to_usage_credits() {
    let note = |fable: Option<FablePlan>, now: i64| {
        let picks = model_picks(&subscribed(on("Opus 5.5", "high"), None, fable), None, now);
        assert_eq!(picks[1].note, None, "the Opus choice");
        picks[0].note.clone()
    };
    let credits = Some("billed to usage credits on this plan".to_owned());
    let unread = Some("billing not read: may use usage credits".to_owned());
    assert_eq!(note(Some(FablePlan::Included), 100), None);
    assert_eq!(note(Some(FablePlan::CreditsOnly), 100), credits);
    assert_eq!(
        note(Some(FablePlan::LimitsEnd(Some(200))), 100),
        None,
        "the plan's Fable limits still run"
    );
    assert_eq!(note(Some(FablePlan::LimitsEnd(Some(200))), 200), credits);
    assert_eq!(note(Some(FablePlan::LimitsEnd(None)), 100), credits);
    assert_eq!(note(Some(FablePlan::Unknown), 100), unread);
    assert_eq!(note(None, 100), unread, "no profile read");
    assert_eq!(
        model_picks(&on("Opus 5.5", "high"), None, 100)[0].note,
        unread,
        "no subscription read"
    );
}

/// The slider's stops are medium, high and xhigh, less the ones the shown model does not take
/// where the build's catalog lists it; its thumb stands on the effort shown, on a pick still
/// standing over it, and on no stop where the level is another or unread.
#[test]
fn the_effort_slider_s_stops_are_the_ones_the_shown_model_takes() {
    let build = baked(&[
        ("claude-opus-5-5", "Opus 5.5", true, ALL),
        ("claude-opus-5", "Opus 5", false, &ALL[..3]),
        ("claude-haiku-4-5", "Haiku 4.5", false, &[]),
    ]);
    assert_eq!(
        effort_stops(&on("Opus 5.5", "high"), Some(&build), None),
        (vec!["medium", "high", "xhigh"], Some(1))
    );
    assert_eq!(
        effort_stops(&on("Opus 5", "medium"), Some(&build), None),
        (vec!["medium", "high"], Some(0)),
        "a model with no xhigh has no such stop"
    );
    assert_eq!(
        effort_stops(&on("Haiku 4.5", "high"), Some(&build), None),
        (vec![], None),
        "a model that takes no effort"
    );
    assert_eq!(
        effort_stops(&on("Sonnet 5", "xhigh"), Some(&build), None),
        (vec!["medium", "high", "xhigh"], Some(2)),
        "a model the catalog does not list keeps every stop"
    );
    assert_eq!(
        effort_stops(&on("Opus 5", "high"), None, None).0,
        ["medium", "high", "xhigh"],
        "no catalog read"
    );
    for off in ["low", "max", "auto", ""] {
        assert_eq!(
            effort_stops(&on("Opus 5.5", off), Some(&build), None).1,
            None,
            "{off:?} is no stop"
        );
    }
    assert_eq!(
        effort_stops(&FooterFacts::default(), None, None).1,
        None,
        "an unread effort"
    );
    assert_eq!(
        effort_stops(&on("Opus 5.5", "high"), Some(&build), Some("xhigh")).1,
        Some(2),
        "a pick stands over the effort shown"
    );
    assert_eq!(
        effort_stops(&on("Opus 5", "high"), Some(&build), Some("xhigh")).1,
        None,
        "a pick the model has no stop for"
    );
}

/// A pick stands as the thumb while it waits or is typed, and for [`EFFORT_SETTLE`] once its
/// press ended done; past that the thumb is the footer's again.
#[test]
fn an_effort_pick_stands_until_it_has_settled() {
    let start = std::time::Instant::now();
    let wish = |until| EffortWish {
        level: "high",
        queued: false,
        until,
    };
    assert!(wish(None).stands(start + EFFORT_SETTLE * 10), "in flight");
    let settling = wish(Some(start + EFFORT_SETTLE));
    assert!(settling.stands(start));
    assert!(!settling.stands(start + EFFORT_SETTLE));
}

/// The resolver's memo: one read per build, due again once it has stood ten seconds; a build
/// no sweep has cached is held as read, with no catalog.
#[test]
fn a_build_s_catalog_is_read_once_and_again_after_ten_seconds() {
    let newer = baked(&[("claude-opus-6", "Opus 6", true, ALL)]);
    let older = baked(&[("claude-opus-5", "Opus 5", false, &ALL[..3])]);
    let mut catalogs = Catalogs::default();
    let start = std::time::Instant::now();
    let later = |s| start + std::time::Duration::from_secs(s);
    assert!(catalogs.due(Some("2.1.400"), start) && catalogs.get(Some("2.1.400")).is_none());
    catalogs.hold(Some("2.1.400"), start, Some(newer.clone()));
    assert!(!catalogs.due(Some("2.1.400"), later(9)), "a fresh read");
    assert!(
        catalogs.due(Some("2.1.300"), later(9)),
        "another build's is its own"
    );
    catalogs.hold(Some("2.1.300"), later(9), Some(older.clone()));
    assert_eq!(catalogs.get(Some("2.1.400")).as_deref(), Some(&newer));
    assert_eq!(catalogs.get(Some("2.1.300")).as_deref(), Some(&older));
    assert!(catalogs.get(None).is_none(), "a build not read");
    assert!(catalogs.due(Some("2.1.400"), later(10)));
    catalogs.hold(Some("2.1.400"), later(10), None);
    assert!(
        catalogs.get(Some("2.1.400")).is_none(),
        "a build no sweep cached"
    );
    assert!(
        !catalogs.due(Some("2.1.400"), later(11)),
        "and that is a read"
    );
}
