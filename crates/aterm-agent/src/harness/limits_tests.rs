// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the limit classifier, its table and its engine (design §5.8).
//! Every clock value is a literal; nothing here reads the wall clock, the
//! environment or a file.
//!
//! The engine's bounded model is NOT written here: it lives once, as
//! `aterm_spec::derive::harness_failure_recovery_model` (§11 item 7), proved
//! at Tier 0 in that crate and bound to this engine in
//! `tests/conformance_harness.rs`.

use std::collections::BTreeMap;

use super::*;
use crate::harness::source::Source;

const NOW: i64 = 1_789_660_000;
const GEN: u64 = 25;

// -- fixtures -----------------------------------------------------------------

fn sf(error: &str) -> Evidence {
    Evidence::StopFailure {
        error: error.into(),
        details: None,
    }
}

fn sfd(error: &str, details: &str) -> Evidence {
    Evidence::StopFailure {
        error: error.into(),
        details: Some(details.into()),
    }
}

fn win(which: WindowKind, pct: f64, resets_at: Option<i64>, source: Source) -> Evidence {
    Evidence::Window {
        which,
        used_pct: Some(pct),
        resets_at,
        source,
        age_s: Some(8),
    }
}

fn notif(kind: &str) -> Evidence {
    Evidence::Notification { kind: kind.into() }
}

fn pms() -> Evidence {
    Evidence::PostModelSwitch {
        from: "claude-fable-5-1".into(),
        to: "claude-sonnet-4-5".into(),
        source: None,
    }
}

fn banner(text: &str) -> Evidence {
    Evidence::Banner {
        text: text.into(),
        resets_at: None,
    }
}

fn cls(evidence: &[Evidence]) -> Classification {
    classify(evidence, NOW).unwrap_or_else(|| panic!("no classification for {evidence:?}"))
}

/// A hand-written classification: the engine is table-driven over `State`, so
/// the pair rule is tested on the classifier and the ladders on states of
/// both kinds.
fn classification(class: Class, unpaired: bool, resets_at: Option<i64>) -> Classification {
    Classification {
        class,
        unpaired,
        reasons: Vec::new(),
        resets_at,
        storm: false,
        no_response: false,
    }
}

/// A state built from [`classification`].
fn state(class: Class, unpaired: bool, resets_at: Option<i64>) -> State {
    State::new(
        &classification(class, unpaired, resets_at),
        NOW,
        GEN,
        Carry::default(),
    )
}

fn guards(level: u8, accounts_enabled: bool) -> Guards {
    let mut g = Guards::from_config(&LimitsConfig::default(), accounts_enabled);
    g.level = level;
    g
}

fn action_of(s: &Step) -> Option<(Action, u8)> {
    match &s.decision {
        Decision::Act { action, level, .. } => Some((*action, *level)),
        _ => None,
    }
}

fn skipped_names(s: &Step) -> Vec<Action> {
    s.skipped.iter().map(|k| k.action).collect()
}

fn zone_none(_: &str) -> Option<i64> {
    None
}

// -- the classifier: the closed list -------------------------------------------

#[test]
fn every_closed_list_value_maps_to_its_class() {
    // A readable window below the threshold so `rate_limit` is a throttle.
    let below = win(
        WindowKind::FiveHour,
        40.0,
        Some(NOW + 3600),
        Source::StatusLine,
    );
    let table: [(&str, Class); 13] = [
        ("rate_limit", Class::TransientCapacity),
        ("overloaded", Class::TransientCapacity),
        ("server_error", Class::TransientCapacity),
        ("authentication_failed", Class::Auth),
        ("cloud_credential_error", Class::Auth),
        ("oauth_org_not_allowed", Class::SpendBilling),
        ("account_on_hold", Class::SpendBilling),
        ("verification_required", Class::SpendBilling),
        ("billing_error", Class::SpendBilling),
        ("invalid_request", Class::Unknown),
        ("model_not_found", Class::Unknown),
        ("max_output_tokens", Class::Unknown),
        ("unknown", Class::Unknown),
    ];
    assert_eq!(table.len(), STOP_FAILURE_ERRORS.len());
    for (error, expected) in table {
        assert!(STOP_FAILURE_ERRORS.contains(&error));
        let c = cls(&[sf(error), below.clone()]);
        assert_eq!(c.class, expected, "{error}: {:?}", c.reasons);
        if expected == Class::Unknown {
            assert!(c.unpaired, "{error}: unknown is escalate-only");
        }
    }
}

#[test]
fn a_value_outside_the_closed_list_forces_unknown_whatever_agrees() {
    // Everything else says "five-hour limit"; the renamed hook wins, closed.
    let c = cls(&[
        sf("rate_limit_exceeded"),
        win(
            WindowKind::FiveHour,
            100.0,
            Some(NOW + 1800),
            Source::StatusLine,
        ),
        notif("quota_auto_resume_armed"),
        banner("You've hit your session limit · resets 7:30pm"),
    ]);
    assert_eq!(c.class, Class::Unknown);
    assert!(c.unpaired);
    assert_eq!(c.resets_at, None, "a fail-closed verdict carries no reset");
    assert!(
        c.reasons[0].contains("outside the closed list"),
        "{:?}",
        c.reasons
    );
    // Even when it is the FIRST of several and a good one follows.
    let c = cls(&[sf("rate_limit_v2"), sf("overloaded")]);
    assert_eq!(c.class, Class::Unknown);
}

// -- the classifier: rate_limit and the readable windows ----------------------

#[test]
fn rate_limit_with_no_readable_window_is_unknown_never_transient() {
    for evidence in [
        vec![sf("rate_limit")],
        vec![
            sf("rate_limit"),
            win(WindowKind::FiveHour, 100.0, Some(NOW + 60), Source::None),
        ],
        vec![
            sf("rate_limit"),
            win(
                WindowKind::FiveHour,
                100.0,
                Some(NOW + 60),
                Source::Transcript,
            ),
        ],
        vec![
            sf("rate_limit"),
            Evidence::Window {
                which: WindowKind::FiveHour,
                used_pct: None,
                resets_at: Some(NOW + 60),
                source: Source::StatusLine,
                age_s: None,
            },
        ],
        // A window whose reset has passed is not readable either.
        vec![
            sf("rate_limit"),
            win(
                WindowKind::FiveHour,
                100.0,
                Some(NOW - 1),
                Source::StatusLine,
            ),
        ],
        // A banner does not make a window.
        vec![
            sf("rate_limit"),
            banner("Server is temporarily limiting requests"),
        ],
    ] {
        let c = cls(&evidence);
        assert_eq!(c.class, Class::Unknown, "{:?}", c.reasons);
        assert!(c.unpaired);
    }
}

#[test]
fn rate_limit_with_every_readable_window_below_95_is_a_throttle() {
    let c = cls(&[
        sf("rate_limit"),
        win(
            WindowKind::FiveHour,
            94.9,
            Some(NOW + 3600),
            Source::StatusLine,
        ),
        win(
            WindowKind::SevenDay,
            60.0,
            Some(NOW + 86_400),
            Source::Cache,
        ),
    ]);
    assert_eq!(c.class, Class::TransientCapacity);
    assert!(!c.unpaired, "hook + window is a pair: {:?}", c.reasons);
}

#[test]
fn rate_limit_with_a_readable_window_at_95_is_the_limit_class() {
    let five = cls(&[
        sf("rate_limit"),
        win(
            WindowKind::FiveHour,
            95.0,
            Some(NOW + 3600),
            Source::StatusLine,
        ),
    ]);
    assert_eq!(five.class, Class::Session5hLimit);
    assert!(!five.unpaired);
    assert_eq!(five.resets_at, Some(NOW + 3600));

    let weekly = cls(&[
        sf("rate_limit"),
        win(
            WindowKind::SevenDay,
            100.0,
            Some(NOW + 200_000),
            Source::Cache,
        ),
    ]);
    assert_eq!(weekly.class, Class::Weekly7dLimit);
    assert!(!weekly.unpaired);

    // A cache window is readable too; a per-model seven-day window is weekly.
    let opus = cls(&[
        sf("rate_limit"),
        win(
            WindowKind::SevenDayOpus,
            99.0,
            Some(NOW + 200_000),
            Source::Cache,
        ),
    ]);
    assert_eq!(opus.class, Class::Weekly7dLimit);
}

#[test]
fn weekly_wins_over_five_hour_and_spend_over_both() {
    let c = cls(&[
        sf("rate_limit"),
        win(
            WindowKind::FiveHour,
            100.0,
            Some(NOW + 1800),
            Source::StatusLine,
        ),
        win(
            WindowKind::SevenDay,
            100.0,
            Some(NOW + 300_000),
            Source::StatusLine,
        ),
    ]);
    assert_eq!(c.class, Class::Weekly7dLimit);
    assert_eq!(c.resets_at, Some(NOW + 300_000));
    let c = cls(&[
        sf("rate_limit"),
        win(
            WindowKind::FiveHour,
            100.0,
            Some(NOW + 1800),
            Source::StatusLine,
        ),
        win(
            WindowKind::SpendLimit,
            140.0,
            Some(NOW + 500_000),
            Source::StatusLine,
        ),
    ]);
    assert_eq!(c.class, Class::SpendBilling);
}

#[test]
fn billing_error_with_the_spend_window_is_a_pair_and_auth_never_pairs_with_a_window() {
    let spend = cls(&[
        sf("billing_error"),
        win(
            WindowKind::SpendLimit,
            140.0,
            Some(NOW + 500_000),
            Source::StatusLine,
        ),
    ]);
    assert_eq!(spend.class, Class::SpendBilling);
    assert!(!spend.unpaired);

    let auth = cls(&[
        sf("authentication_failed"),
        win(
            WindowKind::FiveHour,
            10.0,
            Some(NOW + 3600),
            Source::StatusLine,
        ),
    ]);
    assert_eq!(auth.class, Class::Auth);
    assert!(auth.unpaired, "a window says nothing about a login");
}

#[test]
fn the_fable_marker_in_error_details_is_the_model_bucket() {
    let c = cls(&[
        sfd(
            "rate_limit",
            r#"{"apiError":"model_requires_usage_credits"}"#,
        ),
        win(
            WindowKind::FiveHour,
            100.0,
            Some(NOW + 1800),
            Source::StatusLine,
        ),
        pms(),
    ]);
    assert_eq!(c.class, Class::ModelBucketLimit, "{:?}", c.reasons);
    assert!(!c.unpaired, "hook + PostModelSwitch is a pair");
    // The exhausted five-hour window does not corroborate the Fable class.
    let c = cls(&[
        sfd(
            "rate_limit",
            r#"{"apiError":"model_requires_usage_credits"}"#,
        ),
        win(
            WindowKind::FiveHour,
            100.0,
            Some(NOW + 1800),
            Source::StatusLine,
        ),
    ]);
    assert_eq!(c.class, Class::ModelBucketLimit);
    assert!(c.unpaired);
}

#[test]
fn unknown_with_a_connection_error_is_network_offline() {
    for details in [
        "network_error",
        "Network error. Please check your internet connection.",
        "read ECONNRESET",
        "getaddrinfo ENOTFOUND api.example",
        "No response from the API after 30000ms",
    ] {
        let c = cls(&[sfd("unknown", details)]);
        assert_eq!(c.class, Class::NetworkOffline, "{details}");
        assert!(c.unpaired);
    }
    assert_eq!(
        cls(&[sfd("unknown", "something else")]).class,
        Class::Unknown
    );
    let c = cls(&[sfd("unknown", "No response from the API after 30000ms")]);
    assert!(c.no_response);
}

// -- the classifier: the pair rule ---------------------------------------------

/// REWRITTEN 2026-09-22 with the two-source rule's narrowing (§5.8.2 under
/// §0.2). It used to be `a_banner_completes_a_pair_for_auth_and_for_no_other_class`
/// and it asserted the inversion: that a banner beside an exhausted window
/// was "L1 only", and that `Auth` was the ONE class a banner could pair for.
/// The grid is rank 1 now, so a banner names a class and pairs with any
/// channel that is not the same frame — which is exactly the design's first
/// listed independent pair, "a rank-1 grid reading plus a window".
#[test]
fn a_banner_pairs_with_any_channel_that_is_not_the_same_grid_frame() {
    let c = cls(&[
        win(
            WindowKind::FiveHour,
            100.0,
            Some(NOW + 1800),
            Source::StatusLine,
        ),
        banner("You've hit your session limit · resets 7:30pm (America/Los_Angeles)"),
    ]);
    assert_eq!(c.class, Class::Session5hLimit);
    assert!(
        !c.unpaired,
        "grid + a window the vendor computed is the design's first pair: {:?}",
        c.reasons
    );

    let c = cls(&[sf("overloaded"), banner("Repeated 529 Overloaded errors")]);
    assert_eq!(c.class, Class::TransientCapacity);
    assert!(!c.unpaired, "hook value + grid: {:?}", c.reasons);
    assert!(c.storm, "the 529 banner still makes a storm");

    let c = cls(&[sf("authentication_failed"), banner("Please run /login")]);
    assert_eq!(c.class, Class::Auth);
    assert!(
        !c.unpaired,
        "auth hook + auth banner pairs: {:?}",
        c.reasons
    );

    // THE ONE COLLAPSE, and the reason the `Grid` source had to exist: two
    // readings of the SAME frame are one source. The banner and a window
    // figure read back off the screen both land on `grid`, so together they
    // are still one channel and the spending actions stay out of reach.
    let one_frame = cls(&[
        win(WindowKind::FiveHour, 100.0, Some(NOW + 1800), Source::Grid),
        banner("You've hit your session limit · resets 7:30pm"),
    ]);
    assert_eq!(one_frame.class, Class::Session5hLimit);
    assert!(
        one_frame.unpaired,
        "banner + grid window is ONE source: {:?}",
        one_frame.reasons
    );
    assert!(
        one_frame
            .reasons
            .iter()
            .any(|r| r == "one source (grid)" || r.starts_with("one source (grid)")),
        "{:?}",
        one_frame.reasons
    );

    // The negative controls. Each of these is one source: real evidence that
    // classifies and runs the reversible ladder, and never a pair.
    let alone = cls(&[banner("Please run /login")]);
    assert_eq!(alone.class, Class::Auth);
    assert!(alone.unpaired, "a banner alone: {:?}", alone.reasons);
    let hook_only = cls(&[sf("authentication_failed")]);
    assert_eq!(hook_only.class, Class::Auth);
    assert!(hook_only.unpaired, "{:?}", hook_only.reasons);
    // And a banner that is NOT an auth banner cannot pair with an auth hook:
    // the corroboration has to be about the same thing.
    let mismatch = cls(&[
        sf("authentication_failed"),
        banner("Repeated 529 Overloaded errors"),
    ]);
    assert_eq!(mismatch.class, Class::Auth);
    assert!(mismatch.unpaired, "{:?}", mismatch.reasons);
}

