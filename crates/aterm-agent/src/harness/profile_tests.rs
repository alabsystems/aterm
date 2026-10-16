// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the allowlist reader of Claude Code's `.claude.json`
//! ([`super::parse`], [`super::read`]), the account key, the plan words and
//! the Fable plan answer. Every file here is composed: invented example.com
//! names, invented ids, and a sentinel wherever the real file can hold a
//! credential or a person's name. The allocation-size control for large escaped
//! unknown values is `tests/profile_sentinels.rs`; it never reads allocator
//! payloads or uninitialized spare capacity.

use aterm_json::{Value, json};
use aterm_tempfile::TempDir as Tmp;

use super::*;

/// The account id the composed profiles carry (invented).
const UUID: &str = "0f6c2b8e-1d3a-4c5e-9b7f-2a4d6e8c0b1a";

/// The text planted wherever a value must never be kept.
const SENTINEL: &str = "S3NT1NEL";

/// 2026-10-15T00:00:00Z.
const DAY: i64 = 1_792_022_400;

fn tmp_dir(tag: &str) -> Tmp {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-profile-{tag}-"))
        .tempdir()
        .expect("temp dir")
}

/// A whole composed profile: every allowlisted field, and a sentinel in
/// every credential and identity field the real file can hold.
fn composed() -> Value {
    json!({
        "numStartups": 42,
        "primaryApiKey": format!("sk-ant-{SENTINEL}-primary"),
        "customApiKeyResponses": {"approved": [format!("{SENTINEL}-approved")], "rejected": []},
        "mcpServers": {
            "tracker": {
                "command": "tracker-mcp",
                "env": {"TRACKER_TOKEN": format!("{SENTINEL}-env")},
                "headers": {"Authorization": format!("Bearer {SENTINEL}-header")}
            }
        },
        "projects": {
            "/work/example": {
                "mcpServers": {"db": {"env": {"DB_PASSWORD": format!("{SENTINEL}-project")}}},
                "history": [{"display": format!("{SENTINEL} typed here")}]
            }
        },
        "oauthAccount": {
            "accountUuid": UUID,
            "emailAddress": format!("ana.{SENTINEL}@example.com"),
            "displayName": format!("Ana {SENTINEL}"),
            "fullName": format!("Ana {SENTINEL} Example"),
            "organizationName": format!("{SENTINEL} Example Org"),
            "organizationUuid": format!("{SENTINEL}-org-uuid"),
            "organizationRole": "admin",
            "organizationType": "claude_max",
            "userRateLimitTier": "default_claude_max_5x",
            "organizationRateLimitTier": "default_claude_max_20x",
            "seatTier": "standard",
            "billingType": "stripe_subscription",
            "subscriptionCreatedAt": "2026-01-02T03:04:05.678Z",
            "hasExtraUsageEnabled": true,
            "planDisplayName": "Max 5x",
            "profileFetchedAt": 1_792_000_000_123_i64
        },
        "cachedExtraUsageDisabledReason": null,
        "cachedUsageUtilization": {
            "fetchedAtMs": 1_792_050_000_000_i64,
            "accountUuid": UUID,
            "utilization": {
                "five_hour": {"utilization": 62.5, "resets_at": "2026-10-15T18:05:00.123456+00:00",
                              "used_dollars": 1.0},
                "seven_day": {"utilization": 71, "resets_at": "2026-10-18T20:50:00Z"},
                "seven_day_opus": null,
                "seven_day_sonnet": {"utilization": 3.04, "resets_at": null},
                "limits": [
                    {"kind": "weekly_scoped", "group": "weekly", "percent": 40,
                     "resets_at": "2026-10-18T20:50:00+00:00", "severity": "normal", "is_active": true,
                     "scope": {"model": {"display_name": "Fable"}}},
                    {"kind": "weekly_scoped", "group": "weekly", "percent": 10,
                     "resets_at": null, "scope": {"model": {"display_name": "Fable 5.1"}}},
                    {"kind": "weekly_scoped", "group": "weekly", "percent": 5,
                     "resets_at": null, "scope": {"model": {"display_name": "Haiku"}}},
                    {"kind": "weekly_all", "group": "weekly", "percent": 71, "resets_at": null},
                    "not an object"
                ],
                "extra_usage": {"is_enabled": true, "monthly_limit": 20_000, "used_credits": 4_100,
                                "utilization": 20.5, "currency": "USD", "disabled_reason": null},
                "cinder_cove": {"utilization": 12, "resets_at": "2026-11-01"},
                "wattle_ember": {"utilization": 50, "resets_at": "2026-12-01T00:00:00Z",
                                 "label": format!("{SENTINEL} label"), "limit_dollars": 50,
                                 "used_dollars": 25.5, "remaining_dollars": 24.5}
            }
        },
        "cachedGrowthBookFeatures": {
            "tengu_saffron_credits_only_tiers": ["enterprise"],
            "tengu_saffron_lattice": {},
            "tengu_other_flag": {"token": format!("{SENTINEL}-flag")}
        }
    })
}