/// The window the vendor PAINTED is readable where the vendor put it
/// (§5.8.2, amended 2026-09-19): a renamed statusLine field alone no longer
/// forces `unknown`, because `source=grid` is a readable window too.
#[test]
fn a_grid_window_is_readable_and_a_transcript_window_is_not() {
    let grid = cls(&[
        sf("rate_limit"),
        win(WindowKind::FiveHour, 100.0, Some(NOW + 1800), Source::Grid),
    ]);
    assert_eq!(
        grid.class,
        Class::Session5hLimit,
        "a painted window decides `rate_limit`: {:?}",
        grid.reasons
    );
    assert_eq!(grid.resets_at, Some(NOW + 1800));
    assert!(!grid.unpaired, "hook value + grid: {:?}", grid.reasons);

    // Below the threshold it is a throttle, exactly as a statusLine sample
    // would be — the source decides admissibility, never the verdict.
    let throttle = cls(&[
        sf("rate_limit"),
        win(WindowKind::FiveHour, 40.0, Some(NOW + 1800), Source::Grid),
    ]);
    assert_eq!(throttle.class, Class::TransientCapacity);

    // The transcript is NOT readable and never became so: its shape is the
    // vendor's and UNVERIFIED, and its error row is the same vendor event
    // the hook carried.
    let transcript = cls(&[
        sf("rate_limit"),
        win(
            WindowKind::FiveHour,
            100.0,
            Some(NOW + 1800),
            Source::Transcript,
        ),
    ]);
    assert_eq!(transcript.class, Class::Unknown);
}

#[test]
fn a_hook_value_and_a_window_make_a_pair_and_a_window_alone_does_not() {
    let alone = cls(&[win(
        WindowKind::FiveHour,
        100.0,
        Some(NOW + 1800),
        Source::StatusLine,
    )]);
    assert_eq!(alone.class, Class::Session5hLimit);
    assert!(alone.unpaired);

    let with_notification = cls(&[
        notif("quota_auto_resume_fired"),
        win(
            WindowKind::FiveHour,
            100.0,
            Some(NOW + 1800),
            Source::StatusLine,
        ),
    ]);
    assert_eq!(with_notification.class, Class::Session5hLimit);
    assert!(
        !with_notification.unpaired,
        "{:?}",
        with_notification.reasons
    );

    // The notification follows the exhausted window to weekly.
    let weekly = cls(&[
        notif("quota_auto_resume_armed"),
        win(
            WindowKind::SevenDay,
            100.0,
            Some(NOW + 300_000),
            Source::Cache,
        ),
    ]);
    assert_eq!(weekly.class, Class::Weekly7dLimit);
    assert!(!weekly.unpaired);
}

/// Two CORROBORATORS are not a pair. The rule that survived the 2026-09-19
/// narrowing is "one channel must NAME the failure"; what changed is which
/// channels can be the namer — the grid joined `StopFailure` and
/// `Notification`, and a `PostModelSwitch` and a window figure did not,
/// because a switch names no failure (`classify` answers `None` for one
/// alone) and a percentage is not a verdict.
#[test]
fn post_model_switch_pairs_only_with_a_channel_that_names_the_failure() {
    let no_namer = cls(&[
        win(
            WindowKind::FiveHour,
            100.0,
            Some(NOW + 1800),
            Source::StatusLine,
        ),
        pms(),
    ]);
    assert_eq!(no_namer.class, Class::Session5hLimit);
    assert!(
        no_namer.unpaired,
        "window + PostModelSwitch is two corroborators: {:?}",
        no_namer.reasons
    );

    let with_hook = cls(&[sf("overloaded"), pms()]);
    assert_eq!(with_hook.class, Class::TransientCapacity);
    assert!(!with_hook.unpaired);

    // And with the grid as the namer, in a session that has no hooks at all.
    let with_grid = cls(&[
        banner("You've hit your session limit · resets 7:30pm"),
        pms(),
    ]);
    assert_eq!(with_grid.class, Class::Session5hLimit);
    assert!(!with_grid.unpaired, "{:?}", with_grid.reasons);

    // A switch does not corroborate an auth or billing failure.
    let auth = cls(&[sf("authentication_failed"), pms()]);
    assert!(auth.unpaired);
}

#[test]
fn a_notification_alone_or_with_a_hook_but_no_window_stays_safe() {
    let alone = cls(&[notif("quota_auto_resume_armed")]);
    assert_eq!(alone.class, Class::Session5hLimit);
    assert!(alone.unpaired);
    // StopFailure + Notification would be a pair, but `rate_limit` with no
    // readable window is `unknown` first: fail closed wins.
    let no_window = cls(&[sf("rate_limit"), notif("quota_auto_resume_armed")]);
    assert_eq!(no_window.class, Class::Unknown);
    assert!(no_window.unpaired);
    // Other notification kinds are not evidence of a limit.
    assert_eq!(classify(&[notif("permission_prompt")], NOW), None);
}

#[test]
fn empty_or_switch_only_evidence_classifies_nothing() {
    assert_eq!(classify(&[], NOW), None);
    assert_eq!(classify(&[pms()], NOW), None);
    assert_eq!(classify(&[pms(), notif("idle_prompt")], NOW), None);
}

// -- THE ZERO-HOOK SESSION: the narrowed two-source rule, end to end ---------------
//
// Added 2026-09-22 with the narrowing (design §5.8.2 under §0.2). The case is
// not hypothetical: it is a `--bare` launch ("skip hooks, LSP, plugin sync"),
// one renamed vendor event, a user `settings.json` that wins the statusLine
// slot, and every already-running session the host adopts. The only evidence
// such a session has is what aterm itself drew, and under the pre-inversion
// rule that meant it could name a class and then never act on it, for ever.

/// The grid-only banner every test in this block reads: one weekly limit,
/// with the reset the vendor printed beside it.
fn weekly_banner() -> Evidence {
    Evidence::Banner {
        text: "You have reached your weekly usage limit".into(),
        resets_at: Some(NOW + 3 * 86_400),
    }
}

#[test]
fn the_two_source_rule_binds_exactly_the_four_actions_that_spend() {
    let bound: Vec<Action> = Action::ALL
        .into_iter()
        .filter(|a| a.needs_two_sources())
        .collect();
    assert_eq!(
        bound,
        [
            Action::SwitchAccount,
            Action::LowerPriority,
            Action::LimitReset,
            Action::ExtraUsage
        ],
        "money, an allowance, an account — and nothing else"
    );
    for free in [
        Action::LetVendorRetry,
        Action::Retry,
        Action::Wait,
        Action::SwitchModel,
        Action::Relogin,
        Action::Escalate,
    ] {
        assert!(!free.needs_two_sources(), "{free} is reversible");
    }
}

#[test]
fn a_zero_hook_session_reaches_switch_model_wait_and_escalate_from_the_grid_alone() {
    let cfg = LimitsConfig::default();
    let g = guards(4, true);
    let c = cls(&[weekly_banner()]);
    assert_eq!(c.class, Class::Weekly7dLimit);
    assert!(
        c.unpaired,
        "one source, and it is the grid: {:?}",
        c.reasons
    );
    assert_eq!(
        c.resets_at,
        Some(NOW + 3 * 86_400),
        "the reset the vendor printed is the one the ladder waits on"
    );

    // The weekly row is [switch-account, switch-model, wait, escalate].
    let mut s = State::new(&c, NOW, GEN, Carry::default());
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(
        action_of(&d),
        Some((Action::SwitchModel, 3)),
        "an L3 model switch runs on rank-1 evidence: {:?}",
        d.decision
    );
    assert_eq!(skipped_names(&d), [Action::SwitchAccount]);
    s.commit(&d, 1, NOW);
    s.verdict(Verdict::Executed, NOW + 1);

    let d = step(&cfg, &s, &g, NOW + 2, GEN);
    assert_eq!(action_of(&d), Some((Action::Wait, 1)));
    assert_eq!(d.wait_until, Some(NOW + 3 * 86_400 + RESET_JITTER_S));
    s.commit(&d, 2, NOW + 2);

    let d = step(&cfg, &s, &g, NOW + 3, GEN);
    assert_eq!(
        action_of(&d),
        Some((Action::Escalate, 1)),
        "a weekly reset is days out: the human is told now"
    );

    // The same, from the OTHER rank-1 shape: a window figure read off the
    // frame the vendor painted, with no banner at all.
    let painted = cls(&[win(
        WindowKind::SevenDay,
        100.0,
        Some(NOW + 3 * 86_400),
        Source::Grid,
    )]);
    assert_eq!(painted.class, Class::Weekly7dLimit);
    assert!(painted.unpaired);
    assert_eq!(
        action_of(&step(
            &cfg,
            &State::new(&painted, NOW, GEN, Carry::default()),
            &g,
            NOW,
            GEN
        )),
        Some((Action::SwitchModel, 3))
    );
}

#[test]
fn the_same_zero_hook_session_may_not_switch_account_lower_priority_limit_reset_or_spend() {
    let mut cfg = LimitsConfig {
        allow_low_priority: true,
        allow_limit_reset: true,
        allow_spend: true,
        ..LimitsConfig::default()
    };
    cfg.actions
        .set(
            Class::Weekly7dLimit,
            vec![
                Action::SwitchAccount,
                Action::LowerPriority,
                Action::LimitReset,
                Action::ExtraUsage,
                Action::Escalate,
            ],
        )
        .expect("the weekly row admits all four");
    let mut g = Guards::from_config(&cfg, true);
    g.level = 4;

    let c = cls(&[weekly_banner()]);
    let s = State::new(&c, NOW, GEN, Carry::default());
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(
        action_of(&d),
        Some((Action::Escalate, 1)),
        "every spending candidate is refused; a human is told instead"
    );
    assert_eq!(
        skipped_names(&d),
        [
            Action::SwitchAccount,
            Action::LowerPriority,
            Action::LimitReset,
            Action::ExtraUsage
        ]
    );
    for k in &d.skipped[..3] {
        assert!(
            k.why.contains("one source") && k.why.contains("needs two"),
            "{}: {}",
            k.action,
            k.why
        );
    }
    assert!(
        d.skipped[3].why.contains("unreachable in ABI 1"),
        "extra-usage is refused BEFORE the two-source rule is asked, which is \
         stronger, not weaker: {}",
        d.skipped[3].why
    );

    // NEGATIVE CONTROL: the same grid banner, now beside a window the vendor
    // COMPUTED — the design's first listed independent pair. The row that
    // refused every step above runs its first one.
    let pair = cls(&[
        weekly_banner(),
        win(
            WindowKind::SevenDay,
            100.0,
            Some(NOW + 3 * 86_400),
            Source::Cache,
        ),
    ]);
    assert_eq!(pair.class, Class::Weekly7dLimit);
    assert!(!pair.unpaired, "{:?}", pair.reasons);
    assert_eq!(
        action_of(&step(
            &cfg,
            &State::new(&pair, NOW, GEN, Carry::default()),
            &g,
            NOW,
            GEN
        )),
        Some((Action::SwitchAccount, 4))
    );
}

#[test]
fn a_renamed_stop_failure_value_forces_unknown_with_the_whole_grid_agreeing() {
    // Fail-closed survived the narrowing untouched: the closed list is read
    // FIRST, before any window or banner, so rank-1 evidence never rescues a
    // hook value nobody can place.
    let c = cls(&[
        sf("rate_limit_v2"),
        weekly_banner(),
        win(
            WindowKind::SevenDay,
            100.0,
            Some(NOW + 3 * 86_400),
            Source::Grid,
        ),
        win(
            WindowKind::FiveHour,
            100.0,
            Some(NOW + 1800),
            Source::StatusLine,
        ),
    ]);
    assert_eq!(c.class, Class::Unknown);
    assert!(c.unpaired, "unknown is never a pair");
    assert_eq!(c.resets_at, None, "a fail-closed verdict carries no reset");
    assert!(
        c.reasons[0].contains("outside the closed list"),
        "{:?}",
        c.reasons
    );

    // And in the engine: a live grid-only class that meets the renamed value
    // moves to `unknown`, whose row is `["escalate"]`.
    let cfg = LimitsConfig::default();
    let g = guards(4, true);
    let mut s = State::new(&cls(&[weekly_banner()]), NOW, GEN, Carry::default());
    s.observe(&Event::Evidence(sf("rate_limit_v2")), NOW + 1);
    assert_eq!(s.class, Class::Unknown);
    assert!(s.unpaired);
    assert_eq!(cfg.actions.get(Class::Unknown), [Action::Escalate]);
    assert_eq!(s.step, 0, "the new row is walked from its beginning");
    let window = i64::try_from(cfg.unknown_escalate_after.window_s).expect("small");
    assert_eq!(
        step(&cfg, &s, &g, NOW + 2, GEN).decision,
        Decision::Wait {
            until: NOW + 1 + window
        }
    );
    assert_eq!(
        action_of(&step(&cfg, &s, &g, NOW + 1 + window, GEN)),
        Some((Action::Escalate, 1)),
        "a value nobody can place still reaches a human"
    );
}

#[test]
fn auth_and_spend_billing_keep_their_pins_when_the_grid_is_the_only_source() {
    let cfg = LimitsConfig::default();
    let g = guards(4, true);

    // Money: one step, and it is `escalate`.
    let spend = cls(&[banner("You've hit your monthly spend limit")]);
    assert_eq!(spend.class, Class::SpendBilling);
    assert!(spend.unpaired);
    assert_eq!(cfg.actions.get(Class::SpendBilling), [Action::Escalate]);
    let s = State::new(&spend, NOW, GEN, Carry::default());
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::Escalate, 1)));
    assert!(d.skipped.is_empty());
    let mut t = ActionTable::default();
    assert!(
        matches!(
            t.set(Class::SpendBilling, vec![Action::Retry, Action::Escalate]),
            Err(TableError::Pinned { .. })
        ),
        "rank-1 evidence does not widen a pinned row"
    );

    // A login: the vendor's own door, then the human — never a switch, never
    // a wait, and never a relaunch into a different account.
    let auth = cls(&[banner("Please run /login")]);
    assert_eq!(auth.class, Class::Auth);
    assert!(auth.unpaired);
    assert_eq!(
        cfg.actions.get(Class::Auth),
        [Action::Relogin, Action::Escalate]
    );
    let mut s = State::new(&auth, NOW, GEN, Carry::default());
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::Relogin, 3)));
    s.commit(&d, 1, NOW);
    s.verdict(Verdict::Executed, NOW + 1);
    let wait_s = i64::try_from(cfg.relogin_wait_s).expect("small");
    assert_eq!(
        step(&cfg, &s, &g, NOW + 2, GEN).decision,
        Decision::Wait {
            until: NOW + 1 + wait_s
        },
        "the human gets relogin_wait_s to walk through the door"
    );
    assert_eq!(
        action_of(&step(&cfg, &s, &g, NOW + 1 + wait_s, GEN)),
        Some((Action::Escalate, 1))
    );

    // The Fable bucket is the vendor's end to end: rank-1 evidence still
    // never reaches a switch there, and the table refuses to give it one.
    let fable = cls(&[banner("You've reached your Fable limit")]);
    assert_eq!(fable.class, Class::ModelBucketLimit);
    assert_eq!(
        cfg.actions.get(Class::ModelBucketLimit),
        [Action::LetVendorRetry, Action::Escalate]
    );
    for class in [
        Class::ModelBucketLimit,
        Class::NetworkOffline,
        Class::Unknown,
    ] {
        assert!(matches!(
            ActionTable::default().set(class, vec![Action::SwitchModel, Action::Escalate]),
            Err(TableError::SwitchNotAllowed { .. })
        ));
    }
}

// -- the classifier: banners, resets, storms -------------------------------------

#[test]
fn banner_words_map_to_classes_and_a_stray_notice_is_unknown() {
    let table: [(&str, Option<Class>); 12] = [
        (
            "You've hit your session limit · resets 7:30pm",
            Some(Class::Session5hLimit),
        ),
        (
            "5-hour limit reached ∙ resets 3am",
            Some(Class::Session5hLimit),
        ),
        (
            "⚠ Usage limit reached · continuing automatically at 1:50pm",
            Some(Class::Session5hLimit),
        ),
        (
            "You've reached your weekly usage limit",
            Some(Class::Weekly7dLimit),
        ),
        (
            "You've reached your Fable limit. Run /usage-credits to continue",
            Some(Class::ModelBucketLimit),
        ),
        (
            "Repeated 529 Overloaded errors",
            Some(Class::TransientCapacity),
        ),
        (
            "Fable is experiencing high load, please use /model to switch to Sonnet",
            Some(Class::TransientCapacity),
        ),
        (
            "API Error: 401 Invalid API key · Please run /login",
            Some(Class::Auth),
        ),
        ("Anthropic profile login expired", Some(Class::Auth)),
        (
            "Network error. Please check your internet connection.",
            Some(Class::NetworkOffline),
        ),
        (
            "You've hit your monthly spend limit",
            Some(Class::SpendBilling),
        ),
        // aterm-phase's wall table reads a rate-limit API error with no
        // status as the vendor's retryable kind — as the supervisor and
        // `status agent=` do (one table, the laws review of 2026-09-24).
        (
            "API Error: Rate limit reached for requests",
            Some(Class::TransientCapacity),
        ),
    ];
    for (text, expected) in table {
        assert_eq!(banner_class(text), expected, "{text}");
    }
    let c = cls(&[banner("You've hit an unfamiliar ceiling today")]);
    assert_eq!(c.class, Class::Unknown);
    assert!(c.unpaired);
}

/// The laws review of 2026-09-24: `harness limits` classified banners with
/// its own phrase table, which disagreed with aterm-phase's on real notices
/// — `Out of usage credits` (money, not a model bucket) and `You've hit your
/// limit · resets 3pm` (the session window, not nothing). Every notice the
/// wall table places is now its kind, whatever this module's needles say.
/// NEGATIVE CONTROL: a full context is no limit class.
#[test]
fn a_notice_the_wall_table_places_is_classed_by_it() {
    for (text, class) in [
        ("Out of usage credits", Class::SpendBilling),
        ("You've hit your limit · resets 3pm", Class::Session5hLimit),
    ] {
        let wall = aterm_phase::classify_wall(text).expect("the wall table places it");
        assert_eq!(banner_class(text), Some(class), "{text}: {wall:?}");
    }
    assert_eq!(
        banner_class("Context limit reached · /compact or /clear to continue"),
        None
    );
}

#[test]
fn resets_at_prefers_the_statusline_over_the_cache_over_the_banner() {
    let both = cls(&[
        sf("rate_limit"),
        win(WindowKind::FiveHour, 100.0, Some(NOW + 1000), Source::Cache),
        win(
            WindowKind::FiveHour,
            100.0,
            Some(NOW + 1800),
            Source::StatusLine,
        ),
        Evidence::Banner {
            text: "You've hit your session limit · resets 7:30pm".into(),
            resets_at: Some(NOW + 9999),
        },
    ]);
    assert_eq!(both.resets_at, Some(NOW + 1800));
    let cache_only = cls(&[
        sf("rate_limit"),
        win(WindowKind::FiveHour, 100.0, Some(NOW + 1000), Source::Cache),
    ]);
    assert_eq!(cache_only.resets_at, Some(NOW + 1000));
    let banner_only = cls(&[
        win(WindowKind::FiveHour, 100.0, None, Source::StatusLine),
        Evidence::Banner {
            text: "You've hit your session limit · resets 7:30pm".into(),
            resets_at: Some(NOW + 9999),
        },
    ]);
    assert_eq!(banner_only.resets_at, Some(NOW + 9999));
    // A window of another class does not lend its reset.
    let other = cls(&[
        sf("overloaded"),
        win(
            WindowKind::FiveHour,
            30.0,
            Some(NOW + 1800),
            Source::StatusLine,
        ),
    ]);
    assert_eq!(other.class, Class::TransientCapacity);
    assert_eq!(other.resets_at, None);
}

#[test]
fn a_storm_is_three_overloaded_failures_or_the_529_banner() {
    let two = cls(&[sf("overloaded"), sf("overloaded"), pms()]);
    assert!(!two.storm);
    let three = cls(&[sf("overloaded"), sf("overloaded"), sf("overloaded"), pms()]);
    assert!(three.storm);
    assert!(!three.unpaired);
    let banner_storm = cls(&[
        sf("overloaded"),
        banner("Repeated 529 Overloaded errors"),
        pms(),
    ]);
    assert!(banner_storm.storm);
    // Three server errors are not three overloads.
    let server = cls(&[sf("server_error"), sf("server_error"), sf("server_error")]);
    assert!(!server.storm);
}

#[test]
fn banner_from_screen_reads_limit_notice_and_places_the_reset() {
    use aterm_phase::prompt::fixtures::{composer, rows};
    let mut screen = rows(&[
        "❯ carry on with the parser",
        "",
        "⏺ Working through it.",
        "  ⎿  You've hit your session limit · resets in 3h",
        "",
    ]);
    screen.extend(composer("  ? for shortcuts"));
    let e = Evidence::banner_from_screen(&screen, NOW, 0, zone_none)
        .unwrap_or_else(|| panic!("no notice read from {screen:?}"));
    match &e {
        Evidence::Banner { text, resets_at } => {
            assert!(text.contains("session limit"), "{text}");
            assert_eq!(*resets_at, Some(NOW + 3 * 3600));
        }
        other => panic!("{other:?}"),
    }
    let c = cls(&[e]);
    assert_eq!(c.class, Class::Session5hLimit);
    assert!(c.unpaired);

    // No notice on the screen: nothing.
    let mut idle = rows(&["⏺ Merged and pushed.", ""]);
    idle.extend(composer("  ? for shortcuts"));
    assert_eq!(Evidence::banner_from_screen(&idle, NOW, 0, zone_none), None);
}

#[test]
fn banner_reset_at_is_the_supervisor_clock() {
    assert_eq!(
        banner_reset_at("in 45m", NOW, 0, zone_none),
        Some(NOW + 45 * 60)
    );
    assert_eq!(
        banner_reset_at("shortly", NOW, 0, zone_none),
        Some(NOW + 60)
    );
    assert_eq!(banner_reset_at("whenever", NOW, 0, zone_none), None);
    assert_eq!(banner_reset_at("", NOW, 0, zone_none), None);
}

// -- the table -------------------------------------------------------------------

#[test]
fn the_default_table_is_the_design_table_and_validates() {
    let t = ActionTable::default();
    assert_eq!(t.validate(), Ok(()));
    let expect: [(Class, &[&str]); 8] = [
        (
            Class::TransientCapacity,
            &[
                "let-vendor-retry",
                "switch-model",
                "wait",
                "retry",
                "escalate",
            ],
        ),
        (
            Class::NetworkOffline,
            &["let-vendor-retry", "wait", "retry", "escalate"],
        ),
        (
            Class::Session5hLimit,
            &[
                "switch-account",
                "switch-model",
                "wait",
                "retry",
                "escalate",
            ],
        ),
        (
            Class::Weekly7dLimit,
            &["switch-account", "switch-model", "wait", "escalate"],
        ),
        (Class::ModelBucketLimit, &["let-vendor-retry", "escalate"]),
        (Class::SpendBilling, &["escalate"]),
        (Class::Auth, &["relogin", "escalate"]),
        (Class::Unknown, &["escalate"]),
    ];
    for (class, names) in expect {
        let got: Vec<&str> = t.get(class).iter().map(|a| a.as_str()).collect();
        assert_eq!(got, names, "{class}");
    }
    for a in Action::ALL {
        assert_eq!(Action::parse(a.as_str()), Some(a));
    }
    for c in Class::ALL {
        assert_eq!(Class::parse(c.as_str()), Some(c));
    }
    assert_eq!(Action::parse("switch_model"), None);
    assert_eq!(Class::parse("5h"), None);
}