fn parsed(v: &Value) -> Profile {
    parse(aterm_json::to_string(v).expect("compose").as_bytes()).expect("a profile")
}

/// `v` with `path` (keys, top down) set to `to`.
fn with(mut v: Value, path: &[&str], to: Value) -> Value {
    let mut at = &mut v;
    for key in path.get(..path.len() - 1).unwrap_or(&[]) {
        at = at
            .as_object_mut()
            .and_then(|o| o.get_mut(*key))
            .expect("composed path");
    }
    let last = path.last().expect("a key");
    at.as_object_mut()
        .expect("an object")
        .insert((*last).to_owned(), to);
    v
}

/// `v` without `path`'s last key.
fn without(mut v: Value, path: &[&str]) -> Value {
    let mut at = &mut v;
    for key in path.get(..path.len() - 1).unwrap_or(&[]) {
        at = at
            .as_object_mut()
            .and_then(|o| o.get_mut(*key))
            .expect("composed path");
    }
    at.as_object_mut()
        .expect("an object")
        .remove(*path.last().expect("a key"));
    v
}

fn word(text: &str) -> Option<Word> {
    Word::new(text)
}

// ---------------------------------------------------------------------------
// The allowlist
// ---------------------------------------------------------------------------

/// Every allowlisted field of a whole composed profile reads, in aterm's
/// words.
#[test]
fn a_composed_profile_reads_every_allowlisted_field() {
    let p = parsed(&composed());
    assert_eq!(p.key, AccountKey::claude(UUID));
    assert_eq!(
        p.plan,
        Plan {
            word: Some(PlanWord::Max),
            tier: word("5x"),
            seat: word("standard"),
            billing: word("stripe_subscription"),
            display: word("Max 5x"),
            subscribed_at: Some(1_767_323_045),
            extra_usage: Some(true),
            fetched_at: Some(1_792_000_000),
        }
    );
    assert_eq!(p.credits, Credits::On);
    assert_eq!(p.fable, FablePlan::Included);
    let usage = p.usage.expect("the cached body names this account");
    assert_eq!(usage.fetched_at_ms, 1_792_050_000_000);
    /// A window as compared: bucket, name, tenths, reset.
    type Row<'a> = (&'a str, Option<&'a str>, Option<u16>, Option<i64>);
    let windows: Vec<Row<'_>> = usage
        .windows
        .iter()
        .map(|w| {
            (
                w.bucket.id(),
                w.name.as_ref().map(Word::as_str),
                w.used_pct_x10,
                w.resets_at,
            )
        })
        .collect();
    assert_eq!(
        windows,
        vec![
            ("five_hour", None, Some(625), Some(DAY + 18 * 3600 + 5 * 60)),
            (
                "seven_day",
                None,
                Some(710),
                Some(DAY + 3 * 86_400 + 20 * 3600 + 50 * 60)
            ),
            ("seven_day_sonnet", None, Some(30), None),
            (
                "seven_day_overage_included",
                Some("Fable"),
                Some(400),
                Some(DAY + 3 * 86_400 + 20 * 3600 + 50 * 60)
            ),
            (
                "seven_day_overage_included",
                Some("Fable 5.1"),
                Some(100),
                None
            ),
            ("seven_day_other", Some("Haiku"), Some(50), None),
        ],
        "the body's windows, then its weekly_scoped rows; `null` opus is none"
    );
    assert_eq!(
        usage.credits,
        Some(CreditsMoney {
            enabled: Some(true),
            cap_minor: Some(20_000),
            spent_minor: Some(4_100),
            used_pct_x10: Some(205),
            currency: word("USD"),
            disabled_reason: None,
        })
    );
    assert_eq!(
        usage.one_time,
        vec![
            OneTime {
                kind: OneTimeKind::CodeAndCowork,
                used_pct_x10: Some(120),
                resets_at: Some(DAY + 17 * 86_400),
                used_usd: None,
                limit_usd: None,
            },
            OneTime {
                kind: OneTimeKind::Credit,
                used_pct_x10: Some(500),
                resets_at: Some(DAY + 47 * 86_400),
                used_usd: Some(25.5),
                limit_usd: Some(50.0),
            },
        ]
    );
}