#[test]
fn validate_refuses_every_forbidden_member() {
    use Action::*;
    let cases: Vec<(Class, Vec<Action>, TableError)> = vec![
        (
            Class::NetworkOffline,
            vec![SwitchModel, Escalate],
            TableError::SwitchNotAllowed {
                class: Class::NetworkOffline,
                action: SwitchModel,
            },
        ),
        (
            Class::NetworkOffline,
            vec![Wait, SwitchAccount, Escalate],
            TableError::SwitchNotAllowed {
                class: Class::NetworkOffline,
                action: SwitchAccount,
            },
        ),
        (
            Class::Unknown,
            vec![SwitchModel, Escalate],
            TableError::SwitchNotAllowed {
                class: Class::Unknown,
                action: SwitchModel,
            },
        ),
        (
            Class::Unknown,
            vec![Retry, Escalate],
            TableError::RetryNotAllowed {
                class: Class::Unknown,
            },
        ),
        (
            Class::ModelBucketLimit,
            vec![LetVendorRetry, SwitchAccount, Escalate],
            TableError::SwitchNotAllowed {
                class: Class::ModelBucketLimit,
                action: SwitchAccount,
            },
        ),
        (
            Class::SpendBilling,
            vec![Wait, Escalate],
            TableError::Pinned {
                class: Class::SpendBilling,
                action: Wait,
            },
        ),
        (
            Class::SpendBilling,
            vec![Escalate, ExtraUsage],
            TableError::Pinned {
                class: Class::SpendBilling,
                action: ExtraUsage,
            },
        ),
        (
            Class::Auth,
            vec![SwitchAccount, Escalate],
            TableError::Pinned {
                class: Class::Auth,
                action: SwitchAccount,
            },
        ),
        (
            Class::Auth,
            vec![Retry, Escalate],
            TableError::Pinned {
                class: Class::Auth,
                action: Retry,
            },
        ),
        (
            Class::Session5hLimit,
            vec![Relogin, Escalate],
            TableError::ReloginOutsideAuth {
                class: Class::Session5hLimit,
            },
        ),
        (
            Class::Session5hLimit,
            vec![Wait, Retry],
            TableError::NoEscalate {
                class: Class::Session5hLimit,
            },
        ),
        (
            Class::SpendBilling,
            vec![],
            TableError::NoEscalate {
                class: Class::SpendBilling,
            },
        ),
        (
            Class::TransientCapacity,
            vec![Wait, Wait, Escalate],
            TableError::Duplicate {
                class: Class::TransientCapacity,
                action: Wait,
            },
        ),
    ];
    for (class, row, expected) in cases {
        let mut t = ActionTable::default();
        let before = t.clone();
        assert_eq!(
            t.set(class, row.clone()),
            Err(expected.clone()),
            "{class} {row:?}"
        );
        assert_eq!(t, before, "a refused row leaves the table unchanged");
        assert!(!expected.to_string().is_empty());
    }
}

#[test]
fn validate_accepts_every_allowed_member() {
    use Action::*;
    let mut t = ActionTable::default();
    assert_eq!(
        t.set(
            Class::Session5hLimit,
            vec![
                SwitchAccount,
                SwitchModel,
                LowerPriority,
                LimitReset,
                ExtraUsage,
                Wait,
                Retry,
                Escalate
            ]
        ),
        Ok(())
    );
    assert_eq!(t.set(Class::Auth, vec![Escalate]), Ok(()));
    assert_eq!(t.set(Class::Auth, vec![Escalate, Relogin]), Ok(()));
    assert_eq!(t.set(Class::Unknown, vec![Escalate]), Ok(()));
    assert_eq!(t.set(Class::NetworkOffline, vec![Wait, Escalate]), Ok(()));
    assert_eq!(
        t.set_names(Class::Weekly7dLimit, &["wait", "escalate"]),
        Ok(())
    );
    assert_eq!(t.get(Class::Weekly7dLimit), &[Wait, Escalate]);
    assert_eq!(t.validate(), Ok(()));
}

#[test]
fn an_unknown_name_refuses_the_whole_row() {
    let mut t = ActionTable::default();
    let before = t.clone();
    assert_eq!(
        t.set_names(
            Class::Session5hLimit,
            &["wait", "switch-accounts", "escalate"]
        ),
        Err(TableError::UnknownAction {
            class: Class::Session5hLimit,
            name: "switch-accounts".into()
        })
    );
    assert_eq!(t, before);
}

#[test]
fn budget_spellings_parse_and_print() {
    let table: [(&str, Option<Budget>); 9] = [
        (
            "4/6h",
            Some(Budget {
                count: 4,
                window_s: 21_600,
            }),
        ),
        (
            "3/10m",
            Some(Budget {
                count: 3,
                window_s: 600,
            }),
        ),
        (
            "3/24h",
            Some(Budget {
                count: 3,
                window_s: 86_400,
            }),
        ),
        (
            " 1/1d ",
            Some(Budget {
                count: 1,
                window_s: 86_400,
            }),
        ),
        (
            "2/90s",
            Some(Budget {
                count: 2,
                window_s: 90,
            }),
        ),
        ("0/6h", None),
        ("4/0h", None),
        ("4/6x", None),
        ("four/6h", None),
    ];
    for (text, expected) in table {
        assert_eq!(Budget::parse(text), expected, "{text}");
    }
    assert_eq!(Budget::parse("4/"), None);
    assert_eq!(Budget::parse("4"), None);
    assert_eq!(Budget::parse("/6h"), None);
    for text in ["4/6h", "3/10m", "3/24h", "2/90s"] {
        let b = Budget::parse(text).unwrap_or_else(|| panic!("{text}"));
        assert_eq!(b.as_string(), text);
    }
    // `d` parses but is never written back: a day-long budget comes back in
    // hours, which is the spelling §5.8.10 uses for `relogin_budget`.
    let day = Budget::parse("1/1d").unwrap_or_else(|| panic!("1/1d"));
    assert_eq!(day.as_string(), "1/24h");
    assert_eq!(
        Budget::parse(&day.as_string()),
        Some(day),
        "the re-spelling still round-trips"
    );
}

#[test]
fn the_default_config_is_the_design_config() {
    let c = LimitsConfig::default();
    assert_eq!(c.validate(), Ok(()));
    assert!(c.enabled);
    assert_eq!(c.level, 3);
    assert_eq!(c.budget.as_string(), "4/6h");
    assert_eq!(c.settle_s, 30);
    assert_eq!((c.transient_t1_s, c.transient_t2_s), (300, 900));
    assert_eq!((c.network_t1_s, c.network_t2_s), (300, 1800));
    assert_eq!(c.min_dwell_s, 600);
    assert!(c.switch_back);
    assert_eq!(c.switch_back_headroom_pct, 80);
    assert_eq!(c.max_wait_h, 24);
    assert_eq!(c.unknown_escalate_after.as_string(), "3/10m");
    assert_eq!(c.retry_text, RetryText::Continue);
    assert!(!c.allow_low_priority && !c.allow_limit_reset && !c.allow_spend);
    assert!(c.own_resume);
    assert!(c.relogin);
    assert_eq!(c.relogin_wait_s, 900);
    assert_eq!(c.relogin_budget.as_string(), "3/24h");
    assert!(!c.relogin_fallback_relaunch);
}

#[test]
fn config_validate_refuses_a_bad_table_a_level_above_4_and_inverted_timers() {
    let c = LimitsConfig {
        level: 5,
        ..Default::default()
    };
    assert_eq!(c.validate(), Err(ConfigError::Level(5)));
    let c = LimitsConfig {
        transient_t1_s: 901,
        ..Default::default()
    };
    assert_eq!(c.validate(), Err(ConfigError::Timers("transient")));
    let c = LimitsConfig {
        network_t2_s: 1,
        ..Default::default()
    };
    assert_eq!(c.validate(), Err(ConfigError::Timers("network")));
    let mut c = LimitsConfig::default();
    c.actions
        .rows_for_test(Class::Unknown, vec![Action::Retry, Action::Escalate]);
    assert!(matches!(
        c.validate(),
        Err(ConfigError::Table(TableError::RetryNotAllowed { .. }))
    ));
}

impl ActionTable {
    /// A back door for the test above: install a row WITHOUT validating it,
    /// so `validate()` on the whole config has something to catch.
    fn rows_for_test(&mut self, class: Class, row: Vec<Action>) {
        self.rows[class.index()] = row;
    }
}

// -- the engine: a synthetic 529 storm ---------------------------------------------

#[test]
fn a_529_storm_lets_the_vendor_retry_then_switches_model_at_t1_then_waits_retries_escalates() {
    let cfg = LimitsConfig::default();
    let g = guards(3, false);
    let c = cls(&[sf("overloaded"), sf("overloaded"), sf("overloaded"), pms()]);
    assert!(c.storm && !c.unpaired);
    let mut s = State::new(&c, NOW, GEN, Carry::default());

    // t = 0: let the vendor retry (L0), at once, even mid-turn.
    let mut busy = g;
    busy.busy = true;
    let d = step(&cfg, &s, &busy, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::LetVendorRetry, 0)));
    s.commit(&d, 1, NOW);
    assert_eq!(s.step, 1);
    assert_eq!(s.in_flight, None);

    // Before T1: wait for it.
    let d = step(&cfg, &s, &g, NOW + 299, GEN);
    assert_eq!(d.decision, Decision::Wait { until: NOW + 300 });
    assert_eq!(s.step, 1, "a time gate does not advance the table");

    // At T1 with a storm: switch-model, L3, one in flight.
    let d = step(&cfg, &s, &g, NOW + 300, GEN);
    assert_eq!(action_of(&d), Some((Action::SwitchModel, 3)));
    s.commit(&d, 2, NOW + 300);
    assert!(s.in_flight.is_some());
    let again = step(&cfg, &s, &g, NOW + 301, GEN);
    assert_eq!(
        again.decision,
        Decision::Refused {
            why: Refusal::InFlight
        }
    );
    s.verdict(Verdict::Executed, NOW + 330);
    assert_eq!(s.last_switch_at, Some(NOW + 330));
    assert_eq!(s.budget.left(cfg.budget, NOW + 330), 3);
    assert_eq!(s.step, 2);

    // Wait arms at T2 only.
    let d = step(&cfg, &s, &g, NOW + 331, GEN);
    assert_eq!(d.decision, Decision::Wait { until: NOW + 900 });
    let d = step(&cfg, &s, &g, NOW + 900, GEN);
    assert_eq!(action_of(&d), Some((Action::Wait, 1)));
    assert_eq!(d.wait_until, Some(NOW + 900 + TRANSIENT_WAIT_S));
    s.commit(&d, 3, NOW + 900);
    assert_eq!(s.wait_until, Some(NOW + 1500));

    // Retry once the wait is over, then escalate.
    let d = step(&cfg, &s, &g, NOW + 1000, GEN);
    assert_eq!(d.decision, Decision::Wait { until: NOW + 1500 });
    let d = step(&cfg, &s, &g, NOW + 1500, GEN);
    assert_eq!(action_of(&d), Some((Action::Retry, 3)));
    s.commit(&d, 4, NOW + 1500);
    s.verdict(Verdict::Timeout, NOW + 1600);
    let d = step(&cfg, &s, &g, NOW + 1600, GEN);
    assert_eq!(action_of(&d), Some((Action::Escalate, 1)));
    s.commit(&d, 5, NOW + 1600);
    assert_eq!(s.terminal, Some(Terminal::Escalated));
    let d = step(&cfg, &s, &g, NOW + 1601, GEN);
    assert_eq!(
        d.decision,
        Decision::Refused {
            why: Refusal::Terminal(Terminal::Escalated)
        }
    );
}

#[test]
fn a_529_without_a_storm_never_switches() {
    let cfg = LimitsConfig::default();
    let g = guards(3, false);
    let mut s = state(Class::TransientCapacity, false, None);
    s.step = 1; // past let-vendor-retry
    assert_eq!(
        step(&cfg, &s, &g, NOW + 300, GEN).decision,
        Decision::Wait { until: NOW + 900 }
    );
    let d = step(&cfg, &s, &g, NOW + 900, GEN);
    assert_eq!(action_of(&d), Some((Action::Wait, 1)));
    assert_eq!(skipped_names(&d), [Action::SwitchModel]);
    assert!(d.skipped[0].why.contains("no retry storm"));

    // A third overloaded failure observed after classification makes the storm.
    let mut s2 = state(Class::TransientCapacity, false, None);
    s2.step = 1;
    for t in [NOW + 10, NOW + 20, NOW + 30] {
        s2.observe(&Event::Evidence(sf("overloaded")), t);
    }
    assert!(s2.storm);
    assert_eq!(
        action_of(&step(&cfg, &s2, &g, NOW + 300, GEN)),
        Some((Action::SwitchModel, 3))
    );
}

// -- the engine: the exhausted five-hour window ----------------------------------

fn five_hour_pair() -> Classification {
    cls(&[
        sf("rate_limit"),
        win(
            WindowKind::FiveHour,
            100.0,
            Some(NOW + 1800),
            Source::StatusLine,
        ),
    ])
}

#[test]
fn an_exhausted_five_hour_window_with_accounts_enabled_switches_account() {
    let cfg = LimitsConfig::default();
    let s = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    let d = step(&cfg, &s, &guards(4, true), NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::SwitchAccount, 4)));
    assert!(d.skipped.is_empty());
    assert_eq!(d.step, 0);
    match d.decision {
        Decision::Act { reason, .. } => assert_eq!(reason, "limits.actions.session-5h-limit[0]"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_exhausted_five_hour_window_without_accounts_switches_model_once() {
    let cfg = LimitsConfig::default();
    let s = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    let d = step(&cfg, &s, &guards(4, false), NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::SwitchModel, 3)));
    assert_eq!(skipped_names(&d), [Action::SwitchAccount]);
    assert!(
        d.skipped[0].why.contains("[accounts] enabled = false"),
        "{}",
        d.skipped[0].why
    );
}

#[test]
fn level_3_with_accounts_enabled_still_skips_switch_account() {
    let cfg = LimitsConfig::default();
    let s = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    let d = step(&cfg, &s, &guards(3, true), NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::SwitchModel, 3)));
    assert!(
        d.skipped[0].why.contains("level 4 > allowed 3"),
        "{}",
        d.skipped[0].why
    );
}