/// THE SENTINEL TEST (design §8.4, A0): nothing the reader answers holds any
/// part of a credential, an identity field or the account id. NEGATIVE
/// CONTROL: a reader of the whole file (the precedent's `Value`) holds
/// every one of them, so the sentinels are where a reader could reach them.
#[test]
fn no_credential_identity_or_id_reaches_what_is_answered() {
    let text = aterm_json::to_string(&composed()).expect("compose");
    let answered = format!("{:?}", parse(text.as_bytes()).expect("a profile"));
    assert!(
        !answered.contains(SENTINEL),
        "a sentinel was kept: {answered}"
    );
    assert!(
        !answered.contains(UUID),
        "the account id was kept: {answered}"
    );
    assert!(
        !answered.contains("example.com"),
        "the email was kept: {answered}"
    );

    let whole = format!(
        "{:?}",
        aterm_json::from_slice::<Value>(text.as_bytes()).expect("JSON")
    );
    for planted in [
        "-primary",
        "-approved",
        "-env",
        "-header",
        "-project",
        "ana.",
        "-org-uuid",
        "-flag",
    ] {
        assert!(
            whole.contains(&format!("{SENTINEL}{planted}"))
                || whole.contains(&format!("{planted}{SENTINEL}")),
            "the control reader holds {planted}"
        );
    }
    assert!(whole.contains(UUID), "the control reader holds the id");
}