#[test]
fn five_hour_waits_to_the_reset_plus_jitter_then_retries_then_observes() {
    let cfg = LimitsConfig::default();
    let g = guards(2, false); // no switches at level 2
    let mut s = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::Wait, 1)));
    assert_eq!(
        skipped_names(&d),
        [Action::SwitchAccount, Action::SwitchModel]
    );
    assert_eq!(d.wait_until, Some(NOW + 1800 + RESET_JITTER_S));
    s.commit(&d, 1, NOW);
    assert_eq!(s.wait_until, Some(NOW + 1860));
    // Retry is L3: level 2 cannot type it, so it is SKIPPED rather than
    // waited on, and the table walks past it at once. A guard a step fails
    // never holds the class on a clock the step will never reach — that is
    // what keeps a dry-budget escalate from being deferred behind a `retry`
    // that `own_resume = false` had already ruled out.
    for t in [NOW + 1800, NOW + 1860] {
        let d = step(&cfg, &s, &g, t, GEN);
        assert_eq!(
            d.decision,
            Decision::Refused {
                why: Refusal::Exhausted
            },
            "t={t}"
        );
        assert_eq!(skipped_names(&d), [Action::Retry, Action::Escalate]);
        assert!(
            d.skipped[0].why.contains("level 3 > allowed 2"),
            "{}",
            d.skipped[0].why
        );
        assert!(
            d.skipped[1].why.contains("reset within max_wait_h"),
            "{}",
            d.skipped[1].why
        );
    }
    // At level 3 nothing is skipped, so the armed wait is what the engine
    // reports until `resets_at` + jitter, and the retry is typed on the tick.
    let g3 = guards(3, false);
    assert_eq!(
        step(&cfg, &s, &g3, NOW, GEN).decision,
        Decision::Wait { until: NOW + 1860 }
    );
    assert_eq!(
        step(&cfg, &s, &g3, NOW + 1859, GEN).decision,
        Decision::Wait { until: NOW + 1860 }
    );
    assert_eq!(
        action_of(&step(&cfg, &s, &g3, NOW + 1860, GEN)),
        Some((Action::Retry, 3))
    );
}

#[test]
fn five_hour_reset_beyond_max_wait_h_escalates_instead_of_waiting() {
    let cfg = LimitsConfig::default();
    let g = guards(2, false);
    let s = state(Class::Session5hLimit, false, Some(NOW + 25 * 3600));
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::Escalate, 1)));
    assert!(
        d.skipped
            .iter()
            .any(|k| k.action == Action::Wait && k.why.contains("max_wait_h"))
    );
    // And with no reset known at all.
    let s = state(Class::Session5hLimit, false, None);
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::Escalate, 1)));
}

#[test]
fn a_dry_budget_degrades_the_switch_to_display_only() {
    let cfg = LimitsConfig::default();
    let mut carry = Carry::default();
    for t in [NOW - 5000, NOW - 4000, NOW - 3000, NOW - 2000] {
        carry.switches.spend(t);
    }
    let mut s = State::new(&five_hour_pair(), NOW, GEN, carry);
    let d = step(&cfg, &s, &guards(4, true), NOW, GEN);
    match &d.decision {
        Decision::Act {
            action,
            level,
            reason,
        } => {
            assert_eq!((*action, *level), (Action::SwitchAccount, 1));
            assert!(reason.starts_with("degraded:budget"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    s.commit(&d, 1, NOW);
    assert_eq!(s.in_flight, None, "a degraded action awaits no verdict");
    assert_eq!(s.step, 1);
    // The next switch degrades too; then wait; the dry budget escalates.
    let d = step(&cfg, &s, &guards(4, true), NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::SwitchModel, 1)));
    s.commit(&d, 2, NOW);
    let d = step(&cfg, &s, &guards(4, true), NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::Wait, 1)));
    s.commit(&d, 3, NOW);
    let d = step(&cfg, &s, &guards(4, true), NOW + 1860, GEN);
    assert_eq!(action_of(&d), Some((Action::Retry, 3)));
    s.commit(&d, 4, NOW + 1860);
    s.verdict(Verdict::Executed, NOW + 1870);
    let d = step(&cfg, &s, &guards(4, true), NOW + 1870, GEN);
    assert_eq!(
        action_of(&d),
        Some((Action::Escalate, 1)),
        "budget dry: escalate"
    );
    // Six hours on, the budget is back.
    assert_eq!(s.budget.left(cfg.budget, NOW + 6 * 3600), 4);
}

#[test]
fn weekly_switches_then_arms_the_wait_then_escalates_at_once() {
    let cfg = LimitsConfig::default();
    let g = guards(4, true);
    let c = cls(&[
        sf("rate_limit"),
        win(
            WindowKind::SevenDay,
            100.0,
            Some(NOW + 3 * 86_400),
            Source::StatusLine,
        ),
    ]);
    assert_eq!(c.class, Class::Weekly7dLimit);
    let mut s = State::new(&c, NOW, GEN, Carry::default());
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::SwitchAccount, 4)));
    s.commit(&d, 1, NOW);
    s.verdict(Verdict::Executed, NOW + 20);
    // The second switch is inside the dwell: skipped, wait arms at the reset.
    let d = step(&cfg, &s, &g, NOW + 30, GEN);
    assert_eq!(action_of(&d), Some((Action::Wait, 1)));
    assert_eq!(skipped_names(&d), [Action::SwitchModel]);
    assert_eq!(d.wait_until, Some(NOW + 3 * 86_400 + RESET_JITTER_S));
    s.commit(&d, 2, NOW + 30);
    // Escalate right away: days of idle capacity is a decision.
    let d = step(&cfg, &s, &g, NOW + 31, GEN);
    assert_eq!(action_of(&d), Some((Action::Escalate, 1)));
}

// -- the engine: the Fable bucket, auth, billing ------------------------------------

#[test]
fn the_fable_bucket_escalates_and_never_switches() {
    let cfg = LimitsConfig::default();
    let g = guards(4, true);
    let c = cls(&[
        sfd("rate_limit", r#"apiError:"model_requires_usage_credits""#),
        notif("quota_auto_resume_offer_armed"),
        pms(),
    ]);
    assert_eq!(c.class, Class::ModelBucketLimit);
    let mut s = State::new(&c, NOW, GEN, Carry::default());
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::LetVendorRetry, 0)));
    s.commit(&d, 1, NOW);
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::Escalate, 1)));
    s.commit(&d, 2, NOW);
    assert_eq!(s.terminal, Some(Terminal::Escalated));
    assert_eq!(s.budget.left(cfg.budget, NOW), 4, "no switch was spent");
    // The table cannot even be told to switch here.
    let mut t = ActionTable::default();
    assert!(
        t.set(
            Class::ModelBucketLimit,
            vec![Action::SwitchModel, Action::Escalate]
        )
        .is_err()
    );
}

#[test]
fn auth_and_billing_are_pinned_to_the_human() {
    let cfg = LimitsConfig::default();
    let g = guards(4, true);
    let spend = cls(&[
        sf("billing_error"),
        win(
            WindowKind::SpendLimit,
            140.0,
            Some(NOW + 500_000),
            Source::StatusLine,
        ),
    ]);
    let s = State::new(&spend, NOW, GEN, Carry::default());
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::Escalate, 1)));
    assert!(d.skipped.is_empty());

    // Auth from a hook plus an AUTH BANNER is a pair, so §5.8.10's re-login
    // door opens: this is the one path that reaches `Relogin` from a real
    // classification rather than a hand-built `State`.
    let auth = cls(&[sf("authentication_failed"), banner("Please run /login")]);
    let s = State::new(&auth, NOW, GEN, Carry::default());
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::Relogin, 3)));
    assert!(d.skipped.is_empty());

    // REWRITTEN 2026-09-22 (§5.8.2, narrowed). This block asserted that the
    // hook ALONE skipped `relogin` for want of a pair and escalated
    // instead. `relogin` is not one of the four spending actions — it opens
    // the VENDOR's own sign-in door and a human walks through it (§5.8.10)
    // — so one source is enough for it, and the same is true of the banner
    // alone, which is the whole zero-hook case.
    for lone in [
        cls(&[sf("authentication_failed")]),
        cls(&[banner("API Error: 401 Invalid API key - Please run /login")]),
    ] {
        assert_eq!(lone.class, Class::Auth);
        assert!(lone.unpaired, "{:?}", lone.reasons);
        let s = State::new(&lone, NOW, GEN, Carry::default());
        let d = step(&cfg, &s, &g, NOW, GEN);
        assert_eq!(
            action_of(&d),
            Some((Action::Relogin, 3)),
            "{:?}",
            d.decision
        );
        assert!(d.skipped.is_empty());
    }

    for (class, action) in [
        (Class::SpendBilling, Action::Wait),
        (Class::SpendBilling, Action::SwitchAccount),
        (Class::Auth, Action::SwitchAccount),
        (Class::Auth, Action::Wait),
    ] {
        let mut t = ActionTable::default();
        assert!(matches!(
            t.set(class, vec![action, Action::Escalate]),
            Err(TableError::Pinned { .. })
        ));
    }
}

#[test]
fn auth_relogin_runs_once_per_generation_then_waits_for_the_human_then_escalates() {
    let cfg = LimitsConfig::default();
    let g = guards(3, false);
    let mut s = state(Class::Auth, false, None);
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::Relogin, 3)));
    s.commit(&d, 1, NOW);
    s.verdict(Verdict::Executed, NOW + 5);
    assert_eq!(s.relogin_at, Some(NOW + 5));
    assert_eq!(s.relogins.left(cfg.relogin_budget, NOW + 5), 2);
    // Awaiting the human: nothing is typed, the timer runs.
    assert_eq!(
        step(&cfg, &s, &g, NOW + 100, GEN).decision,
        Decision::Wait { until: NOW + 905 }
    );
    // The vendor's flow completed: settled, never re-typed.
    let mut done = s.clone();
    done.observe(&Event::Cleared, NOW + 200);
    assert_eq!(
        step(&cfg, &done, &g, NOW + 300, GEN).decision,
        Decision::Refused {
            why: Refusal::Terminal(Terminal::Settled)
        }
    );
    // Timed out: escalate.
    let d = step(&cfg, &s, &g, NOW + 905, GEN);
    assert_eq!(action_of(&d), Some((Action::Escalate, 1)));
    // A second expiry inside the generation is not re-typed.
    let mut again = s.clone();
    again.step = 0;
    let d = step(&cfg, &again, &g, NOW + 905, GEN);
    assert_eq!(action_of(&d), Some((Action::Escalate, 1)));
    assert!(d.skipped[0].why.contains("already typed"));
}

#[test]
fn auth_relogin_is_skipped_when_its_budget_is_dry_or_the_knob_is_off() {
    let mut cfg = LimitsConfig::default();
    let g = guards(3, false);
    let mut carry = Carry::default();
    for t in [NOW - 3600, NOW - 7200, NOW - 10_800] {
        carry.relogins.spend(t);
    }
    let c = Classification {
        class: Class::Auth,
        unpaired: false,
        reasons: Vec::new(),
        resets_at: None,
        storm: false,
        no_response: false,
    };
    let s = State::new(&c, NOW, GEN, carry);
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::Escalate, 1)));
    assert!(
        d.skipped[0].why.contains("relogin budget 3/24h is dry"),
        "{}",
        d.skipped[0].why
    );

    cfg.relogin = false;
    let s = state(Class::Auth, false, None);
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::Escalate, 1)));
    assert!(d.skipped[0].why.contains("relogin = false"));
}

// -- the engine: flapping, the budget, one in flight, generations -----------------

#[test]
fn flapping_is_prevented_by_the_dwell() {
    let cfg = LimitsConfig::default();
    let g = guards(4, true);
    let carry = Carry {
        last_switch_at: Some(NOW - 100),
        ..Carry::default()
    };
    let s = State::new(&five_hour_pair(), NOW, GEN, carry);
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::Wait, 1)));
    assert_eq!(
        skipped_names(&d),
        [Action::SwitchAccount, Action::SwitchModel]
    );
    assert!(
        d.skipped[0].why.contains("min_dwell_s"),
        "{}",
        d.skipped[0].why
    );
    // The dwell over, the switch is back.
    let d = step(&cfg, &s, &g, NOW + 500, GEN);
    assert_eq!(action_of(&d), Some((Action::SwitchAccount, 4)));
}

#[test]
fn model_and_account_switches_share_one_budget() {
    let cfg = LimitsConfig::default();
    let g = guards(4, true);
    let mut carry = Carry::default();
    let mut t = NOW - 3000;
    for action in [
        Action::SwitchModel,
        Action::SwitchAccount,
        Action::SwitchModel,
        Action::SwitchAccount,
    ] {
        // Each earlier generation executed one switch.
        let mut s = State::new(&five_hour_pair(), t, GEN, carry);
        s.in_flight = Some(InFlight {
            action,
            step: 0,
            class: s.class,
            id: 1,
            started: t,
        });
        s.verdict(Verdict::Executed, t);
        carry = s.carry();
        t += 10;
    }
    assert_eq!(carry.switches.left(cfg.budget, NOW), 0);
    let carry = Carry {
        last_switch_at: Some(NOW - 5000), // the dwell is over
        ..carry
    };
    let s = State::new(&five_hour_pair(), NOW, GEN, carry);
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::SwitchAccount, 1)), "degraded");
}