/// A file that is not one JSON object is the one word `unreadable` — its
/// sentinels with it, never the parser's message.
#[test]
fn a_file_that_is_not_json_is_one_word() {
    for bad in [
        format!(
            r#"{{"primaryApiKey": "{SENTINEL}", "oauthAccount": {{"organizationType": "claude_max"}}"#
        ),
        format!(r#"{{"primaryApiKey": "{SENTINEL}",}}"#),
        format!(r#"["{SENTINEL}"]"#),
        format!(r#""{SENTINEL}""#),
        String::new(),
    ] {
        let answer = parse(bad.as_bytes());
        assert_eq!(answer, Err(NoProfile::Unreadable), "{bad}");
        assert_eq!(NoProfile::Unreadable.word(), "unreadable");
    }
}

/// A kept field of another type reads as absent; the rest of the file
/// still reads. A vendor build that changes one field's shape costs that
/// field, never the profile.
#[test]
fn a_field_of_another_shape_reads_absent_and_the_rest_still_reads() {
    let v = with(composed(), &["oauthAccount", "organizationType"], json!(5));
    let v = with(v, &["oauthAccount", "seatTier"], json!({"nested": [1, 2]}));
    let v = with(v, &["oauthAccount", "hasExtraUsageEnabled"], json!("yes"));
    let v = with(
        v,
        &["cachedUsageUtilization", "utilization", "five_hour"],
        json!([62.5]),
    );
    let v = with(
        v,
        &["cachedGrowthBookFeatures", "tengu_saffron_lattice"],
        json!("on"),
    );
    let p = parsed(&v);
    assert_eq!(p.plan.word, None);
    assert_eq!(p.plan.seat, None);
    assert_eq!(p.plan.extra_usage, None);
    assert_eq!(
        p.plan.tier,
        word("5x"),
        "the rest of oauthAccount still reads"
    );
    assert_eq!(p.key, AccountKey::claude(UUID));
    let usage = p.usage.expect("the rest of the body still reads");
    assert!(usage.windows.iter().all(|w| w.bucket != Bucket::FiveHour));
    assert!(usage.windows.iter().any(|w| w.bucket == Bucket::SevenDay));
    assert_eq!(
        p.fable,
        FablePlan::Unknown,
        "no plan word, and a lattice of no shape"
    );

    let p = parsed(&with(composed(), &["oauthAccount"], json!("signed out")));
    assert_eq!((p.key, p.plan), (None, Plan::default()));
    assert_eq!(
        p.usage, None,
        "a body no profile key vouches for is dropped"
    );
}

/// Text kept from the file is printable ASCII and short, or nothing.
#[test]
fn a_kept_text_is_a_short_printable_word_or_nothing() {
    let v = with(
        composed(),
        &["oauthAccount", "billingType"],
        json!("stripe\u{1b}[31m"),
    );
    let v = with(
        v,
        &["oauthAccount", "planDisplayName"],
        json!("M".repeat(WORD_MAX + 1)),
    );
    let p = parsed(&v);
    assert_eq!(p.plan.billing, None, "an escape sequence is not a word");
    assert_eq!(p.plan.display, None, "past WORD_MAX is not a word");
    assert_eq!(
        word(&"M".repeat(WORD_MAX)).map(|w| w.as_str().len()),
        Some(WORD_MAX)
    );
    assert_eq!(word(""), None);
}

/// The served tier: the user's, else the organization's; a trailing
/// multiple is the multiple, any other tier its own word.
#[test]
fn the_tier_is_the_users_else_the_organizations() {
    let p = parsed(&without(composed(), &["oauthAccount", "userRateLimitTier"]));
    assert_eq!(p.plan.tier, word("20x"));
    let p = parsed(&with(
        composed(),
        &["oauthAccount", "userRateLimitTier"],
        json!("default_claude_zero"),
    ));
    assert_eq!(p.plan.tier, word("default_claude_zero"));
    for (raw, tier) in [
        ("default_claude_max_5x", "5x"),
        ("default_claude_max_20x", "20x"),
        ("default_raven", "default_raven"),
        ("tier_x", "tier_x"),
        ("tier_", "tier_"),
    ] {
        assert_eq!(
            tier_word(Word::new(raw).expect("word")).as_str(),
            tier,
            "{raw}"
        );
    }
}

/// The plan word from every organization type Claude Code maps, any other
/// as `other`, none without the field.
#[test]
fn the_plan_word_is_claude_codes_mapping() {
    for (raw, plan, title) in [
        ("claude_max", PlanWord::Max, "Max"),
        ("claude_pro", PlanWord::Pro, "Pro"),
        ("claude_team", PlanWord::Team, "Team"),
        ("claude_enterprise", PlanWord::Enterprise, "Enterprise"),
        ("claude_zero", PlanWord::Other, "other plan"),
    ] {
        let p = parsed(&with(
            composed(),
            &["oauthAccount", "organizationType"],
            json!(raw),
        ));
        assert_eq!(p.plan.word, Some(plan), "{raw}");
        assert_eq!(plan.title(), title);
    }
    let p = parsed(&without(composed(), &["oauthAccount", "organizationType"]));
    assert_eq!(p.plan.word, None);
}

/// The credits word: absent is unknown, `null` on, `out_of_credits` out,
/// any other reason off with its reason.
#[test]
fn the_credits_word_follows_the_cached_reason() {
    let key = ["cachedExtraUsageDisabledReason"];
    for (value, credits, state) in [
        (Some(json!(null)), Credits::On, Some("on")),
        (Some(json!("out_of_credits")), Credits::Out, Some("out")),
        (
            Some(json!("org_level_disabled")),
            Credits::Off(word("org_level_disabled")),
            Some("off"),
        ),
        (Some(json!("\u{7}")), Credits::Off(None), Some("off")),
        (Some(json!(7)), Credits::Unknown, None),
        (None, Credits::Unknown, None),
    ] {
        let v = match &value {
            Some(to) => with(composed(), &key, to.clone()),
            None => without(composed(), &key),
        };
        let p = parsed(&v);
        assert_eq!(p.credits, credits, "{value:?}");
        assert_eq!(p.credits.state(), state);
    }
    assert_eq!(
        Credits::Off(word("org_level_disabled"))
            .reason()
            .map(Word::as_str),
        Some("org_level_disabled")
    );
}

/// The cached body is kept only where it names the profile's own account,
/// as Claude Code itself drops it; and only with its fetch time.
#[test]
fn a_cached_body_of_another_account_is_dropped() {
    let other = with(
        composed(),
        &["cachedUsageUtilization", "accountUuid"],
        json!("7a1e0c3b-5d2f-4e6a-8c9b-1f3e5a7c9d0b"),
    );
    assert_eq!(parsed(&other).usage, None);
    let unnamed = without(composed(), &["cachedUsageUtilization", "accountUuid"]);
    assert_eq!(parsed(&unnamed).usage, None);
    let undated = without(composed(), &["cachedUsageUtilization", "fetchedAtMs"]);
    assert_eq!(parsed(&undated).usage, None);
    assert!(
        parsed(&composed()).usage.is_some(),
        "control: the same body, its own account"
    );
}

// ---------------------------------------------------------------------------
// The Fable plan answer
// ---------------------------------------------------------------------------

fn fable_of(plan: &str, seat: &str, features: Value) -> FablePlan {
    fable_at(plan, seat, "default_claude_max_5x", features)
}

fn fable_at(plan: &str, seat: &str, tier: &str, features: Value) -> FablePlan {
    let v = with(
        composed(),
        &["oauthAccount", "organizationType"],
        json!(plan),
    );
    let v = with(v, &["oauthAccount", "seatTier"], json!(seat));
    let v = with(v, &["oauthAccount", "userRateLimitTier"], json!(tier));
    parsed(&with(v, &["cachedGrowthBookFeatures"], features)).fable
}

/// Every answer the two flags give, decided as Claude Code 2.1.301 decides
/// whether Fable is billed to usage credits; a renamed or reshaped flag is
/// `unknown`, never a guess.
#[test]
fn the_fable_plan_answer_follows_the_two_flags() {
    let tiers = "tengu_saffron_credits_only_tiers";
    let lattice = "tengu_saffron_lattice";
    let ends = DAY + 17 * 86_400;
    for (plan, seat, features, answer) in [
        (
            "claude_max",
            "standard",
            json!({tiers: ["enterprise"], lattice: {}}),
            FablePlan::Included,
        ),
        (
            "claude_max",
            "standard",
            json!({tiers: ["max"], lattice: {}}),
            FablePlan::CreditsOnly,
        ),
        (
            "claude_pro",
            "standard",
            json!({tiers: ["max"], lattice: {}}),
            FablePlan::Included,
        ),
        (
            "claude_max",
            "standard",
            json!({tiers: [], lattice: {"planLimitsEndDate": "2026-11-01"}}),
            FablePlan::LimitsEnd(Some(ends)),
        ),
        (
            "claude_max",
            "standard",
            json!({tiers: [], lattice: {"planLimitsEndDate": "soon"}}),
            FablePlan::LimitsEnd(None),
        ),
        (
            "claude_max",
            "standard",
            json!({tiers: [], lattice: {"overageConsentRequired": true}}),
            FablePlan::CreditsOnly,
        ),
        (
            "claude_max",
            "standard",
            json!({tiers: [], lattice: {"enabled": false, "planLimitsEndDate": "2026-11-01"}}),
            FablePlan::Included,
        ),
        (
            "claude_max",
            "standard",
            json!({tiers: [], lattice: {"overageConsentRequired": true,
                                        "perModel": {"claude-fable-5": {"enabled": false}}}}),
            FablePlan::Included,
        ),
        (
            "claude_max",
            "standard",
            json!({tiers: [], lattice: {"perModel": {"claude-sonnet-5": {"enabled": true},
                                                     "Claude-FABLE-5-1": {"enabled": true}}}}),
            FablePlan::CreditsOnly,
        ),
        (
            "claude_max",
            "standard",
            json!({tiers: [], lattice: {"perModel": {"claude-sonnet-5": {"enabled": true}}}}),
            FablePlan::Included,
        ),
        (
            "claude_enterprise",
            "standard",
            json!({tiers: [], lattice: {}}),
            FablePlan::CreditsOnly,
        ),
        (
            "claude_enterprise",
            "enterprise_usage_based",
            json!({tiers: [], lattice: {}}),
            FablePlan::Included,
        ),
        (
            "claude_max",
            "standard",
            json!({lattice: {}}),
            FablePlan::Unknown,
        ),
        (
            "claude_max",
            "standard",
            json!({tiers: ["pro"]}),
            FablePlan::Unknown,
        ),
        (
            "claude_max",
            "standard",
            json!({tiers: "max", lattice: {}}),
            FablePlan::Unknown,
        ),
        (
            "claude_max",
            "standard",
            json!({tiers: ["max", 5], lattice: {}}),
            FablePlan::Unknown,
        ),
        (
            "claude_zero",
            "standard",
            json!({tiers: [], lattice: {}}),
            FablePlan::Unknown,
        ),
    ] {
        assert_eq!(
            fable_of(plan, seat, features.clone()),
            answer,
            "{plan} {seat} {features:?}"
        );
    }
    let none = parsed(&without(composed(), &["cachedGrowthBookFeatures"]));
    assert_eq!(none.fable, FablePlan::Unknown);
    assert_eq!(
        [
            FablePlan::Included.word(),
            FablePlan::CreditsOnly.word(),
            FablePlan::LimitsEnd(Some(ends)).word(),
            FablePlan::Unknown.word(),
        ],
        ["included", "credits-only", "limits-end", "unknown"]
    );
    assert_eq!(FablePlan::LimitsEnd(Some(ends)).ends_at(), Some(ends));
}

/// The answer is decided in Claude Code's order: a login billed per request
/// (an Enterprise seat `enterprise_usage_based`, the tier
/// `default_claude_zero`) is included whatever the flags say — even the
/// vendor's default tiers flag, `["enterprise"]`; a plan in the tiers flag
/// is credits only with no lattice at all; and a lattice whose plan-wide
/// keys Claude Code's parse would refuse is `unknown`, never read as half a
/// flag.
#[test]
fn the_fable_plan_answer_is_decided_in_claude_codes_order() {
    let tiers = "tengu_saffron_credits_only_tiers";
    let lattice = "tengu_saffron_lattice";
    let max = "default_claude_max_5x";
    for (plan, seat, tier, features, answer) in [
        (
            "claude_enterprise",
            "enterprise_usage_based",
            "default_raven",
            json!({tiers: ["enterprise"], lattice: {"overageConsentRequired": true}}),
            FablePlan::Included,
        ),
        (
            "claude_max",
            "standard",
            "default_claude_zero",
            json!({tiers: ["max"], lattice: {"planLimitsEndDate": "2026-11-01"}}),
            FablePlan::Included,
        ),
        (
            "claude_max",
            "standard",
            "default_claude_zero",
            json!({}),
            FablePlan::Included,
        ),
        (
            "claude_max",
            "standard",
            max,
            json!({tiers: ["max"]}),
            FablePlan::CreditsOnly,
        ),
        (
            "claude_max",
            "standard",
            max,
            json!({tiers: [], lattice: {"enabled": "yes", "overageConsentRequired": true}}),
            FablePlan::Unknown,
        ),
        (
            "claude_max",
            "standard",
            max,
            json!({tiers: [], lattice: {"overageConsentRequired": null}}),
            FablePlan::Unknown,
        ),
        (
            "claude_max",
            "standard",
            max,
            json!({tiers: [], lattice: {"planLimitsEndDate": 20_261_101}}),
            FablePlan::Unknown,
        ),
        (
            "claude_max",
            "standard",
            max,
            json!({tiers: [], lattice: {"overageConsentRequired": true, "perModel": [1]}}),
            FablePlan::CreditsOnly,
        ),
    ] {
        assert_eq!(
            fable_at(plan, seat, tier, features.clone()),
            answer,
            "{plan} {seat} {tier} {features:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The key
// ---------------------------------------------------------------------------

/// The key is design §3.1's hash, computed here independently: one id gives
/// one key wherever it is read, the agent is in the hash, and the id is in
/// no part of what a key prints.
#[test]
fn the_key_is_the_designs_hash_of_the_id() {
    let mut want = aterm_digest::Sha256::new();
    want.update(b"aterm-account-v1\0claude\0");
    want.update(UUID);
    let want = format!(
        "c-{}",
        aterm_codec::hex::encode_lower(want.finalize().get(..6).expect("digest"))
    );
    let key = AccountKey::claude(UUID).expect("a key");
    assert_eq!(key.to_string(), want);
    assert_eq!(
        format!("{key:?}"),
        want,
        "Debug prints the key, never the id"
    );
    assert_eq!(key.short(), want.get(2..8).expect("six digits"));
    assert_eq!(AccountKey::claude(UUID), Some(key), "one id, one key");
    assert_ne!(
        AccountKey::claude("1f6c2b8e-1d3a-4c5e-9b7f-2a4d6e8c0b1a"),
        Some(key)
    );

    let mut codex = aterm_digest::Sha256::new();
    codex.update(b"aterm-account-v1\0codex\0acct-example\0user-example");
    let codex_want = format!(
        "x-{}",
        aterm_codec::hex::encode_lower(codex.finalize().get(..6).expect("digest"))
    );
    let codex_key = AccountKey::codex("acct-example", "user-example").expect("a key");
    assert_eq!(codex_key.to_string(), codex_want);
    assert_ne!(
        AccountKey::codex("acct-example", "user-other"),
        Some(codex_key),
        "a Team workspace's members share the account id: the pair is the key"
    );
    assert_eq!(AccountKey::claude(""), None);
    assert_eq!(AccountKey::codex("acct-example", ""), None);
    assert_eq!(AccountKey::claude(&"a".repeat(129)), None);
}

// ---------------------------------------------------------------------------
// Instants
// ---------------------------------------------------------------------------

/// Every instant shape the vendor writes: UTC with and without a fraction,
/// an offset applied, a bare date at midnight UTC; anything else none.
#[test]
fn instants_read_in_every_shape_the_vendor_writes() {
    let at = DAY + 18 * 3600 + 5 * 60;
    for (text, want) in [
        ("2026-10-15T18:05:00Z", Some(at)),
        ("2026-10-15T18:05:00.123Z", Some(at)),
        ("2026-10-15T18:05:00.123456+00:00", Some(at)),
        ("2026-10-15T20:05:00+02:00", Some(at)),
        ("2026-10-15T13:35:00.5-04:30", Some(at)),
        ("2026-10-15", Some(DAY)),
        ("2026-10-15T18:05:00", None),
        ("2026-10-15T18:05:00+24:00", None),
        ("2026-10-15T18:05:00+0200", None),
        ("2026-13-15", None),
        ("soon", None),
        ("", None),
    ] {
        assert_eq!(instant(text), want, "{text}");
    }
}

// ---------------------------------------------------------------------------
// Where, and the read
// ---------------------------------------------------------------------------

/// The profile's path is aterm's home rule: beside the user's own
/// `~/.claude` with no `CLAUDE_CONFIG_DIR`, inside any set one — the user's
/// own `~/.claude` named by it included, as the real-Claude lab launches
/// Claude Code (`CLAUDE_CONFIG_DIR || homedir()`) — in `$HOME` for an empty
/// one, whose directory need not be known, beside another `HOME`'s `.claude`
/// for a process with its own.
#[test]
fn the_profile_path_follows_the_home_rule() {
    let user = Path::new("/home/ana");
    let work = || Some(PathBuf::from("/work/repo"));
    assert_eq!(
        path_of(user, None, None, work),
        Some(user.join(".claude.json"))
    );
    assert_eq!(
        path_of(user, Some("/srv/claude-b"), None, work),
        Some(PathBuf::from("/srv/claude-b/.claude.json"))
    );
    assert_eq!(
        path_of(user, Some("/home/ana/.claude"), None, work),
        Some(user.join(".claude/.claude.json")),
        "a set home keeps its profile inside it, the user's own ~/.claude too"
    );
    assert_eq!(
        path_of(user, Some(""), None, || None),
        Some(user.join(".claude.json")),
        "an empty home's profile is $HOME's wherever its records are"
    );
    assert_eq!(
        path_of(user, Some("rel/claude"), None, work),
        Some(PathBuf::from("/work/repo/rel/claude/.claude.json"))
    );
    assert_eq!(
        path_of(user, Some(""), None, work),
        Some(user.join(".claude.json"))
    );
    assert_eq!(
        path_of(user, None, Some("/home/other"), work),
        Some(PathBuf::from("/home/other/.claude.json"))
    );
    assert_eq!(path_of(user, Some("rel/claude"), None, || None), None);
    assert_eq!(
        path_in(user, None, &user.join(".claude")),
        user.join(".claude.json")
    );
    assert_eq!(
        path_in(user, Some("/home/ana/.claude"), &user.join(".claude")),
        user.join(".claude/.claude.json"),
        "named explicitly, ~/.claude keeps its own profile inside it"
    );
    assert_eq!(
        path_in(user, Some("/srv/claude-b"), Path::new("/srv/claude-b")),
        PathBuf::from("/srv/claude-b/.claude.json")
    );
    assert_eq!(
        path_in(user, Some(""), Path::new("/work/repo")),
        user.join(".claude.json"),
        "an empty home's records are where it runs, its profile in $HOME"
    );
}

/// The file Claude Code reads for a home ([`effective`], 2.1.301's `v_()`):
/// a `.config.json` in the records directory that holds a byte stands in
/// for the plain profile, present or not, a link to one too; an empty one
/// does only while it has a write bit and no plain file is there; and a
/// custom OAuth URL renames the plain file, whose absence is then the
/// renamed one's. CONTROL: no `.config.json`, an empty read-only one, an
/// empty writable one beside a plain file, a directory by that name, and an
/// unknown records directory each leave the plain file.
#[cfg(unix)]
#[test]
fn the_file_read_is_the_one_claude_code_picks() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tmp_dir("effective");
    let home = tmp.path().join("home");
    let records = home.join(".claude");
    std::fs::create_dir_all(&records).expect("records");
    let plain = home.join(".claude.json");
    let legacy = records.join(".config.json");
    let custom = home.join(".claude-custom-oauth.json");
    let pick = |custom_oauth| effective(&plain, Some(&records), custom_oauth);
    let mode = |path: &Path, bits| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).expect("chmod");
    };

    // CONTROL: no `.config.json`, the plain file, there or not.
    assert_eq!(pick(false), plain);
    std::fs::write(&plain, "{}").expect("plain");
    assert_eq!(pick(false), plain);

    // One that holds a byte wins, whatever its mode and the plain file's.
    std::fs::write(&legacy, "{}").expect("legacy");
    assert_eq!(pick(false), legacy);
    mode(&legacy, 0o444);
    assert_eq!(pick(false), legacy, "read-only, and not empty");
    std::fs::remove_file(&plain).expect("rm");
    assert_eq!(pick(false), legacy);

    // An empty one: only with a write bit, and no plain file.
    std::fs::remove_file(&legacy).expect("rm");
    std::fs::write(&legacy, "").expect("legacy");
    mode(&legacy, 0o444);
    assert_eq!(pick(false), plain, "CONTROL: empty and read-only");
    mode(&legacy, 0o644);
    assert_eq!(pick(false), legacy, "empty, writable, no plain file");
    std::fs::write(&plain, "{}").expect("plain");
    assert_eq!(pick(false), plain, "CONTROL: empty, beside a plain file");

    // A custom OAuth URL: the renamed file, whose absence counts.
    assert_eq!(pick(true), legacy, "no .claude-custom-oauth.json");
    std::fs::write(&custom, "{}").expect("custom");
    assert_eq!(pick(true), custom);
    std::fs::remove_file(&legacy).expect("rm");
    assert_eq!(pick(true), custom);

    // A link to one that holds a byte is followed; a directory is not one.
    let elsewhere = tmp.path().join("kept.json");
    std::fs::write(&elsewhere, "{}").expect("kept");
    std::os::unix::fs::symlink(&elsewhere, &legacy).expect("link");
    assert_eq!(pick(false), legacy, "the link followed");
    std::fs::remove_file(&legacy).expect("rm");
    std::fs::create_dir(&legacy).expect("dir");
    assert_eq!(pick(false), plain, "CONTROL: a directory");
    std::fs::remove_dir(&legacy).expect("rm");

    // CONTROL: records unknown, no `.config.json` looked for.
    std::fs::write(&legacy, "{}").expect("legacy");
    assert_eq!(effective(&plain, None, false), plain);
    assert_eq!(effective(&plain, None, true), custom);
}

/// A custom OAuth URL renames the profile when it is set and not empty, as
/// Claude Code's JavaScript tests it. CONTROL: unset, and set empty.
#[test]
fn a_custom_oauth_url_counts_when_it_is_not_empty() {
    use std::ffi::OsStr;
    assert!(custom_oauth(Some(OsStr::new("https://auth.example.com"))));
    assert!(!custom_oauth(Some(OsStr::new(""))), "CONTROL: empty");
    assert!(!custom_oauth(None), "CONTROL: unset");
}

/// `read`: the file's profile, or the one word for why there is none.
#[test]
fn read_answers_the_profile_or_why_not() {
    let tmp = tmp_dir("read");
    let path = tmp.path().join(".claude.json");
    assert_eq!(read(&path), Err(NoProfile::Missing));
    std::fs::write(&path, aterm_json::to_string(&composed()).expect("compose")).expect("write");
    assert_eq!(read(&path), Ok(parsed(&composed())));
    std::fs::write(&path, b"{\"oauthAccount\": ").expect("write");
    assert_eq!(read(&path), Err(NoProfile::Unreadable));
    assert_eq!(read(tmp.path()), Err(NoProfile::NotAFile));
    let large = tmp.path().join("large.json");
    let file = std::fs::File::create(&large).expect("create");
    file.set_len(PROFILE_MAX + 1).expect("a sparse file");
    assert_eq!(read(&large), Err(NoProfile::TooLarge));
    assert_eq!(
        [
            NoProfile::Missing.word(),
            NoProfile::NotAFile.word(),
            NoProfile::TooLarge.word(),
        ],
        ["no-profile", "not-a-file", "too-large"]
    );
}

/// A FIFO planted where the profile should be is not a file, answered at
/// once: the open does not wait for a writer. The wait is a one-minute hang
/// detector; past it the test opens the FIFO's other end itself, so a
/// reader stuck in its open is let go before the test fails.
#[cfg(unix)]
#[test]
fn a_fifo_is_not_a_profile_and_does_not_block() {
    use std::os::unix::fs::OpenOptionsExt as _;
    let tmp = tmp_dir("fifo");
    let path = tmp.path().join(".claude.json");
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).expect("path");
    // SAFETY: `c_path` is a NUL-terminated path that outlives the call.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0, "mkfifo");
    let (tx, rx) = std::sync::mpsc::channel();
    let fifo = path.clone();
    let reader = std::thread::spawn(move || {
        let _ = tx.send(read(&fifo));
    });
    let answer = rx.recv_timeout(std::time::Duration::from_secs(60));
    if answer.is_err() {
        let _writer = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path);
        let _ = reader.join();
        panic!("read waited on a FIFO for a writer");
    }
    assert_eq!(answer.ok(), Some(Err(NoProfile::NotAFile)));
    reader.join().expect("reader");
}