#[test]
fn never_two_automatic_actions_in_flight() {
    let cfg = LimitsConfig::default();
    let g = guards(4, true);
    let mut s = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    let d = step(&cfg, &s, &g, NOW, GEN);
    s.commit(&d, 1, NOW);
    assert!(s.in_flight.is_some());
    // A second class arriving is queued: no act, no advance.
    s.observe(&Event::Evidence(sf("overloaded")), NOW + 1);
    let d = step(&cfg, &s, &g, NOW + 1, GEN);
    assert_eq!(
        d.decision,
        Decision::Refused {
            why: Refusal::InFlight
        }
    );
    assert_eq!(d.step, 0);
    // Refused verdicts advance past the action but spend nothing.
    s.verdict(Verdict::Refused, NOW + 2);
    assert_eq!(s.in_flight, None);
    assert_eq!(s.step, 1);
    assert_eq!(s.budget.left(cfg.budget, NOW + 2), 4);
    assert_eq!(s.last_switch_at, None);
}

#[test]
fn a_generation_change_while_in_flight_drops_it_and_refuses_the_stale_generation() {
    let cfg = LimitsConfig::default();
    let g = guards(4, true);
    let mut s = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    let d = step(&cfg, &s, &g, NOW, GEN);
    s.commit(&d, 1, NOW);
    s.observe(&Event::GenerationChanged(GEN + 1), NOW + 1);
    assert_eq!(s.in_flight, None);
    assert_eq!(s.terminal, Some(Terminal::Generation));
    assert_eq!(
        step(&cfg, &s, &g, NOW + 2, GEN).decision,
        Decision::Refused {
            why: Refusal::StaleGeneration
        }
    );
    assert_eq!(
        step(&cfg, &s, &g, NOW + 2, GEN + 1).decision,
        Decision::Refused {
            why: Refusal::Terminal(Terminal::Generation)
        }
    );
    // A late verdict for the dropped action changes nothing.
    s.verdict(Verdict::Executed, NOW + 3);
    assert_eq!(s.last_switch_at, None);
    assert_eq!(s.budget.left(cfg.budget, NOW + 3), 4);
}

#[test]
fn a_request_from_another_generation_is_refused_even_when_idle() {
    let cfg = LimitsConfig::default();
    let s = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    for other_gen in [GEN - 1, GEN + 1, 0] {
        assert_eq!(
            step(&cfg, &s, &guards(4, true), NOW, other_gen).decision,
            Decision::Refused {
                why: Refusal::StaleGeneration
            }
        );
    }
}

#[test]
fn a_vendor_quota_auto_resume_cancels_the_pending_retry() {
    let cfg = LimitsConfig::default();
    let g = guards(3, false);
    let mut s = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    // Past the switches (level 3, no accounts: switch-model would act, so
    // put the dwell on) — arm the wait.
    s.last_switch_at = Some(NOW - 10);
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::Wait, 1)));
    s.commit(&d, 1, NOW);
    assert_eq!(s.wait_until, Some(NOW + 1860));

    // The human re-armed the vendor's own resume: the harness's retry is
    // dropped for this generation, and the class only observes.
    let mut armed = s.clone();
    armed.observe(&Event::Evidence(notif("quota_auto_resume_armed")), NOW + 10);
    assert!(armed.retry_cancelled);
    let d = step(&cfg, &armed, &g, NOW + 1860, GEN);
    assert_eq!(
        d.decision,
        Decision::Refused {
            why: Refusal::Exhausted
        }
    );
    assert!(
        d.skipped
            .iter()
            .any(|k| k.action == Action::Retry && k.why.contains("cancelled"))
    );

    // The vendor resumed by itself: settled.
    let mut fired = s.clone();
    fired.observe(
        &Event::Evidence(notif("quota_auto_resume_fired")),
        NOW + 1850,
    );
    assert_eq!(
        step(&cfg, &fired, &g, NOW + 1860, GEN).decision,
        Decision::Refused {
            why: Refusal::Terminal(Terminal::Settled)
        }
    );

    // Other members of the family cancel nothing.
    let mut stale = s.clone();
    stale.observe(&Event::Evidence(notif("quota_auto_resume_stale")), NOW + 10);
    assert!(!stale.retry_cancelled);
    assert_eq!(
        action_of(&step(&cfg, &stale, &g, NOW + 1860, GEN)),
        Some((Action::Retry, 3))
    );
}

// -- the engine: hold, busy, self, disabled, opt-ins ---------------------------------

#[test]
fn hold_refuses_everything_including_l0() {
    let cfg = LimitsConfig::default();
    let mut g = guards(4, true);
    g.hold = true;
    let s = state(Class::TransientCapacity, false, None);
    assert_eq!(
        step(&cfg, &s, &g, NOW, GEN).decision,
        Decision::Refused { why: Refusal::Hold }
    );
    let s = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    assert_eq!(
        step(&cfg, &s, &g, NOW, GEN).decision,
        Decision::Refused { why: Refusal::Hold }
    );
    assert_eq!(Refusal::Hold.as_str(), "refused:hold");
}

#[test]
fn busy_refuses_actions_above_l1_and_does_not_advance() {
    let cfg = LimitsConfig::default();
    let mut g = guards(4, true);
    g.busy = true;
    let s = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(d.decision, Decision::Refused { why: Refusal::Busy });
    assert_eq!(d.step, 0, "the candidate is re-awaited, not skipped");
    // L1 runs mid-turn.
    let s = State::new(
        &cls(&[
            sf("billing_error"),
            win(
                WindowKind::SpendLimit,
                120.0,
                Some(NOW + 1000),
                Source::Cache,
            ),
        ]),
        NOW,
        GEN,
        Carry::default(),
    );
    assert_eq!(
        action_of(&step(&cfg, &s, &g, NOW, GEN)),
        Some((Action::Escalate, 1))
    );
}

#[test]
fn a_self_targeted_call_needs_the_self_flag() {
    let cfg = LimitsConfig::default();
    let mut g = guards(4, true);
    g.caller_is_target = true;
    let s = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    assert_eq!(
        step(&cfg, &s, &g, NOW, GEN).decision,
        Decision::Refused {
            why: Refusal::SelfTarget
        }
    );
    g.self_allowed = true;
    assert_eq!(
        action_of(&step(&cfg, &s, &g, NOW, GEN)),
        Some((Action::SwitchAccount, 4))
    );
}

#[test]
fn a_disabled_capability_refuses() {
    let cfg = LimitsConfig {
        enabled: false,
        ..Default::default()
    };
    let s = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    assert_eq!(
        step(&cfg, &s, &guards(4, true), NOW, GEN).decision,
        Decision::Refused {
            why: Refusal::Disabled
        }
    );
}

#[test]
fn extra_usage_is_unreachable_regardless_of_allow_spend() {
    let mut cfg = LimitsConfig {
        allow_spend: true,
        ..Default::default()
    };
    assert_eq!(
        cfg.actions.set(
            Class::Session5hLimit,
            vec![Action::ExtraUsage, Action::Escalate]
        ),
        Ok(())
    );
    let mut g = guards(4, true);
    g.allow_spend = true;
    let s = state(Class::Session5hLimit, false, Some(NOW + 100_000));
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::Escalate, 1)));
    assert_eq!(skipped_names(&d), [Action::ExtraUsage]);
    assert!(
        d.skipped[0].why.contains("unreachable in ABI 1"),
        "{}",
        d.skipped[0].why
    );
    assert_eq!(Action::ExtraUsage.level(), 5);
}

#[test]
fn lower_priority_and_limit_reset_need_their_opt_ins() {
    let mut cfg = LimitsConfig::default();
    assert_eq!(
        cfg.actions.set(
            Class::Session5hLimit,
            vec![Action::LowerPriority, Action::LimitReset, Action::Escalate]
        ),
        Ok(())
    );
    let s = state(Class::Session5hLimit, false, Some(NOW + 100_000));
    let off = guards(3, false);
    let d = step(&cfg, &s, &off, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::Escalate, 1)));
    assert_eq!(
        skipped_names(&d),
        [Action::LowerPriority, Action::LimitReset]
    );
    let mut on = off;
    on.allow_low_priority = true;
    assert_eq!(
        action_of(&step(&cfg, &s, &on, NOW, GEN)),
        Some((Action::LowerPriority, 3))
    );
    let mut reset = off;
    reset.allow_limit_reset = true;
    let d = step(&cfg, &s, &reset, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::LimitReset, 3)));
    assert_eq!(skipped_names(&d), [Action::LowerPriority]);
}

#[test]
fn own_resume_false_leaves_the_wait_and_retry_to_the_vendor() {
    let cfg = LimitsConfig::default();
    let mut g = guards(2, false);
    g.own_resume = false;
    let s = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(
        d.decision,
        Decision::Refused {
            why: Refusal::Exhausted
        }
    );
    let skipped = skipped_names(&d);
    assert!(skipped.contains(&Action::Wait) && skipped.contains(&Action::Retry));
    assert!(
        d.skipped
            .iter()
            .any(|k| k.why.contains("own_resume = false"))
    );
}

/// REWRITTEN 2026-09-22. It was `a_display_only_classification_runs_nothing_above_l1`
/// and it asserted the pre-inversion rule verbatim: on one source the engine
/// skipped `switch-model` too, armed the L1 wait, and then answered
/// `refused:exhausted` with `retry` and `escalate` both skipped. Under the
/// narrowed rule (§5.8.2) one source withholds the SPENDING action and
/// nothing else, so the same state now switches models, waits and retries —
/// and `switch-account` is still the one thing it is refused.
#[test]
fn one_source_runs_the_reversible_ladder_and_refuses_only_the_spending_action() {
    let cfg = LimitsConfig::default();
    let g = guards(4, true);
    let c = cls(&[win(
        WindowKind::FiveHour,
        100.0,
        Some(NOW + 1800),
        Source::StatusLine,
    )]);
    assert!(c.unpaired);
    let mut s = State::new(&c, NOW, GEN, Carry::default());

    // Step 0 of the five-hour row is `switch-account`: refused, journaled,
    // and named by the rule that refused it.
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(
        action_of(&d),
        Some((Action::SwitchModel, 3)),
        "a reversible L3 action runs on one source: {:?}",
        d.decision
    );
    assert_eq!(skipped_names(&d), [Action::SwitchAccount]);
    assert!(
        d.skipped[0].why.contains("one source") && d.skipped[0].why.contains("account"),
        "{}",
        d.skipped[0].why
    );
    s.commit(&d, 1, NOW);
    s.verdict(Verdict::Executed, NOW + 1);

    // And the rest of the row is reachable too: the wait arms on the reset,
    // then the retry it gates.
    let d = step(&cfg, &s, &g, NOW + 2, GEN);
    assert_eq!(action_of(&d), Some((Action::Wait, 1)), "arming is L1");
    assert_eq!(d.wait_until, Some(NOW + 1800 + RESET_JITTER_S));
    s.commit(&d, 2, NOW + 2);
    assert_eq!(
        action_of(&step(&cfg, &s, &g, NOW + 1900, GEN)),
        Some((Action::Retry, 3)),
        "retry is reversible: one source is enough"
    );

    // The pair arriving later is what unlocks the account move, and nothing
    // else changes.
    let paired = five_hour_pair();
    let mut s = State::new(&paired, NOW, GEN, Carry::default());
    assert!(!s.unpaired);
    assert_eq!(
        action_of(&step(&cfg, &s, &g, NOW, GEN)),
        Some((Action::SwitchAccount, 4))
    );
    // A thinner re-read of the SAME class does not take the pair away: the
    // fold is `unpaired && c.unpaired`, so a generation that once had two
    // independent sources keeps them.
    s.observe(&Event::Reclassified(c), NOW + 1);
    assert!(!s.unpaired, "a pair, once made, holds for the generation");
}

#[test]
fn a_human_keystroke_pauses_the_timers() {
    let cfg = LimitsConfig::default();
    let g = guards(4, true);
    let mut s = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    s.observe(&Event::HumanKeystroke, NOW + 5);
    assert_eq!(
        step(&cfg, &s, &g, NOW + 6, GEN).decision,
        Decision::Wait { until: NOW + 605 }
    );
    assert_eq!(
        action_of(&step(&cfg, &s, &g, NOW + 605, GEN)),
        Some((Action::SwitchAccount, 4))
    );
}

// -- the engine: unknown and network ----------------------------------------------

#[test]
fn unknown_escalates_after_three_hits_in_ten_minutes_and_never_retries() {
    let cfg = LimitsConfig::default();
    let g = guards(4, true);
    let mut s = State::new(&cls(&[sf("invalid_request")]), NOW, GEN, Carry::default());
    assert_eq!(s.hits, [NOW]);
    assert_eq!(
        step(&cfg, &s, &g, NOW + 1, GEN).decision,
        Decision::Wait { until: NOW + 600 }
    );
    s.observe(&Event::Evidence(sf("invalid_request")), NOW + 60);
    assert_eq!(
        step(&cfg, &s, &g, NOW + 61, GEN).decision,
        Decision::Wait { until: NOW + 600 }
    );
    s.observe(&Event::Evidence(sf("invalid_request")), NOW + 120);
    assert_eq!(
        action_of(&step(&cfg, &s, &g, NOW + 121, GEN)),
        Some((Action::Escalate, 1))
    );
    // Three hits spread over more than ten minutes are not a burst — but the
    // wait they are told to serve ENDS, and it ends at the first hit's window,
    // not at a deadline that walks away as each hit ages out.
    let mut slow = State::new(&cls(&[sf("invalid_request")]), NOW, GEN, Carry::default());
    slow.observe(&Event::Evidence(sf("invalid_request")), NOW + 400);
    slow.observe(&Event::Evidence(sf("invalid_request")), NOW + 800);
    assert_eq!(
        action_of(&step(&cfg, &slow, &g, NOW + 801, GEN)),
        Some((Action::Escalate, 1)),
        "the first hit's ten minutes are over: a human is told"
    );
    // A renamed hook observed later turns any class into unknown.
    let mut s = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    s.observe(&Event::Evidence(sf("rate_limit_v9")), NOW + 1);
    assert_eq!(s.class, Class::Unknown);
    assert!(s.unpaired);
}

#[test]
fn a_lone_unknown_reaches_escalate_and_its_wait_never_walks_away() {
    let cfg = LimitsConfig::default();
    let g = guards(4, true);
    let s = State::new(&cls(&[sf("invalid_request")]), NOW, GEN, Carry::default());
    let window = i64::try_from(cfg.unknown_escalate_after.window_s).expect("small");

    // One hit and nothing else. The deadline is the FIRST hit's window and it
    // does not move, however often the host asks: driving the engine at every
    // deadline it returns used to hand back a deadline one window further on
    // for ever, so a lone `unknown` parked the session with nobody told.
    let mut now = NOW;
    let mut waits = 0;
    loop {
        match step(&cfg, &s, &g, now, GEN).decision {
            Decision::Wait { until } => {
                assert!(
                    until > now,
                    "a wait that ends now is a spin: {until} <= {now}"
                );
                assert_eq!(until, NOW + window, "the deadline is anchored, not rolling");
                waits += 1;
                assert!(waits <= 4, "the wait never converged");
                now = until;
            }
            Decision::Act { action, .. } => {
                assert_eq!(action, Action::Escalate);
                assert_eq!(
                    now,
                    NOW + window,
                    "escalation is due when that window closes"
                );
                break;
            }
            other => panic!("neither a wait nor an action: {other:?}"),
        }
    }
    assert_eq!(waits, 1, "exactly one wait, then the escalation");

    // NEGATIVE CASE: one second before the window closes it is still a wait,
    // and a burst inside the window escalates without waiting for it at all.
    assert_eq!(
        step(&cfg, &s, &g, NOW + window - 1, GEN).decision,
        Decision::Wait {
            until: NOW + window
        }
    );
    let mut burst = State::new(&cls(&[sf("invalid_request")]), NOW, GEN, Carry::default());
    burst.observe(&Event::Evidence(sf("invalid_request")), NOW + 1);
    burst.observe(&Event::Evidence(sf("invalid_request")), NOW + 2);
    assert_eq!(
        action_of(&step(&cfg, &burst, &g, NOW + 3, GEN)),
        Some((Action::Escalate, 1)),
        "three inside the window is the burst rule, long before the deadline"
    );
}

#[test]
fn network_offline_waits_at_t1_retries_only_after_no_response_and_escalates_at_t2() {
    let cfg = LimitsConfig::default();
    let g = guards(4, true);
    let mut s = State::new(
        &cls(&[sfd("unknown", "network_error"), sf("unknown")]),
        NOW,
        GEN,
        Carry::default(),
    );
    // The last StopFailure decides: a bare `unknown` after a network error.
    assert_eq!(s.class, Class::Unknown);
    s = State::new(
        &cls(&[sf("unknown"), sfd("unknown", "ECONNRESET")]),
        NOW,
        GEN,
        Carry::default(),
    );
    assert_eq!(s.class, Class::NetworkOffline);
    let d = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(action_of(&d), Some((Action::LetVendorRetry, 0)));
    s.commit(&d, 1, NOW);
    assert_eq!(
        step(&cfg, &s, &g, NOW + 10, GEN).decision,
        Decision::Wait { until: NOW + 300 }
    );
    let d = step(&cfg, &s, &g, NOW + 300, GEN);
    assert_eq!(action_of(&d), Some((Action::Wait, 1)));
    assert_eq!(d.wait_until, Some(NOW + 1800));
    s.commit(&d, 2, NOW + 300);
    // No `No response …` seen: retry is skipped, escalate waits for T2.
    let d = step(&cfg, &s, &g, NOW + 1800, GEN);
    assert_eq!(action_of(&d), Some((Action::Escalate, 1)));
    assert_eq!(skipped_names(&d), [Action::Retry]);
    // With the vendor's first-byte timeout banner, one retry at the boundary.
    // CHANGED 2026-09-22: this used to clear `display_only` by hand first,
    // because a single source could not reach an L3 act. `retry` is
    // reversible, so the one-source state reaches it — the hand-cleared flag
    // is kept only as the assertion that a PAIR behaves identically.
    let mut pair = s.clone();
    assert!(pair.unpaired, "network-offline from one hook value");
    assert_eq!(
        action_of(&{
            let mut one = s.clone();
            one.observe(
                &Event::Evidence(banner("No response from the API after 30000ms")),
                NOW + 1700,
            );
            step(&cfg, &one, &g, NOW + 1800, GEN)
        }),
        Some((Action::Retry, 3)),
        "one source retries: nothing is spent by asking again"
    );
    pair.unpaired = false;
    pair.observe(
        &Event::Evidence(banner("No response from the API after 30000ms")),
        NOW + 1700,
    );
    assert!(pair.no_response);
    assert_eq!(
        action_of(&step(&cfg, &pair, &g, NOW + 1800, GEN)),
        Some((Action::Retry, 3))
    );
}

#[test]
fn the_ledger_counts_inside_the_window_only() {
    let b = Budget {
        count: 2,
        window_s: 100,
    };
    let mut l = Ledger::default();
    assert_eq!(l.left(b, NOW), 2);
    l.spend(NOW - 150);
    l.spend(NOW - 50);
    assert_eq!(
        l.used(b, NOW),
        1,
        "the NOW-150 entry is long outside a 100 s window"
    );
    assert_eq!(l.left(b, NOW), 1);
    l.spend(NOW);
    assert_eq!(l.left(b, NOW), 0);
    // The far edge is CLOSED: at NOW+100 the NOW entry is exactly window_s
    // old and still counts; one second later it has fallen out.
    assert_eq!(
        l.used(b, NOW + 100),
        1,
        "the NOW entry is exactly at the edge and still counts"
    );
    assert_eq!(l.left(b, NOW + 100), 1);
    assert_eq!(
        l.left(b, NOW + 101),
        2,
        "one second past the edge it is out"
    );
    assert_eq!(l.left(b, NOW + 150), 2);
    assert_eq!(l.entries().len(), 3);
    l.spend(NOW + 3 * 86_400);
    assert_eq!(l.entries(), [NOW + 3 * 86_400], "old entries are dropped");
}

// -- the bounds the derived model states, on the real engine ------------------------

/// The real engine against the two bounds `harness_failure_recovery_model`
/// states (`OneInFlight`, `BudgetHeld`), read off the state's public fields
/// rather than from a method that decides.
///
/// The model itself is NOT rewritten here: it lives once, in `aterm-spec`'s
/// registry, is proved there at Tier 0 and is bound to this engine
/// transition-by-transition in `tests/conformance_harness.rs`. A second
/// hand-written copy in this file was two descriptions of one machine that
/// nothing compared.
#[test]
fn the_real_engine_never_exceeds_the_model_bounds_on_the_walk_it_explores() {
    let cfg = LimitsConfig {
        budget: Budget {
            count: 2,
            window_s: 6 * 3600,
        },
        min_dwell_s: 0,
        ..LimitsConfig::default()
    };
    let g = guards(4, true);
    let mut carry = Carry::default();
    let mut seen: BTreeMap<&str, i64> = BTreeMap::new();
    let mut t = NOW;
    for round in 0..4 {
        let mut s = State::new(&five_hour_pair(), t, GEN + round, carry.clone());
        let d = step(&cfg, &s, &g, t, GEN + round);
        s.commit(&d, 1, t);
        // A second class arrives while the action is in flight: queued.
        if s.in_flight.is_some() {
            s.observe(&Event::Evidence(sf("overloaded")), t + 1);
            let queued = step(&cfg, &s, &g, t + 1, GEN + round);
            assert!(matches!(
                queued.decision,
                Decision::Refused {
                    why: Refusal::InFlight
                }
            ));
            s.verdict(Verdict::Executed, t + 2);
        }
        let inflight = i64::from(s.in_flight.is_some());
        let spent = i64::from(s.budget.used(cfg.budget, t + 2));
        assert!(inflight <= 1, "round {round}");
        assert!(spent <= 2, "round {round}: {spent}");
        seen.insert(
            "inflight_max",
            seen.get("inflight_max").copied().unwrap_or(0).max(inflight),
        );
        seen.insert(
            "spent_max",
            seen.get("spent_max").copied().unwrap_or(0).max(spent),
        );
        carry = s.carry();
        t += 100;
    }
    // Negative control: the walk really did spend the budget to the bound.
    assert_eq!(seen.get("spent_max"), Some(&2));
    assert_eq!(seen.get("inflight_max"), Some(&0));
}

// -- the class and the table cursor are one coupled value ---------------------------

#[test]
fn a_renamed_hook_value_enters_the_unknown_ladder_at_its_beginning_and_escalates() {
    let cfg = LimitsConfig::default();
    let g = guards(4, true);
    let window = i64::try_from(cfg.unknown_escalate_after.window_s).expect("small");

    // Walk each class's ladder one step first, so the cursor is NOT at zero
    // when the renamed hook value arrives. `unknown`'s row is one entry long:
    // a cursor of 1 inherited from the old row walks it past its end, and the
    // generation then answers `refused:exhausted` for ever with nobody told.
    for start in [
        five_hour_pair(),
        cls(&[sf("overloaded"), pms()]),
        classification(Class::Weekly7dLimit, false, Some(NOW + 1800)),
    ] {
        let mut s = State::new(&start, NOW, GEN, Carry::default());
        let first = step(&cfg, &s, &g, NOW, GEN);
        if let Decision::Act { .. } = first.decision {
            s.commit(&first, 1, NOW);
        }
        if let Some(f) = s.in_flight.clone() {
            s.verdict(Verdict::Executed, NOW + 1);
            assert_eq!(s.step, f.step + 1, "the ladder really did advance");
        }
        assert!(
            s.step > 0 || s.in_flight.is_some(),
            "{:?} never moved",
            s.class
        );

        s.observe(&Event::Evidence(sf("a_renamed_hook_value")), NOW + 2);
        assert_eq!(s.class, Class::Unknown);
        assert!(s.unpaired, "a value nobody can place is never a pair");
        assert_eq!(s.step, 0, "the cursor goes back to the new row's beginning");
        assert_eq!(s.hits, [NOW + 2], "the unknown window starts here");
        assert_eq!(s.since, NOW + 2, "and so do the class's timers");

        // Two more make the burst; the ladder reaches `escalate`, which the
        // broken cursor turned into silence.
        s.observe(&Event::Evidence(sf("a_renamed_hook_value")), NOW + 3);
        s.observe(&Event::Evidence(sf("a_renamed_hook_value")), NOW + 4);
        assert_eq!(
            action_of(&step(&cfg, &s, &g, NOW + 5, GEN)),
            Some((Action::Escalate, 1)),
            "a renamed hook value must still reach a human, from any class"
        );
    }

    // And the lone one, with no burst behind it, escalates when its window
    // closes rather than answering `refused:exhausted` for ever.
    let mut s = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    let first = step(&cfg, &s, &g, NOW, GEN);
    s.commit(&first, 1, NOW);
    s.verdict(Verdict::Executed, NOW + 1);
    s.observe(&Event::Evidence(sf("a_renamed_hook_value")), NOW + 2);
    assert_eq!(
        step(&cfg, &s, &g, NOW + 3, GEN).decision,
        Decision::Wait {
            until: NOW + 2 + window
        }
    );
    assert_eq!(
        action_of(&step(&cfg, &s, &g, NOW + 2 + window, GEN)),
        Some((Action::Escalate, 1))
    );
}

#[test]
fn a_verdict_that_lands_after_the_class_moved_does_not_advance_the_new_row() {
    let cfg = LimitsConfig::default();
    let g = guards(4, true);
    let mut s = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    let switch = step(&cfg, &s, &g, NOW, GEN);
    assert_eq!(action_of(&switch), Some((Action::SwitchAccount, 4)));
    s.commit(&switch, 1, NOW);
    let f = s
        .in_flight
        .clone()
        .expect("an L4 action awaits its verdict");
    assert_eq!(f.class, Class::Session5hLimit, "the row it indexes into");

    // The class moves while the switch is out. Nothing may be decided until
    // the verdict lands — that is the queue, not a second ladder.
    s.observe(&Event::Evidence(sf("a_renamed_hook_value")), NOW + 1);
    assert_eq!(s.class, Class::Unknown);
    assert_eq!(s.step, 0);
    assert!(matches!(
        step(&cfg, &s, &g, NOW + 2, GEN).decision,
        Decision::Refused {
            why: Refusal::InFlight
        }
    ));

    // The verdict spends the budget the switch really used, and leaves the
    // NEW row at its beginning instead of advancing it past an entry that
    // belongs to a row the state has left.
    s.verdict(Verdict::Executed, NOW + 3);
    assert_eq!(s.step, 0, "the old row's index is not the new row's");
    assert_eq!(
        s.budget.used(cfg.budget, NOW + 3),
        1,
        "the switch is paid for"
    );
    s.observe(&Event::Evidence(sf("a_renamed_hook_value")), NOW + 4);
    s.observe(&Event::Evidence(sf("a_renamed_hook_value")), NOW + 5);
    assert_eq!(
        action_of(&step(&cfg, &s, &g, NOW + 6, GEN)),
        Some((Action::Escalate, 1))
    );

    // NEGATIVE CASE: with the class UNCHANGED the verdict still advances.
    let mut same = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    let switch = step(&cfg, &same, &g, NOW, GEN);
    same.commit(&switch, 1, NOW);
    same.verdict(Verdict::Executed, NOW + 3);
    assert_eq!(same.step, 1, "an unchanged class advances past the action");
}

#[test]
fn a_reclassification_to_another_class_is_adopted_and_never_decides_on_the_old_row() {
    let cfg = LimitsConfig::default();
    let g = guards(4, true);
    // The pinned rows are the sharp end: money and a login are a human's, and
    // the old five-hour row would have relaunched an account for both.
    for (evidence, class, expected) in [
        // CHANGED 2026-09-22: the expected action for a lone auth hook was
        // `escalate`, because one source could not reach an L3 act at all.
        // `relogin` is not one of the four spending actions, so the auth row
        // now decides its own first step — which is the point of the case:
        // whatever it decides, it is never the five-hour row's relaunch.
        (
            vec![sf("authentication_failed")],
            Class::Auth,
            Action::Relogin,
        ),
        (
            vec![
                sf("billing_error"),
                win(WindowKind::SpendLimit, 99.0, None, Source::StatusLine),
            ],
            Class::SpendBilling,
            Action::Escalate,
        ),
    ] {
        let mut s = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
        let switch = step(&cfg, &s, &g, NOW, GEN);
        assert_eq!(action_of(&switch), Some((Action::SwitchAccount, 4)));
        s.commit(&switch, 1, NOW);
        s.verdict(Verdict::Executed, NOW + 1);
        assert_eq!(s.step, 1);

        let later = cls(&evidence);
        assert_eq!(later.class, class);
        s.observe(&Event::Reclassified(later.clone()), NOW + 2);
        assert_eq!(s.class, class, "the latest evidence names the class");
        assert_eq!(s.step, 0, "and its row is walked from the beginning");
        assert_eq!(s.since, NOW + 2);
        assert_eq!(
            s.unpaired, later.unpaired,
            "the adopted reading is the new classification's, not a fold of two"
        );
        assert_eq!(
            action_of(&step(&cfg, &s, &g, NOW + 3, GEN)),
            Some((expected, expected.level())),
            "{class} must decide on its OWN row"
        );
        // The dwell is the one thing that does not reset: a switch that ran
        // stays paid for across the reclassification.
        assert_eq!(s.budget.used(cfg.budget, NOW + 3), 1);
    }

    // NEGATIVE CASE: the SAME class is folded, not adopted — the cursor stays
    // where the ladder left it and the pair stays sticky the safe way.
    let mut s = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    let switch = step(&cfg, &s, &g, NOW, GEN);
    s.commit(&switch, 1, NOW);
    s.verdict(Verdict::Executed, NOW + 1);
    let again = five_hour_pair();
    s.observe(&Event::Reclassified(again), NOW + 2);
    assert_eq!(s.class, Class::Session5hLimit);
    assert_eq!(s.step, 1, "more of the same evidence is not a new ladder");
    assert_eq!(s.since, NOW, "and the class's timers are not restarted");
}

// -- the budget degrades a candidate; it never makes one due ------------------------

#[test]
fn a_dry_switch_budget_waits_for_the_timing_gate_before_it_degrades() {
    let cfg = LimitsConfig::default();
    let g = guards(4, true);
    let t1 = i64::try_from(cfg.transient_t1_s).expect("small");
    let t2 = i64::try_from(cfg.transient_t2_s).expect("small");

    // A transient-capacity state whose row starts with `let-vendor-retry`
    // (done at once), so the next candidate is `switch-model` behind its T1.
    let mut dry = State::new(&cls(&[sf("overloaded"), pms()]), NOW, GEN, Carry::default());
    let first = step(&cfg, &dry, &g, NOW, GEN);
    assert_eq!(action_of(&first), Some((Action::LetVendorRetry, 0)));
    dry.commit(&first, 1, NOW);
    for at in [NOW - 400, NOW - 300, NOW - 200, NOW - 100] {
        dry.budget.spend(at);
    }
    assert_eq!(dry.budget.left(cfg.budget, NOW), 0, "the budget is dry");

    // Before T1 a dry budget must not fire the switch: the timing gate says
    // when, the budget says whether. A degrade here journalled
    // `degraded:budget` at a moment when no switch was a candidate at all.
    assert_eq!(
        step(&cfg, &dry, &g, NOW + 2, GEN).decision,
        Decision::Wait { until: NOW + t1 },
        "a dry budget must not advance the table past its own T1"
    );
    // Between T1 and T2 with no storm the switch is not due either.
    assert_eq!(
        step(&cfg, &dry, &g, NOW + t1, GEN).decision,
        Decision::Wait { until: NOW + t2 }
    );

    // With the storm the switch IS due, and THEN the dry budget degrades it
    // to its L1 half rather than running it.
    dry.storm = true;
    let degraded = step(&cfg, &dry, &g, NOW + t1, GEN);
    match &degraded.decision {
        Decision::Act {
            action,
            level,
            reason,
        } => {
            assert_eq!((*action, *level), (Action::SwitchModel, 1));
            assert!(reason.starts_with("degraded:budget"), "{reason}");
        }
        other => panic!("a due switch on a dry budget must degrade: {other:?}"),
    }

    // NEGATIVE CASE: the same state with budget left runs the switch whole.
    let mut fresh = State::new(&cls(&[sf("overloaded"), pms()]), NOW, GEN, Carry::default());
    let first = step(&cfg, &fresh, &g, NOW, GEN);
    fresh.commit(&first, 1, NOW);
    fresh.storm = true;
    assert_eq!(
        action_of(&step(&cfg, &fresh, &g, NOW + t1, GEN)),
        Some((Action::SwitchModel, 3))
    );
}

// -- the screen readers are the supervisor's, not a second copy ---------------------

#[test]
fn the_auto_continue_phrases_are_read_by_the_supervisors_own_reader() {
    // `banner_class` must agree with `supervise::limit::resumes_by_itself` on
    // every phrase that reader accepts, including the casing and the
    // surrounding text a real banner carries.
    for text in [
        "⚠ Usage limit reached · continuing automatically at 1:50pm · esc to cancel",
        "Continuing Automatically At 1:50pm",
        "continuing shortly",
        "CONTINUING SHORTLY",
    ] {
        assert!(crate::supervise::limit::resumes_by_itself(text), "{text}");
        assert_eq!(
            banner_class(text),
            Some(Class::Session5hLimit),
            "{text} says the vendor resumes by itself"
        );
    }
    // NEGATIVE CASES: a notice that names a reset says no such thing, and a
    // near-miss of the phrase is not the phrase.
    for text in ["resets Sep 19 at 11am", "continuing", "automatically"] {
        assert!(!crate::supervise::limit::resumes_by_itself(text), "{text}");
    }
    assert_eq!(banner_class("continuing"), None);

    // The reason string quotes a banner through ONE line-folding reader and
    // one byte cut: no newline survives, no character is split.
    let long = format!("weekly limit\u{2028}reached  {}", "ω".repeat(200));
    let folded = one_line(&long);
    assert!(
        !folded.contains('\n') && !folded.contains('\u{2028}'),
        "{folded}"
    );
    assert!(folded.len() <= 80, "{} bytes", folded.len());
    assert!(folded.ends_with("..."));
    assert_eq!(one_line("  a \n b  "), "a b");
}

/// F-6 (2026-09-22). `State::hits` was pushed to and never trimmed except on
/// a class CHANGE, so a session parked on one class grew one `i64` per
/// failure for its whole life — and both readers walk it linearly on every
/// event. Retention is now the same shape `Ledger::spend` twelve lines away
/// already had.
#[test]
fn the_hit_vector_is_retained_and_the_escalation_anchor_survives_the_retention() {
    let cfg = LimitsConfig::default();
    let g = guards(4, true);
    let mut s = State::new(&cls(&[sf("invalid_request")]), NOW, GEN, Carry::default());
    // Three days of one failure an hour, all on the SAME class.
    let mut t = NOW;
    for _ in 0..(3 * 24) {
        t += 3600;
        s.observe(&Event::Evidence(sf("invalid_request")), t);
    }
    assert_eq!(s.class, Class::Unknown, "the class never changed");
    // Bounded by the retention window, not by the session's life.
    assert!(
        s.hits.len() <= 49,
        "two days of hourly hits at most, held {}",
        s.hits.len()
    );
    assert!(
        s.hits.iter().all(|h| t - *h <= HITS_RETAIN_S),
        "nothing older than the retention window survives"
    );
    // The ANCHOR is the FIRST hit of the class and retention cannot move it:
    // anchoring on the surviving hits is what made a lone `unknown` wait for
    // ever, and a naive retain would have re-broken it.
    assert_eq!(s.hits_anchor, Some(NOW));
    assert!(
        s.hits_anchor < s.hits.iter().copied().min(),
        "the anchor is older than every surviving hit — the exact case a \
         `hits.iter().min()` reader gets wrong"
    );
    assert_eq!(
        action_of(&step(&cfg, &s, &g, t, GEN)),
        Some((Action::Escalate, 1)),
        "the first hit's window closed two days ago"
    );

    // NEGATIVE CONTROL: a class CHANGE still clears both, so the new class's
    // deadline counts from when IT was first seen and not from the old
    // class's first event.
    let mut c = State::new(&five_hour_pair(), NOW, GEN, Carry::default());
    assert_eq!(c.hits_anchor, Some(NOW));
    c.observe(&Event::Evidence(sf("rate_limit_v9")), NOW + 10);
    assert_eq!(c.class, Class::Unknown);
    assert_eq!(c.hits, [NOW + 10]);
    assert_eq!(c.hits_anchor, Some(NOW + 10));
}

// -- the painted windows: the producer rank 1 never had -----------------------

/// The `/usage` panel Claude Code 2.1.278 painted in a real 120-column
/// session, captured over aterm's control socket 2026-09-22.
const PANEL: &str = "\
   Current session
   ███▌                                               7% used
   Resets 1:20pm (America/Los_Angeles)

   Current week (all models)
   ███████████████████████████████                    62% used
   Resets Sep 23 at 12pm

   Current week (Fable)
   ██████████████████████████████████████████████████ 100% used
   Resets Sep 23 at 11:59am
";

fn panel_rows(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

#[test]
fn windows_from_screen_constructs_the_grid_source_nothing_used_to_build() {
    let got = Evidence::windows_from_screen(&panel_rows(PANEL), NOW, 0, zone_none);
    let kinds: Vec<WindowKind> = got
        .iter()
        .filter_map(|e| match e {
            Evidence::Window { which, .. } => Some(*which),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            WindowKind::FiveHour,
            WindowKind::SevenDay,
            // The bucket §5.8.2 recorded as never readable. The grid is its
            // ONLY source: no statusLine and no cache carries it.
            WindowKind::SevenDayOverageIncluded,
        ]
    );
    for e in &got {
        let Evidence::Window {
            source,
            used_pct,
            resets_at,
            age_s,
            ..
        } = e
        else {
            unreachable!("only windows come back")
        };
        assert_eq!(*source, Source::Grid);
        assert!(used_pct.is_some(), "a painted figure is a figure");
        assert!(resets_at.is_some(), "every painted row carried a reset");
        assert_eq!(*age_s, Some(0), "a grid read is the instant it was taken");
    }
    // NEGATIVE CONTROL: a screen with no panel on it yields nothing, so the
    // reader cannot be passing by finding windows everywhere.
    assert!(
        Evidence::windows_from_screen(
            &panel_rows("⏺ done\n  ⎿  7% until auto-compact\n"),
            NOW,
            0,
            zone_none
        )
        .is_empty()
    );
}

#[test]
fn a_painted_window_classifies_and_pairs_exactly_as_a_statusline_one_does() {
    let painted = Evidence::windows_from_screen(&panel_rows(PANEL), NOW, 0, zone_none);
    // The Fable window is at 100 %, and a banner that names the failure
    // completes the pair on a DIFFERENT channel.
    let mut ev = painted.clone();
    ev.push(sfd("rate_limit", MODEL_BUCKET_MARKER));
    let c = classify(&ev, NOW).expect("a classification");
    assert_eq!(c.class, Class::ModelBucketLimit);
    assert!(!c.unpaired, "hook value + painted window is a pair");

    // NEGATIVE CONTROL 1: the painted windows ALONE name no failure — a
    // percentage corroborates, it does not accuse.
    let alone = classify(&painted, NOW);
    assert!(
        alone.is_none_or(|c| c.unpaired || c.class == Class::Unknown),
        "windows alone must not name a class as if they were a pair"
    );
    // NEGATIVE CONTROL 2: two readings of ONE frame are ONE source, so a
    // banner off the same grid does not pair with a painted window.
    let mut same_frame = painted.clone();
    same_frame.push(banner("You've hit your Fable limit"));
    let c = classify(&same_frame, NOW).expect("a classification");
    assert!(
        c.unpaired,
        "banner + painted window are both the grid channel"
    );
}
