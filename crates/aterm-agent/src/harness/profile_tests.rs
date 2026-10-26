// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the `.claude.json` allowlist reader ([`super::parse`], [`super::read`]), the
//! subscription display, and agy's and Muse's login, usage, model and effort reads.

use aterm_json::{Value, json};
use aterm_tempfile::TempDir as Tmp;

use super::*;

const UUID: &str = "0f6c2b8e-1d3a-4c5e-9b7f-2a4d6e8c0b1a";

const SENTINEL: &str = "S3NT1NEL";

const DAY: i64 = 1_792_022_400;

fn tmp_dir(tag: &str) -> Tmp {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-profile-{tag}-"))
        .tempdir()
        .expect("temp dir")
}

#[test]
fn subscription_display_uses_one_selected_accounts_email_plan_and_weekly_cache() {
    let value = with(
        composed(),
        &["oauthAccount", "emailAddress"],
        json!("owner@example.com"),
    );
    let now = 1_792_050_000;
    let text = aterm_json::to_string(&value).unwrap();
    let display = parse_subscription(text.as_bytes(), now).unwrap();
    assert_eq!(display.email.as_deref(), Some("owner@example.com"));
    assert_eq!(display.plan.as_deref(), Some("Max 5x"));
    assert_eq!(display.weekly.as_deref(), Some("weekly 29% left"));
    assert!(!format!("{display:?}").contains(SENTINEL));
    assert!(!format!("{:?}", parse(text.as_bytes()).unwrap()).contains("owner@example.com"));

    let switched = with(
        with(
            value.clone(),
            &["oauthAccount", "accountUuid"],
            json!("another-account"),
        ),
        &["oauthAccount", "emailAddress"],
        json!("second@example.com"),
    );
    let display =
        parse_subscription(aterm_json::to_string(&switched).unwrap().as_bytes(), now).unwrap();
    assert_eq!(display.email.as_deref(), Some("second@example.com"));
    assert!(
        display.weekly.is_none(),
        "the old account's cache was rejected"
    );
    let aged = parse_subscription(text.as_bytes(), now + 15 * 60 + 1).unwrap();
    assert_eq!(
        aged.weekly.as_deref(),
        Some("weekly 29% left \u{b7} 15m ago"),
        "past fifteen minutes the cache shows with its age"
    );
    for time in [now - 61, now + 60 * 60 + 1, DAY + 10 * 24 * 60 * 60] {
        let display = parse_subscription(text.as_bytes(), time).unwrap();
        assert!(
            display.weekly.is_none() && display.weekly_figure.is_none(),
            "future, stale or expired cache: {time}"
        );
    }
}

/// Every weekly figure's age rule: current for fifteen minutes, then shown with its age up to an
/// hour; nothing older, from the future or past its reset. Tenths as the `/usage` cache has them.
#[test]
fn a_weekly_figure_is_current_for_fifteen_minutes_then_aged_for_an_hour() {
    const AT: i64 = 1_792_050_000;
    let figure = WeeklyFigure {
        used_x10: 380,
        resets_at: AT + 86_400,
        at: AT,
    };
    let words = |elapsed: i64| figure.words(AT + elapsed);
    assert_eq!(words(0).as_deref(), Some("weekly 62% left"));
    assert_eq!(words(15 * 60).as_deref(), Some("weekly 62% left"));
    assert!(figure.current(AT + 15 * 60) && !figure.current(AT + 15 * 60 + 1));
    assert_eq!(
        words(16 * 60).as_deref(),
        Some("weekly 62% left \u{b7} 16m ago")
    );
    assert_eq!(
        words(60 * 60).as_deref(),
        Some("weekly 62% left \u{b7} 60m ago")
    );
    assert_eq!(words(61 * 60), None, "older than an hour");
    assert_eq!(
        words(-60).as_deref(),
        Some("weekly 62% left"),
        "stamped up to a minute ahead of this clock: just read"
    );
    assert!(figure.current(AT - 60));
    assert_eq!(words(-61), None, "a reading from further in the future");
    assert_eq!(figure.words(AT + 86_400), None, "its window reset");
    let tenths = WeeklyFigure {
        used_x10: 385,
        ..figure
    };
    assert_eq!(tenths.words(AT).as_deref(), Some("weekly 61.5% left"));
    let older = WeeklyFigure {
        at: AT - 1,
        ..tenths
    };
    assert_eq!(WeeklyFigure::newer(Some(older), Some(figure)), Some(figure));
    assert_eq!(WeeklyFigure::newer(Some(figure), Some(older)), Some(figure));
    assert_eq!(WeeklyFigure::newer(None, Some(older)), Some(older));
}

#[test]
fn subscription_display_follows_process_profile_selection_and_clears_on_missing() {
    let root = tmp_dir("footer-subscription");
    let records = root.path().join(".claude");
    std::fs::create_dir(&records).unwrap();
    let profile = root.path().join(".claude.json");
    let custom = root.path().join(".claude-custom-oauth.json");
    let first = with(
        composed(),
        &["oauthAccount", "emailAddress"],
        json!("first@example.com"),
    );
    let second = with(
        composed(),
        &["oauthAccount", "emailAddress"],
        json!("second@example.com"),
    );
    std::fs::write(&profile, aterm_json::to_string(&first).unwrap()).unwrap();
    std::fs::write(&custom, aterm_json::to_string(&second).unwrap()).unwrap();
    let mut env = vec![format!("HOME={}", root.path().display())];
    let roster = super::super::accounts::Roster::default();
    let read =
        |env: &[String]| subscription_for(env, root.path(), &records, Some(&roster), 1_792_050_000);
    assert_eq!(
        read(&env).unwrap().email.as_deref(),
        Some("first@example.com")
    );
    env.push("CLAUDE_CODE_CUSTOM_OAUTH_URL=https://example.invalid".into());
    assert_eq!(
        read(&env).unwrap().email.as_deref(),
        Some("second@example.com")
    );
    std::fs::remove_file(&custom).unwrap();
    assert!(read(&env).is_none());
    env.push("ANTHROPIC_API_KEY=synthetic-test-key".into());
    assert!(
        read(&env).is_none(),
        "a private credential route is not the stored account"
    );
    assert!(
        read(&[]).is_none(),
        "an unread environment names no profile"
    );
}

#[test]
fn subscription_display_checks_the_canonical_identity_of_a_login_alias() {
    use super::super::accounts::{Roster, Row};

    let root = tmp_dir("footer-alias-identity");
    let records = root.path().join(".claude");
    std::fs::create_dir(&records).unwrap();
    let env = vec![format!("HOME={}", root.path().display())];
    let roster = Roster {
        rows: vec![Row {
            label: "alias".into(),
            email: Some("alias@example.test".into()),
            identity_email: Some("canonical@example.test".into()),
            ..Row::default()
        }],
    };
    for email in ["canonical@example.test", "alias@example.test"] {
        let value = with(composed(), &["oauthAccount", "emailAddress"], json!(email));
        std::fs::write(
            root.path().join(".claude.json"),
            aterm_json::to_string(&value).unwrap(),
        )
        .unwrap();
        let displayed = subscription_for(&env, root.path(), &records, Some(&roster), 1_792_050_000);
        assert_eq!(displayed.is_some(), email == "canonical@example.test");
        if let Some(displayed) = displayed {
            assert_eq!(displayed.email.as_deref(), Some(email));
        }
    }
}

#[test]
fn subscription_shared_profile_is_bound_to_the_captured_secure_storage_login() {
    use super::super::accounts::{LOGIN_ENV, Roster, Row};

    let root = tmp_dir("footer-shared-logins");
    let records = root.path().join(".claude");
    std::fs::create_dir(&records).unwrap();
    let profile = root.path().join(".claude.json");
    let roster = Roster {
        rows: vec![
            Row {
                label: "first".into(),
                email: Some("first@example.com".into()),
                ..Row::default()
            },
            Row {
                label: "second".into(),
                dir: Some("/logins/second".into()),
                email: Some("second@example.com".into()),
                ..Row::default()
            },
        ],
    };
    let first_env = vec![format!("HOME={}", root.path().display())];
    let mut second_env = first_env.clone();
    second_env.push(format!("{LOGIN_ENV}=/logins/second"));
    let write = |email: &str| {
        let value = with(composed(), &["oauthAccount", "emailAddress"], json!(email));
        std::fs::write(&profile, aterm_json::to_string(&value).unwrap()).unwrap();
    };
    let read = |env: &[String], roster| {
        subscription_for(env, root.path(), &records, roster, 1_792_050_000)
    };

    write("first@example.com");
    let first = read(&first_env, Some(&roster)).unwrap();
    assert_eq!(first.email.as_deref(), Some("first@example.com"));
    assert_eq!(
        first.account,
        AccountKey::claude(UUID),
        "the week shown is the profile's own account's"
    );
    assert_eq!(first.plan.as_deref(), Some("Max 5x"));
    assert_eq!(first.weekly.as_deref(), Some("weekly 29% left"));
    assert!(read(&second_env, Some(&roster)).is_none());

    write("second@example.com");
    assert!(read(&first_env, Some(&roster)).is_none());
    assert_eq!(
        read(&second_env, Some(&roster)).unwrap().email.as_deref(),
        Some("second@example.com")
    );
    assert!(read(&second_env, None).is_none(), "unknown binding");
    assert!(
        read(&first_env, None).is_none(),
        "an unread roster cannot rebind a shared profile"
    );
    let mut missing_email = roster.clone();
    missing_email.rows[1].email = None;
    assert!(read(&second_env, Some(&missing_email)).is_none());
    let mut renewed = roster.clone();
    let previous = renewed.rows[1].dir.take();
    renewed.rows[1].was.push(previous);
    renewed.rows[1].dir = Some("/logins/renewed-second".into());
    assert!(read(&second_env, Some(&renewed)).is_some());
    let lone = Roster {
        rows: vec![roster.rows[1].clone()],
    };
    assert!(read(&second_env, Some(&lone)).is_some());
}

fn parse_subscription(bytes: &[u8], now: i64) -> Result<SubscriptionDisplay, NoProfile> {
    parse_selected(bytes).map(|selected| selected.subscription(now))
}

#[test]
fn plan_usage_selection() {
    use super::super::accounts::{Roster, Row};

    let root = tmp_dir("plan-usage");
    let records = root.path().join(".claude");
    std::fs::create_dir(&records).unwrap();
    let profile = root.path().join(".claude.json");
    let env = vec![format!("HOME={}", root.path().display())];
    let now = 1_792_050_000;
    let weekly_reset = instant("2026-10-18T20:50:00Z").unwrap();
    let row = |email: Option<&str>| Row {
        label: "owner".into(),
        email: email.map(str::to_owned),
        ..Row::default()
    };
    let owner = Roster {
        rows: vec![row(Some("owner@example.com"))],
    };
    let owned = with(
        composed(),
        &["oauthAccount", "emailAddress"],
        json!("owner@example.com"),
    );
    let reading = |fetched: i64, account: &str, body: Value| {
        with(
            owned.clone(),
            &["cachedUsageUtilization"],
            json!({"fetchedAtMs": fetched * 1000, "accountUuid": account, "utilization": body}),
        )
    };
    let write = |value: &Value| {
        std::fs::write(&profile, aterm_json::to_string(value).unwrap()).unwrap();
    };
    let read = |roster: Option<&Roster>, at: i64, model: Option<&str>| {
        plan_usage_for(&env, root.path(), &records, roster, at, model)
    };
    let binding = |at: i64, model: Option<&str>| {
        read(Some(&owner), at, model).map(|plan| (plan.window, plan.used_x10))
    };
    let window = |pct: f64, resets: &str| json!({"utilization": pct, "resets_at": resets});
    let scoped = |name: &str, pct: f64| {
        json!({"kind": "weekly_scoped", "percent": pct, "resets_at": "2026-10-18T20:50:00Z",
               "scope": {"model": {"display_name": name}}})
    };
    let body = |five_hour: Value| {
        json!({
            "five_hour": five_hour,
            "seven_day": window(71.0, "2026-10-18T20:50:00Z"),
            "seven_day_opus": window(80.0, "2026-10-18T20:50:00Z"),
            "seven_day_sonnet": {"utilization": 99, "resets_at": null},
            "limits": [scoped("Fable 5.1", 90.0), scoped("Haiku", 99.0)]
        })
    };
    let calm = body(window(62.5, "2026-10-15T12:00:00Z"));

    write(&reading(now, UUID, calm.clone()));
    assert_eq!(
        read(Some(&owner), now, None),
        Some(PlanUsage {
            account: AccountKey::claude(UUID).unwrap(),
            window: Bucket::SevenDay,
            used_x10: 710,
            resets_at: weekly_reset,
            read_at: now,
        })
    );
    for (model, expected) in [
        ("claude-opus-5-5", (Bucket::SevenDayOpus, 800)),
        ("Opus 5.5", (Bucket::SevenDayOpus, 800)),
        ("opus", (Bucket::SevenDayOpus, 800)),
        ("claude-fable-5-1[1m]", (Bucket::Fable, 900)),
        ("claude-sonnet-5", (Bucket::SevenDay, 710)),
        ("claude-haiku-4-5-20251001", (Bucket::SevenDay, 710)),
    ] {
        assert_eq!(binding(now, Some(model)), Some(expected), "{model}");
    }
    assert_eq!(binding(now + 15 * 60, None), Some((Bucket::SevenDay, 710)));
    let kept = read(Some(&owner), now, None).expect("a current reading");
    assert!(kept.current_at(now + 15 * 60));
    assert!(!kept.current_at(now + 15 * 60 + 1));
    assert!(
        !kept.current_at(now - 1),
        "a reading from after now is not current"
    );
    let ending = PlanUsage {
        resets_at: now + 60,
        ..kept
    };
    assert!(ending.current_at(now + 59));
    assert!(
        !ending.current_at(now + 60),
        "a window that resets at now is over"
    );
    assert_eq!(
        subscription_and_plan_for(&env, root.path(), &records, Some(&owner), now, Some("opus")),
        subscription_for(&env, root.path(), &records, Some(&owner), now).zip(Some(read(
            Some(&owner),
            now,
            Some("opus")
        )))
    );

    write(&reading(
        now,
        UUID,
        body(window(95.0, "2026-10-15T12:00:00Z")),
    ));
    assert_eq!(
        binding(now, Some("claude-fable-5-1")),
        Some((Bucket::FiveHour, 950))
    );
    write(&reading(
        now,
        UUID,
        body(window(95.0, "2026-10-15T07:40:00Z")),
    ));
    assert_eq!(
        binding(now, Some("claude-fable-5-1")),
        Some((Bucket::Fable, 900))
    );
    write(&reading(
        now,
        UUID,
        json!({"five_hour": window(95.0, "2026-10-15T07:40:00Z"),
               "seven_day": window(71.0, "2026-10-15T07:00:00Z")}),
    ));
    assert_eq!(
        binding(now, None),
        None,
        "every window reset at or before now"
    );

    write(&reading(
        now,
        UUID,
        json!({"five_hour": window(62.5, "2026-10-15T12:00:00Z"),
               "seven_day": window(71.0, "2026-10-18T20:50:00Z"),
               "seven_day_sonnet": window(85.0, "2026-10-18T20:50:00Z")}),
    ));
    for model in ["claude-sonnet-5", "Sonnet 5", "sonnet[1m]"] {
        assert_eq!(
            binding(now, Some(model)),
            Some((Bucket::SevenDaySonnet, 850)),
            "{model}"
        );
    }
    assert_eq!(binding(now, Some("opus")), Some((Bucket::SevenDay, 710)));

    write(&reading(
        now,
        UUID,
        json!({"five_hour": window(62.5, "2026-10-15T12:00:00Z"),
               "seven_day": window(71.0, "2026-10-18T20:50:00Z"),
               "limits": [scoped("Fable 5", 95.0), scoped("Fable 5.1", 75.0)]}),
    ));
    for (model, used) in [
        ("claude-fable-5-1[1m]", 750),
        ("Fable 5.1", 750),
        ("claude-fable-5", 950),
        ("fable", 950),
    ] {
        assert_eq!(
            binding(now, Some(model)),
            Some((Bucket::Fable, used)),
            "{model}"
        );
    }

    for share in [json!(null), json!("high")] {
        write(&reading(
            now,
            UUID,
            json!({"five_hour": window(30.0, "2026-10-15T12:00:00Z"),
                   "seven_day": window(71.0, "2026-10-18T20:50:00Z"),
                   "seven_day_opus": {"utilization": share, "resets_at": "2026-10-18T20:50:00Z"}}),
        ));
        assert_eq!(binding(now, Some("claude-opus-5-5")), None, "{share:?}");
        assert_eq!(
            binding(now, Some("claude-sonnet-5")),
            Some((Bucket::SevenDay, 710)),
            "{share:?}"
        );
    }

    write(&reading(now, UUID, calm.clone()));
    assert_eq!(
        binding(now + 15 * 60 + 1, None),
        None,
        "fetched over 15 min ago"
    );
    assert_eq!(binding(now - 1, None), None, "fetched in the future");
    write(&reading(now, "another-account", calm.clone()));
    assert_eq!(binding(now, None), None, "another account's cache");

    write(&reading(now, UUID, calm));
    let several = Roster {
        rows: vec![
            row(None),
            Row {
                label: "second".into(),
                dir: Some("/logins/second".into()),
                email: Some("second@example.com".into()),
                ..Row::default()
            },
        ],
    };
    let stranger = Roster {
        rows: vec![row(Some("someone-else@example.com"))],
    };
    for (roster, why) in [
        (Some(&several), "a shared default home with several logins"),
        (Some(&stranger), "an address the roster does not bind"),
        (None, "an unread roster"),
    ] {
        assert_eq!(read(roster, now, None), None, "{why}");
        assert_eq!(
            subscription_for(&env, root.path(), &records, roster, now),
            None,
            "{why}"
        );
        assert_eq!(
            subscription_and_plan_for(&env, root.path(), &records, roster, now, None),
            None,
            "{why}"
        );
    }
    assert!(read(Some(&Roster::default()), now, None).is_some());
}

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
            "tengu_calm_seal": ["Nimbus", format!("{SENTINEL}-seal")],
            "tengu_other_flag": {"token": format!("{SENTINEL}-flag")}
        },
        "fableOverageConsentV2": keyed(&[
            (&format!("{SENTINEL}-consent-org"), json!(false)),
            (&format!("acct:{UUID}"), json!(true)),
        ]),
        "usageCreditsOnlyModelConsent": keyed(&[
            ("Nimbus", keyed(&[(&format!("{SENTINEL}-model-org"), json!(true))])),
            ("Lyra", keyed(&[("acct:x", json!(false))])),
            (&format!("{SENTINEL}-family"), keyed(&[("acct:x", json!(true))])),
        ]),
    })
}

fn keyed(entries: &[(&str, Value)]) -> Value {
    let mut map = aterm_json::Map::new();
    for (key, value) in entries {
        map.insert((*key).to_string(), value.clone());
    }
    Value::Object(map)
}

fn parsed(v: &Value) -> Profile {
    parse(aterm_json::to_string(v).expect("compose").as_bytes()).expect("a profile")
}

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
    assert_eq!(p.consented, ["fable", "nimbus"]);
    assert_eq!(p.credits_only, Some(vec!["nimbus".to_string()]));
    let usage = p.usage.expect("the cached body names this account");
    assert_eq!(usage.fetched_at_ms, 1_792_050_000_000);
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
        "-seal",
        "-consent-org",
        "-model-org",
        "-family",
    ] {
        assert!(
            whole.contains(&format!("{SENTINEL}{planted}"))
                || whole.contains(&format!("{planted}{SENTINEL}")),
            "the control reader holds {planted}"
        );
    }
    assert!(whole.contains(UUID), "the control reader holds the id");
}

#[test]
fn a_key_reads_back_from_its_text_and_nothing_else_does() {
    let claude = AccountKey::claude(UUID).expect("a key");
    let codex = AccountKey::codex("acct-1", "user-1").expect("a key");
    let grok = AccountKey::grok("user-1").expect("a key");
    let kimi = AccountKey::kimi(crate::ai::connector::Region::Global, "user-1").expect("a key");
    let kimi_cn =
        AccountKey::kimi(crate::ai::connector::Region::MainlandCn, "user-1").expect("a key");
    for key in [claude, codex, grok, kimi, kimi_cn] {
        assert_eq!(AccountKey::parse(&key.to_string()), Some(key));
    }
    assert!(grok.to_string().starts_with("g-") && kimi.to_string().starts_with("k-"));
    assert_ne!(kimi, kimi_cn, "one id on each region is two accounts");
    assert_eq!(AccountKey::grok(""), None);
    let text = claude.to_string();
    for bad in [
        text.replacen("c-", "y-", 1),
        text[..text.len() - 1].to_owned(),
        format!("{text}0"),
        format!("c-{}", text[2..].to_uppercase()),
        format!("c-g{}", &text[3..]),
        String::new(),
    ] {
        assert_ne!(bad, text, "each control changes the key");
        assert_eq!(AccountKey::parse(&bad), None, "{bad}");
    }
}

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

#[test]
fn a_cached_body_of_another_account_is_dropped() {
    let other = with(
        composed(),
        &["cachedUsageUtilization", "accountUuid"],
        json!("7a1e0c3b-5d2f-4e6a-8c9b-1f3e5a7c9d0b"),
    );
    assert_eq!(parsed(&other).usage, None);
    assert_eq!(parsed(&other).usage_of_another_at, Some(1_792_050_000));
    let unnamed = without(composed(), &["cachedUsageUtilization", "accountUuid"]);
    assert_eq!(parsed(&unnamed).usage, None);
    assert_eq!(parsed(&unnamed).usage_of_another_at, None);
    let undated = without(composed(), &["cachedUsageUtilization", "fetchedAtMs"]);
    assert_eq!(parsed(&undated).usage, None);
    assert!(
        parsed(&composed()).usage.is_some(),
        "control: the same body, its own account"
    );
    assert_eq!(parsed(&composed()).usage_of_another_at, None);
}

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

#[test]
fn the_profile_path_follows_the_home_rule() {
    use crate::harness::host_path::abs;
    let (ana, repo, srv, other) = (
        abs("/home/ana"),
        abs("/work/repo"),
        abs("/srv/claude-b"),
        abs("/home/other"),
    );
    let own = format!("{ana}/.claude");
    let user = Path::new(&ana);
    let work = || Some(PathBuf::from(&repo));
    assert_eq!(
        path_of(user, None, None, work),
        Some(user.join(".claude.json"))
    );
    assert_eq!(
        path_of(user, Some(&srv), None, work),
        Some(Path::new(&srv).join(".claude.json"))
    );
    assert_eq!(
        path_of(user, Some(&own), None, work),
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
        Some(Path::new(&repo).join("rel/claude/.claude.json"))
    );
    assert_eq!(
        path_of(user, Some(""), None, work),
        Some(user.join(".claude.json"))
    );
    assert_eq!(
        path_of(user, None, Some(&other), work),
        Some(Path::new(&other).join(".claude.json"))
    );
    assert_eq!(path_of(user, Some("rel/claude"), None, || None), None);
    assert_eq!(
        path_in(user, None, &user.join(".claude")),
        user.join(".claude.json")
    );
    assert_eq!(
        path_in(user, Some(&own), &user.join(".claude")),
        user.join(".claude/.claude.json"),
        "named explicitly, ~/.claude keeps its own profile inside it"
    );
    assert_eq!(
        path_in(user, Some(&srv), Path::new(&srv)),
        Path::new(&srv).join(".claude.json")
    );
    assert_eq!(
        path_in(user, Some(""), Path::new(&repo)),
        user.join(".claude.json"),
        "an empty home's records are where it runs, its profile in $HOME"
    );
}

/// The file Claude Code reads for a home ([`effective`], 2.1.301's `v_()`): a `.config.json` in
/// the records directory that holds a byte stands in for the plain profile, present or not, a link
/// to one too; an empty one does only while it has a write bit and no plain file is there; and a
/// custom OAuth URL renames the plain file, whose absence is then the renamed one's.
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

    assert_eq!(pick(false), plain);
    std::fs::write(&plain, "{}").expect("plain");
    assert_eq!(pick(false), plain);

    std::fs::write(&legacy, "{}").expect("legacy");
    assert_eq!(pick(false), legacy);
    mode(&legacy, 0o444);
    assert_eq!(pick(false), legacy, "read-only, and not empty");
    std::fs::remove_file(&plain).expect("rm");
    assert_eq!(pick(false), legacy);

    std::fs::remove_file(&legacy).expect("rm");
    std::fs::write(&legacy, "").expect("legacy");
    mode(&legacy, 0o444);
    assert_eq!(pick(false), plain, "CONTROL: empty and read-only");
    mode(&legacy, 0o644);
    assert_eq!(pick(false), legacy, "empty, writable, no plain file");
    std::fs::write(&plain, "{}").expect("plain");
    assert_eq!(pick(false), plain, "CONTROL: empty, beside a plain file");

    assert_eq!(pick(true), legacy, "no .claude-custom-oauth.json");
    std::fs::write(&custom, "{}").expect("custom");
    assert_eq!(pick(true), custom);
    std::fs::remove_file(&legacy).expect("rm");
    assert_eq!(pick(true), custom);

    let elsewhere = tmp.path().join("kept.json");
    std::fs::write(&elsewhere, "{}").expect("kept");
    std::os::unix::fs::symlink(&elsewhere, &legacy).expect("link");
    assert_eq!(pick(false), legacy, "the link followed");
    std::fs::remove_file(&legacy).expect("rm");
    std::fs::create_dir(&legacy).expect("dir");
    assert_eq!(pick(false), plain, "CONTROL: a directory");
    std::fs::remove_dir(&legacy).expect("rm");

    std::fs::write(&legacy, "{}").expect("legacy");
    assert_eq!(effective(&plain, None, false), plain);
    assert_eq!(effective(&plain, None, true), custom);

    assert!(effective_for_billing(&plain, Some(&records), Selection::Standard).is_err());
    std::fs::remove_file(&legacy).expect("remove billing legacy");
    assert_eq!(
        effective_for_billing(&plain, Some(&records), Selection::Standard).expect("standard"),
        plain
    );
    assert_eq!(
        effective_for_billing(&plain, Some(&records), Selection::CustomOauth).expect("custom"),
        custom
    );
    assert!(effective_for_billing(&plain, Some(&records), Selection::Unknown).is_err());
    assert!(effective_for_billing(&plain, None, Selection::Standard).is_err());
    std::os::unix::fs::symlink(".config.json", &legacy).expect("owned selector cycle");
    assert!(effective_for_billing(&plain, Some(&records), Selection::Standard).is_err());
    std::fs::remove_file(&legacy).expect("remove selector cycle");
    std::fs::write(&legacy, "{}").expect("restore legacy control");

    let subscription = r#"{"oauthAccount":{"organizationType":"claude_max"}}"#;
    let console = r#"{"oauthAccount":{"organizationType":"api_console"}}"#;
    let env = vec![format!("HOME={}", home.display())];
    let subscribed = Some(aterm_phase::LoginMethod::Subscription);
    std::fs::write(&plain, subscription).expect("plain subscription");
    std::fs::remove_file(&legacy).expect("remove legacy");
    assert_eq!(login_method(&env, &home), subscribed, "plain control");
    std::fs::write(&legacy, console).expect("effective Console profile");
    assert_eq!(
        login_method(&env, &home),
        None,
        "a stale plain subscription is not the effective login"
    );
    std::fs::write(&plain, console).expect("plain Console profile");
    std::fs::write(&legacy, subscription).expect("effective subscription");
    assert_eq!(
        login_method(&env, &home),
        subscribed,
        "the effective subscription is used"
    );
    std::fs::write(&legacy, "not JSON").expect("unreadable effective profile");
    assert_eq!(login_method(&env, &home), None);
    std::fs::write(&plain, subscription).expect("plain subscription again");
    std::fs::write(&legacy, "").expect("empty legacy beside plain");
    assert_eq!(login_method(&env, &home), subscribed);
    let mut unknown = env.clone();
    unknown.push("CLAUDE_CONFIG_DIR=".to_string());
    assert_eq!(
        login_method(&unknown, &home),
        None,
        "without the records directory its legacy profile cannot be ruled out"
    );
    unknown[1] = "CLAUDE_CONFIG_DIR=relative".to_string();
    assert_eq!(login_method(&unknown, &home), None);
    let mut credential = env.clone();
    credential.push("ANTHROPIC_API_KEY=invented-test-key".to_string());
    assert_eq!(login_method(&credential, &home), None);
    assert_eq!(login_method(&[], &home), None);
    assert_eq!(login_method(&env, &home), subscribed, "credential control");
}

#[test]
fn a_custom_oauth_url_counts_when_it_is_not_empty() {
    use std::ffi::OsStr;
    assert!(custom_oauth(Some(OsStr::new("https://auth.example.com"))));
    assert!(!custom_oauth(Some(OsStr::new(""))), "CONTROL: empty");
    assert!(!custom_oauth(None), "CONTROL: unset");
}

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
/// once: the open does not wait for a writer.
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

/// The account a home is signed in to is read afresh each call, its key
/// alone: a login rewritten under it reads as the new account, and a home
/// of its own (`CLAUDE_CONFIG_DIR`) reads its own profile, not the plain one.
#[test]
fn the_account_a_home_is_signed_in_to_is_read_afresh_each_call() {
    let tmp = tmp_dir("account-of");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let env = vec![format!("HOME={}", home.display())];
    let plain = home.join(".claude.json");
    assert_eq!(account_of(&env, &home), None);
    std::fs::write(
        &plain,
        r#"{"oauthAccount":{"accountUuid":"u-first","emailAddress":"a@example.com"}}"#,
    )
    .expect("profile");
    assert_eq!(account_of(&env, &home), AccountKey::claude("u-first"));
    std::fs::write(
        &plain,
        r#"{"numStartups":3,"oauthAccount":{"emailAddress":"b@example.com","accountUuid":"u-second"}}"#,
    )
    .expect("profile");
    assert_eq!(account_of(&env, &home), AccountKey::claude("u-second"));
    for text in [
        r#"{"oauthAccount":{"emailAddress":"b@example.com"}}"#,
        r#"{"oauthAccount":{"accountUuid":7}}"#,
        r#"{"oauthAccount":"u-second"}"#,
        "not JSON",
    ] {
        std::fs::write(&plain, text).expect("profile");
        assert_eq!(account_of(&env, &home), None, "{text}");
    }
    let own = tmp.path().join("own");
    std::fs::create_dir_all(&own).expect("own home");
    std::fs::write(&plain, r#"{"oauthAccount":{"accountUuid":"u-plain"}}"#).expect("plain");
    std::fs::write(
        own.join(".claude.json"),
        r#"{"oauthAccount":{"accountUuid":"u-own"}}"#,
    )
    .expect("own");
    let mut in_own = env.clone();
    in_own.push(format!("CLAUDE_CONFIG_DIR={}", own.display()));
    assert_eq!(account_of(&in_own, &home), AccountKey::claude("u-own"));
    assert_eq!(account_of(&env, &home), AccountKey::claude("u-plain"));
}

#[test]
fn a_login_answers_to_its_rows_address_alone() {
    let tmp = tmp_dir("answers");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let env = vec![format!("HOME={}", home.display())];
    let plain = home.join(".claude.json");
    let row = "owner@example.com";
    std::fs::write(
        &plain,
        r#"{"primaryApiKey":"sk-invented","oauthAccount":{"emailAddress":"Owner@Example.COM","organizationType":"claude_max"}}"#,
    )
    .expect("profile");
    assert_eq!(answers_to(&env, &home, row), Some(true));
    assert_eq!(answers_to(&env, &home, "other@example.com"), Some(false));
    for (text, want) in [
        (r#"{"oauthAccount":{"emailAddress":7}}"#, None),
        (
            r#"{"oauthAccount":{"emailAddress":{"a":"owner@example.com"}}}"#,
            None,
        ),
        (r#"{"oauthAccount":{"accountUuid":"u-1"}}"#, None),
        ("not JSON", None),
    ] {
        std::fs::write(&plain, text).expect("profile");
        assert_eq!(answers_to(&env, &home, row), want, "{text}");
    }
    std::fs::remove_file(&plain).expect("gone");
    assert_eq!(answers_to(&env, &home, row), None);
    let own = tmp.path().join("own");
    std::fs::create_dir_all(&own).expect("own home");
    std::fs::write(
        &plain,
        r#"{"oauthAccount":{"emailAddress":"owner@example.com"}}"#,
    )
    .expect("plain");
    let mut in_own = env.clone();
    in_own.push(format!("CLAUDE_CONFIG_DIR={}", own.display()));
    assert_eq!(answers_to(&in_own, &home, row), None);
    std::fs::write(
        own.join(".claude.json"),
        r#"{"oauthAccount":{"emailAddress":"owner@example.com"}}"#,
    )
    .expect("own profile");
    assert_eq!(answers_to(&in_own, &home, row), Some(true));
}

#[test]
fn full_provider_identity_survives_email_changes_and_never_falls_back_to_another_home() {
    let tmp = tmp_dir("full-provider-identity");
    let first = tmp.path().join("first");
    let other = tmp.path().join("other");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let profile = first.join(".claude.json");
    std::fs::write(
        &profile,
        r#"{"oauthAccount":{"accountUuid":"stable-one","emailAddress":"first@example.test"}}"#,
    )
    .unwrap();
    let observed = principal_in_home(&first).unwrap();
    std::fs::write(
        &profile,
        r#"{"oauthAccount":{"accountUuid":"stable-one","emailAddress":"changed@example.test"}}"#,
    )
    .unwrap();
    assert_eq!(principal_in_home(&first), Some(observed));
    assert_eq!(principal_in_home(&other), None);
    std::fs::write(&profile, r#"{"oauthAccount":{"accountUuid":"stable-two"}}"#).unwrap();
    assert_ne!(principal_in_home(&first), Some(observed));
    std::fs::write(&profile, r#"{"oauthAccount":{"accountUuid":42}}"#).unwrap();
    assert_eq!(principal_in_home(&first), None);
}

#[test]
fn a_muse_login_store_names_its_account_alone() {
    let tmp = tmp_dir("muse-auth");
    let home = tmp.path().join("home");
    let store = home.join(".config/muse");
    std::fs::create_dir_all(&store).expect("store");
    let auth = store.join("auth.json");
    let env = vec![format!("HOME={}", home.display())];
    std::fs::write(
        &auth,
        r#"{"schema_version":2,"providers":{"meta":{"mechanism":"oauth","storage":"keychain","user_email":"owner@example.com","api_key":"[REDACTED]"}}}"#,
    )
    .expect("auth");
    let args = |words: &[&str]| {
        words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>()
    };
    let shown = muse_subscription_for(&args(&["muse"]), &env, &home).expect("the account");
    assert_eq!(shown.email.as_deref(), Some("owner@example.com"));
    assert_eq!(
        (shown.plan, shown.weekly, shown.account),
        (None, None, None)
    );
    for words in [
        vec!["muse", "--provider", "echo"],
        vec!["muse", "--provider=local"],
        vec!["muse", "-c", "provider=\"echo\""],
        vec!["muse", "-cprovider=echo"],
        vec!["muse", "--config=provider=local"],
    ] {
        assert_eq!(
            muse_subscription_for(&args(&words), &env, &home),
            None,
            "{words:?}"
        );
    }
    assert!(
        muse_subscription_for(&args(&["muse", "--", "--provider", "echo"]), &env, &home).is_some(),
        "a prompt's words are not its flags"
    );
    let settings = store.join("settings.json");
    std::fs::write(&settings, r#"{"provider":"echo"}"#).expect("settings");
    assert_eq!(
        muse_subscription_for(&args(&["muse"]), &env, &home),
        None,
        "its settings' provider"
    );
    assert!(
        muse_subscription_for(&args(&["muse", "--provider", "meta"]), &env, &home).is_some(),
        "its command line outranks its settings"
    );
    assert!(
        muse_subscription_for(&args(&["muse", "-cprovider=meta"]), &env, &home).is_some(),
        "an attached config value outranks its settings too"
    );
    std::fs::remove_file(&settings).expect("settings gone");
    let mut keyed = env.clone();
    keyed.push("META_API_KEY=[REDACTED]".to_owned());
    assert_eq!(
        muse_subscription_for(&args(&["muse"]), &keyed, &home),
        None,
        "an API key"
    );
    for text in [
        r#"{"providers":{"meta":{"mechanism":"api_key","user_email":"owner@example.com"}}}"#,
        r#"{"providers":{"meta":{"mechanism":"oauth","storage":"keychain"}}}"#,
        r#"{"providers":{"other":{"user_email":"owner@example.com"}}}"#,
        "not JSON",
    ] {
        std::fs::write(&auth, text).expect("auth");
        assert_eq!(
            muse_subscription_for(&args(&["muse"]), &env, &home),
            None,
            "{text}"
        );
    }
    std::fs::remove_file(&auth).expect("gone");
    assert_eq!(muse_subscription_for(&args(&["muse"]), &env, &home), None);
    let xdg = tmp.path().join("xdg");
    std::fs::create_dir_all(xdg.join("muse")).expect("xdg store");
    std::fs::write(
        xdg.join("muse/auth.json"),
        r#"{"providers":{"meta":{"mechanism":"oauth","user_email":"xdg@example.com"}}}"#,
    )
    .expect("xdg auth");
    let mut in_xdg = env.clone();
    in_xdg.push(format!("XDG_CONFIG_HOME={}", xdg.display()));
    assert_eq!(
        muse_subscription_for(&args(&["muse"]), &in_xdg, &home).and_then(|s| s.email),
        Some("xdg@example.com".to_owned())
    );
}

/// agy's usage fetch is bounded whole: an agy that prints its doc and exits while a child of
/// its own still holds stdout open ends the read at `LAUNCH_READ_BUDGET` with no doc and that
/// child gone, never a read that waits on the child.
#[cfg(unix)]
#[test]
fn an_agy_usage_fetch_is_bounded_whole() {
    use crate::harness::stand_in::proven;
    let mut tmp = aterm_uds::deadline::KeptUntilRan::new(tmp_dir("agy-usage-whole").keep());
    let agy = tmp.path().join("agy");
    let parked = aterm_spec::wait::PARKED_SECS;
    proven(
        &agy,
        &format!(
            "/bin/sleep {parked} &\nprintf '%s\\n' \"$!\" > \"$0.child\"\nprintf '%s\\n' '{{\"status\":\"SUCCESS\"}}'\nexit 0"
        ),
    );
    tmp.ran();
    let (exe, home) = (agy.clone(), tmp.path().to_path_buf());
    let (tx, fetched) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(fetch_agy_usage_doc(&exe, &[], None, &home));
    });
    let got = fetched.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    // A stand-in the budget cut before it wrote its receipt left no child to look for.
    let child = std::fs::read_to_string(tmp.path().join("agy.child"))
        .ok()
        .and_then(|text| text.strip_suffix('\n')?.parse::<u32>().ok());
    assert!(
        got.is_ok(),
        "the fetch still waits on agy's child {child:?} holding its stdout"
    );
    assert_eq!(got.ok(), Some(None), "a read cut at its budget has no doc");
    let gone = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        std::time::Duration::from_millis(20),
        || {
            child
                .is_none_or(|pid| !crate::harness::upgrade_drive::alive(pid))
                .then_some(())
        },
    );
    assert!(gone.is_some(), "agy's child {child:?} outlived the read");
}

#[test]
fn agy_subscription_reads_auth_log_and_usage_json() {
    let log_bytes = br"I1022 09:30:56.342240 375 main.go:1084] Starting CLI v1.4.5
I1022 09:30:56.364166 375 server_oauth.go:224] applyAuthResult: email=interviewstreetuser@gmail.com, authMethod=consumer, quotaProject=
";
    assert_eq!(
        parse_agy_auth_log(log_bytes).as_deref(),
        Some("interviewstreetuser@gmail.com")
    );
    let usage_json = br#"{"status":"SUCCESS","command":{"name":"usage","data":{"groups":[{"name":"Gemini Models","buckets":[{"id":"gemini-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":0.782786,"reset_time":"2026-10-29T16:31:22Z"}]},{"name":"Claude and GPT models","buckets":[{"id":"3p-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":1.0,"reset_time":"2026-10-31T00:51:20Z"}]}]}}}"#;
    let now = 1_792_796_000;
    let gemini_fig = parse_agy_usage_json(usage_json, false, now).expect("gemini weekly");
    assert_eq!(gemini_fig.words(now).as_deref(), Some("weekly 78% left"));
    let third_party_fig = parse_agy_usage_json(usage_json, true, now).expect("3p weekly");
    assert_eq!(
        third_party_fig.words(now).as_deref(),
        Some("weekly 100% left")
    );

    // The stand-ins the fetches below run live here: it is removed only once each showed it ran.
    let mut tmp = aterm_uds::deadline::KeptUntilRan::new(tmp_dir("agy-auth").keep());
    #[cfg(not(unix))]
    tmp.ran();
    let home = tmp.path().join("home");
    let cfg = home.join(".gemini/antigravity-cli");
    std::fs::create_dir_all(&cfg).expect("cfg");
    std::fs::write(cfg.join("cli.log"), log_bytes).expect("cli.log");
    let argv = vec!["agy".to_owned()];
    let env = vec![format!("HOME={}", home.display())];
    let sub = agy_subscription_for(
        &argv,
        &env,
        &home,
        &SubscriptionDisplay::default(),
        Some("Gemini 3.1 Pro"),
        now,
    )
    .expect("agy subscription");
    assert_eq!(sub.email.as_deref(), Some("interviewstreetuser@gmail.com"));

    #[cfg(unix)]
    {
        let usage_file = tmp.path().join("usage.json");
        std::fs::write(&usage_file, usage_json).expect("write usage.json");
        let fake_agy = tmp.path().join("fake-agy.sh");
        crate::harness::stand_in::proven(
            &fake_agy,
            &format!(
                "for arg in \"$@\"; do\n  case \"$arg\" in\n    --config-dir*|-config-dir*) exit 2 ;;\n  esac\ndone\ncat '{}'\n",
                usage_file.display()
            ),
        );
        let exe_argv = vec![fake_agy.to_string_lossy().into_owned()];
        let store = super::super::agy_quota_store::Store::new(tmp.path(), "home");
        let fetched_sub = agy_subscription_with_store(
            &exe_argv,
            &env,
            &home,
            &SubscriptionDisplay::default(),
            Some("Gemini 3.1 Pro"),
            now + AGY_USAGE_RETRY_S + 5,
            Some(&store),
        )
        .expect("fetched agy subscription");
        assert_eq!(fetched_sub.weekly.as_deref(), Some("weekly 78% left"));
        assert!(store.last().is_some(), "fetch persisted reading to store");
        // The same fetch also cached the 3p-weekly bucket for Claude/GPT models.
        let cached_3p = agy_subscription_with_store(
            &argv,
            &env,
            &home,
            &fetched_sub,
            Some("Claude Opus 4.6"),
            now + AGY_USAGE_RETRY_S + 5,
            Some(&store),
        )
        .expect("cached 3p subscription");
        assert_eq!(cached_3p.weekly.as_deref(), Some("weekly 100% left"));
        // With the in-memory cache cleared and another reader holding the store lock past
        // `FRESH_OPEN_S`, a stale store reading whose weekly window is still open is the fallback.
        AGY_USAGE_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let held_lock = store.try_lock().expect("take store lock");
        let locked_fallback = agy_subscription_with_store(
            &argv,
            &env,
            &home,
            &SubscriptionDisplay::default(),
            Some("Gemini 3.1 Pro"),
            now + 1_200,
            Some(&store),
        )
        .expect("locked fallback subscription");
        assert_eq!(
            locked_fallback.weekly.as_deref(),
            Some("weekly 78% left · 18m ago")
        );
        drop(held_lock);
        // Mid-session /login switch to a new email bypasses the previous account's
        // stored reading and fetches fresh quota via ~/.local/bin/agy.
        let local_bin = home.join(".local/bin");
        std::fs::create_dir_all(&local_bin).expect("mkdir .local/bin");
        let switched_usage = tmp.path().join("usage-switched.json");
        std::fs::write(
            &switched_usage,
            br#"{"status":"SUCCESS","command":{"name":"usage","data":{"groups":[{"name":"Gemini Models","buckets":[{"id":"gemini-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":0.42,"reset_time":"2026-10-29T16:31:22Z"}]}]}}}"#,
        )
        .expect("write switched usage");
        let local_agy = local_bin.join("agy");
        crate::harness::stand_in::proven(
            &local_agy,
            &format!("cat '{}'\n", switched_usage.display()),
        );
        tmp.ran();
        std::fs::write(
            cfg.join("cli.log"),
            br"I1022 09:35:00.000000 375 server_oauth.go:224] applyAuthResult: email=second@example.com, authMethod=consumer, quotaProject=
",
        )
        .expect("write switched cli.log");
        let switched_sub = agy_subscription_with_store(
            &argv,
            &env,
            &home,
            &fetched_sub,
            Some("Gemini 3.1 Pro"),
            now + AGY_USAGE_RETRY_S + 10,
            Some(&store),
        )
        .expect("switched agy subscription");
        assert_eq!(switched_sub.email.as_deref(), Some("second@example.com"));
        assert_eq!(switched_sub.weekly.as_deref(), Some("weekly 42% left"));
        std::fs::remove_file(&local_agy).expect("remove local agy");
        std::fs::write(cfg.join("cli.log"), log_bytes).expect("restore cli.log");
        AGY_USAGE_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }

    let mut keyed = env.clone();
    keyed.push("GEMINI_API_KEY=secret".to_owned());
    assert_eq!(
        agy_subscription_for(
            &argv,
            &keyed,
            &home,
            &SubscriptionDisplay::default(),
            None,
            now,
        ),
        None
    );

    // An expired WeeklyFigure on `prev` must not leave stale `prev.weekly` text when no live
    // figure replaces it, whereas an on-screen `/usage` reading (`weekly_figure: None`) stays
    // until its reset time passes.
    let expired_prev = SubscriptionDisplay {
        email: Some("interviewstreetuser@gmail.com".to_owned()),
        weekly: Some("weekly 78% left".to_owned()),
        weekly_figure: Some(gemini_fig),
        ..SubscriptionDisplay::default()
    };
    let after_expiry = agy_subscription_for(
        &argv,
        &env,
        &home,
        &expired_prev,
        Some("Gemini 3.1 Pro"),
        now + USAGE_SHOWN_S + 10,
    )
    .expect("subscription after figure expiry");
    assert_eq!(after_expiry.weekly, None);
    assert_eq!(after_expiry.weekly_figure, None);

    let screen_prev = SubscriptionDisplay {
        email: Some("interviewstreetuser@gmail.com".to_owned()),
        weekly: Some("weekly 78% left".to_owned()),
        weekly_figure: None,
        ..SubscriptionDisplay::default()
    };
    let kept_screen = agy_subscription_for(
        &argv,
        &env,
        &home,
        &screen_prev,
        Some("Gemini 3.1 Pro"),
        now + USAGE_SHOWN_S + 10,
    )
    .expect("subscription with screen weekly");
    assert_eq!(kept_screen.weekly.as_deref(), Some("weekly 78% left"));

    // Switching models between Gemini (third_party=false) and Claude (third_party=true)
    // on the same email does not carry Gemini's cached weekly_figure across the bucket boundary.
    {
        let mut guard = AGY_USAGE_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let map = guard.get_or_insert_with(std::collections::HashMap::new);
        map.insert(
            (
                cfg.clone(),
                "interviewstreetuser@gmail.com".to_owned(),
                false,
            ),
            (now, expired_prev.weekly_figure),
        );
        map.insert(
            (
                cfg.clone(),
                "interviewstreetuser@gmail.com".to_owned(),
                true,
            ),
            (now, None),
        );
    }
    let switched_bucket = agy_subscription_for(
        &argv,
        &env,
        &home,
        &expired_prev,
        Some("Claude Opus 4.6"),
        now,
    )
    .expect("switched model bucket");
    assert_eq!(
        switched_bucket.email.as_deref(),
        Some("interviewstreetuser@gmail.com")
    );
    assert_eq!(switched_bucket.weekly_figure, None);
    assert_eq!(switched_bucket.weekly, None);

    // A mid-session `/login` in `cli.log`'s last 128 KiB overrides the startup email in its head.
    let mut big_log = log_bytes.to_vec();
    big_log.extend(std::iter::repeat_n(b'x', 200 * 1024));
    big_log.extend_from_slice(
        b"\nI1023 18:00:00.000000 375 server_oauth.go:224] applyAuthResult: email=switched@example.com, authMethod=consumer\n",
    );
    std::fs::write(cfg.join("cli.log"), &big_log).expect("big cli.log");
    let switched = agy_subscription_for(
        &argv,
        &env,
        &home,
        &expired_prev,
        Some("Gemini 3.1 Pro"),
        now,
    )
    .expect("switched agy subscription");
    assert_eq!(switched.email.as_deref(), Some("switched@example.com"));
    assert_eq!(switched.weekly, None);
    assert_eq!(switched.weekly_figure, None);
}

#[test]
fn agy_and_muse_model_effort_read_argv_settings_and_log() {
    assert_eq!(
        parse_agy_model_label("Gemini 4 Argon (High)"),
        Some(("Gemini 4 Argon".to_owned(), Some("high".to_owned())))
    );
    assert_eq!(
        parse_agy_model_label("Claude Sonnet 4.6"),
        Some(("Claude Sonnet 4.6".to_owned(), None))
    );

    let log = b"I1023 17:50:20.231653 404 model_config_manager.go:340] Propagating selected model override to backend: label=\"Gemini 4 Argon (High)\"\n";
    assert_eq!(
        parse_agy_model_log(log),
        Some(("Gemini 4 Argon".to_owned(), Some("high".to_owned())))
    );

    let dir = tmp_dir("agy-model-effort");
    let home = dir.path().join("home");
    let cfg = home.join(".gemini/antigravity-cli");
    std::fs::create_dir_all(&cfg).expect("create agy config");
    std::fs::write(
        cfg.join("settings.json"),
        br#"{"model":"Gemini 4 Argon (High)"}"#,
    )
    .expect("write settings.json");
    let argv = vec!["agy".to_owned()];
    let env = vec![format!("HOME={}", home.display())];
    assert_eq!(
        agy_model_effort_for(&argv, &env, &home),
        (
            Some("Gemini 4 Argon".to_owned()),
            Some("high".to_owned()),
            true,
            true
        )
    );

    // Pinned on argv closes model_open and effort_open (including Go single-dash flags).
    let pinned = vec![
        "agy".to_owned(),
        "-model=Claude Opus 4.6".to_owned(),
        "-effort=max".to_owned(),
    ];
    assert_eq!(
        agy_model_effort_for(&pinned, &env, &home),
        (
            Some("Claude Opus 4.6".to_owned()),
            Some("max".to_owned()),
            false,
            false
        )
    );

    // Fallback to cli.log when settings.json has no model key.
    std::fs::write(cfg.join("settings.json"), br"{}").expect("clear settings.json");
    std::fs::write(cfg.join("cli.log"), log).expect("write cli.log");
    assert_eq!(
        agy_model_effort_for(&argv, &env, &home),
        (
            Some("Gemini 4 Argon".to_owned()),
            Some("high".to_owned()),
            true,
            true
        )
    );
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt as _;
        let custom_log = cfg.join("custom.log");
        std::fs::write(&custom_log, log).expect("write custom.log");
        std::fs::remove_file(cfg.join("settings.json")).expect("rm settings.json");
        std::fs::remove_file(cfg.join("cli.log")).expect("rm cli.log");
        for fifo in [cfg.join("settings.json"), cfg.join("cli.log")] {
            let c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        }
        assert_eq!(
            agy_model_effort_for(
                &[
                    "agy".to_owned(),
                    format!("-log-file={}", custom_log.display()),
                ],
                &env,
                &home,
            ),
            (
                Some("Gemini 4 Argon".to_owned()),
                Some("high".to_owned()),
                true,
                true
            )
        );
        std::fs::remove_file(cfg.join("settings.json")).expect("rm fifo settings.json");
        std::fs::remove_file(cfg.join("cli.log")).expect("rm fifo cli.log");
        std::fs::write(cfg.join("settings.json"), br"{}").expect("restore settings.json");
        std::fs::write(cfg.join("cli.log"), log).expect("restore cli.log");
    }

    let muse_cfg = home.join(".config/muse");
    std::fs::create_dir_all(&muse_cfg).expect("create muse config");
    std::fs::write(
        muse_cfg.join("settings.json"),
        br#"{"model":"Muse Spark (Medium)"}"#,
    )
    .expect("write muse settings.json");
    assert_eq!(
        muse_model_effort_for(&["muse".to_owned()], &env, &home),
        (
            Some("Muse Spark".to_owned()),
            Some("medium".to_owned()),
            true,
            true
        )
    );
    assert_eq!(
        muse_model_effort_for(
            &[
                "muse".to_owned(),
                "-c".to_owned(),
                "model=\"Muse Spark\"".to_owned(),
                "--config=reasoning_effort=high".to_owned(),
            ],
            &env,
            &home
        ),
        (
            Some("Muse Spark".to_owned()),
            Some("high".to_owned()),
            false,
            false
        )
    );

    let cid = "dfaaf1c5-98ff-4412-9226-76412af952a5";
    let logs_dir = cfg.join("brain").join(cid).join(".system_generated/logs");
    let presence_dir = cfg.join("presence");
    std::fs::create_dir_all(&logs_dir).expect("create brain logs");
    std::fs::create_dir_all(&presence_dir).expect("create presence");
    std::fs::write(presence_dir.join(format!("{cid}.lock")), b"").expect("write lock");
    let workspace = home.join("aterm");
    std::fs::write(
        cfg.join("history.jsonl"),
        format!(
            r#"{{"display":"inspect the window","timestamp":1,"workspace":"{}","conversationId":"{cid}"}}"#,
            workspace.display()
        ),
    )
    .expect("write history.jsonl");
    let transcript_bytes = br#"{"step_index":0,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"All footer fields are populated.","tool_calls":[{"name":"edit_file","args":{}}]}
"#;
    std::fs::write(logs_dir.join("transcript.jsonl"), transcript_bytes)
        .expect("write transcript.jsonl");
    let (excerpts, len) = agy_excerpts_for(
        &argv,
        &env,
        &home,
        Some(&[
            std::fs::canonicalize(crate::harness::upgrade_agy::presence_lock(&cfg, cid)).unwrap(),
        ]),
    )
    .expect("agy excerpts");
    assert_eq!(len, transcript_bytes.len() as u64);
    assert_eq!(excerpts.prompts.len(), 1);
    assert_eq!(excerpts.prompts[0].text, "inspect the window");
    assert_eq!(
        excerpts.said.as_deref(),
        Some("All footer fields are populated.")
    );
    assert_eq!(excerpts.doing.as_deref(), Some("edit_file"));
    assert_eq!(
        agy_resume_for(
            &argv,
            &env,
            &home,
            Some(&[
                std::fs::canonicalize(crate::harness::upgrade_agy::presence_lock(&cfg, cid))
                    .unwrap()
            ])
        )
        .as_deref(),
        Some(format!("agy --conversation={cid}").as_str())
    );

    // When two conversations share the same workspace and `cid_newer` is later in `history.jsonl`,
    // the observed presence lock resolves to `cid`; without held evidence neither history owns it.
    let cid_newer = "b14557eb-9109-4eb3-ae3d-212449d689cb";
    let newer_logs = cfg
        .join("brain")
        .join(cid_newer)
        .join(".system_generated/logs");
    std::fs::create_dir_all(&newer_logs).expect("create newer brain logs");
    std::fs::write(presence_dir.join(format!("{cid_newer}.lock")), b"").expect("write newer lock");
    std::fs::write(
        newer_logs.join("transcript.jsonl"),
        br#"{"step_index":0,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"Other tab response."}
"#,
    )
    .expect("write newer transcript");
    std::fs::write(
        cfg.join("history.jsonl"),
        format!(
            "{{\"display\":\"inspect the window\",\"timestamp\":1,\"workspace\":\"{}\",\"conversationId\":\"{cid}\"}}\n{{\"display\":\"other tab prompt\",\"timestamp\":2,\"workspace\":\"{}\",\"conversationId\":\"{cid_newer}\"}}\n",
            workspace.display(),
            workspace.display()
        ),
    )
    .expect("write two-tab history.jsonl");
    assert!(agy_resume_for(&argv, &env, &home, None).is_none());
    let lock = std::fs::canonicalize(presence_dir.join(format!("{cid}.lock"))).unwrap();
    let newer_lock = std::fs::canonicalize(presence_dir.join(format!("{cid_newer}.lock"))).unwrap();
    let resumed_argv = vec!["agy".to_owned(), format!("--conversation={cid_newer}")];
    assert_eq!(
        agy_resume_for(
            &resumed_argv,
            &env,
            &home,
            Some(std::slice::from_ref(&lock))
        )
        .as_deref(),
        Some(format!("agy --conversation={cid}").as_str())
    );
    let (held_excerpts, _) = agy_excerpts_for(
        &resumed_argv,
        &env,
        &home,
        Some(std::slice::from_ref(&lock)),
    )
    .expect("held agy excerpts");
    assert_eq!(
        held_excerpts.said.as_deref(),
        Some("All footer fields are populated.")
    );
    let db_dir = cfg.join(crate::harness::upgrade_agy::CONVERSATIONS);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = db_dir.join(format!("{cid}.db"));
    let newer_db = db_dir.join(format!("{cid_newer}.db"));
    std::fs::write(&db, b"").unwrap();
    std::fs::write(&newer_db, b"").unwrap();
    let db = std::fs::canonicalize(db).unwrap();
    let newer_db = std::fs::canonicalize(newer_db).unwrap();
    let old_launch = vec!["agy".to_owned(), format!("--conversation={cid}")];
    assert_eq!(
        agy_resume_for(
            &old_launch,
            &env,
            &home,
            Some(std::slice::from_ref(&newer_db))
        )
        .as_deref(),
        Some(format!("agy --conversation={cid_newer}").as_str())
    );
    assert_eq!(
        agy_excerpts_for(&argv, &env, &home, Some(std::slice::from_ref(&newer_db)))
            .unwrap()
            .0
            .said
            .as_deref(),
        Some("Other tab response.")
    );
    assert!(agy_resume_for(&old_launch, &env, &home, Some(&[db, newer_db.clone()])).is_none());
    assert!(
        agy_resume_for(
            &old_launch,
            &env,
            &home,
            Some(&[lock.clone(), newer_lock, newer_db.clone()])
        )
        .is_none()
    );
    assert_eq!(
        agy_resume_for(&argv, &env, &home, Some(&[lock, newer_db])).as_deref(),
        Some(format!("agy --conversation={cid}").as_str())
    );

    let muse_sid = "01a16480-f908-7d10-a78e-5d95372dfb98";
    let muse_old_sid = "01a16470-0000-7d10-a78e-5d95372dfb98";
    let muse_dir = home
        .join(".local/share/muse/sessions/2026/10/23")
        .join(muse_sid);
    let muse_old_dir = home
        .join(".local/share/muse/sessions/2026/10/23")
        .join(muse_old_sid);
    std::fs::create_dir_all(&muse_dir).expect("create muse session dir");
    std::fs::create_dir_all(&muse_old_dir).expect("create old muse session dir");
    std::fs::write(muse_dir.join(".session.lock"), b"pid=4242\nhost=mac\n")
        .expect("write muse lock");
    std::fs::write(muse_old_dir.join("session.jsonl"), b"{}\n").expect("write old muse log");
    let muse_log = br#"{"payload_type":"runtime.session","payload":{"kind":"run","run_id":"r1","event":{"kind":"started","prompt":"verify muse resume"}}}
{"payload_type":"runtime.session","payload":{"kind":"run","run_id":"r1","event":{"kind":"assistant_message_committed","text":"Muse resume verified."}}}
{"payload_type":"runtime.session","payload":{"kind":"task","run_id":"r1","task_id":"t1","event":{"kind":"proposed","task_kind":"tool.read_file"}}}
"#;
    std::fs::write(muse_dir.join("session.jsonl"), muse_log).expect("write muse session.jsonl");
    let muse_resumed_argv = vec![
        "muse".to_owned(),
        "resume".to_owned(),
        muse_old_sid.to_owned(),
    ];
    assert_eq!(
        muse_resume_for(
            4242,
            &muse_resumed_argv,
            &env,
            &home,
            Some(&[std::fs::canonicalize(muse_dir.join(".session.lock")).unwrap()])
        )
        .as_deref(),
        Some(format!("muse resume {muse_sid}").as_str())
    );
    assert_eq!(
        muse_resume_for(0, &muse_resumed_argv, &env, &home, None).as_deref(),
        Some(format!("muse resume {muse_old_sid}").as_str()),
        "without an observed lock the explicit launch UUID remains available"
    );
    let (muse_exc, muse_len) = muse_excerpts_for(
        4242,
        &muse_resumed_argv,
        &env,
        &home,
        Some(&[std::fs::canonicalize(muse_dir.join(".session.lock")).unwrap()]),
    )
    .expect("muse excerpts");
    assert_eq!(muse_len, muse_log.len() as u64);
    assert_eq!(muse_exc.prompts.len(), 1);
    assert_eq!(muse_exc.prompts[0].text, "verify muse resume");
    assert_eq!(muse_exc.said.as_deref(), Some("Muse resume verified."));
    assert_eq!(muse_exc.doing.as_deref(), Some("read_file"));

    // A trailing line over 64 KiB widens `read_jsonl_tail`'s window rather than emptying the tail.
    let mut large_log = muse_log.to_vec();
    let padding = "x".repeat(70 * 1024);
    large_log.extend_from_slice(
        format!(
            r#"{{"payload_type":"runtime.session","payload":{{"kind":"task","run_id":"r1","task_id":"t2","event":{{"kind":"proposed","task_kind":"tool.bash","pad":"{padding}"}}}}}}
"#
        )
        .as_bytes(),
    );
    std::fs::write(muse_dir.join("session.jsonl"), &large_log).expect("write large muse log");
    let (large_exc, _) = muse_excerpts_for(
        4242,
        &muse_resumed_argv,
        &env,
        &home,
        Some(&[std::fs::canonicalize(muse_dir.join(".session.lock")).unwrap()]),
    )
    .expect("large muse excerpts");
    assert_eq!(large_exc.doing.as_deref(), Some("bash"));
}

#[test]
fn agy_excerpts_belong_to_the_processes_held_conversation() {
    let dir = tmp_dir("agy-excerpt-owner");
    let cfg = dir.path().join("config");
    let presence = cfg.join("presence");
    std::fs::create_dir_all(&presence).unwrap();
    let cfg = std::fs::canonicalize(cfg).unwrap();
    let presence = cfg.join("presence");
    let a = "11111111-1111-4111-8111-111111111111";
    let b = "22222222-2222-4222-8222-222222222222";
    for (cid, words) in [(a, "own A"), (b, "other B")] {
        let logs = cfg.join("brain").join(cid).join(".system_generated/logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(
            logs.join("transcript.jsonl"),
            format!(
                "{}\n{}\n",
                json!({"type":"USER_INPUT", "content":format!("<USER_REQUEST>\n{words} prompt\n</USER_REQUEST>")}),
                json!({"type":"PLANNER_RESPONSE", "content":format!("{words} reply")}),
            ),
        )
        .unwrap();
        std::fs::write(presence.join(format!("{cid}.lock")), b"").unwrap();
    }
    let history = cfg.join("history.jsonl");
    let write_history = |workspace: &str| {
        std::fs::write(
            &history,
            format!(
                "{}\n{}\n",
                json!({"conversationId":a, "workspace":workspace, "display":"own A history"}),
                json!({"conversationId":b, "workspace":workspace, "display":"other B history"}),
            ),
        )
        .unwrap();
    };
    write_history("/same-workspace");
    let argv = vec!["agy".to_owned()];
    let explicit_a = vec!["agy".to_owned(), format!("--conversation={a}")];
    let env = vec![format!("AGY_CLI_CONFIG_DIR={}", cfg.display())];
    let lock_a = presence.join(format!("{a}.lock"));
    let lock_b = presence.join(format!("{b}.lock"));
    let read = |argv: &[String], files: Option<&[PathBuf]>| {
        agy_excerpts_for(argv, &env, dir.path(), files).map(|(excerpts, _)| excerpts)
    };
    let resume =
        |argv: &[String], files: Option<&[PathBuf]>| agy_resume_for(argv, &env, dir.path(), files);
    let own = read(&argv, Some(std::slice::from_ref(&lock_a))).unwrap();
    assert_eq!(own.prompts[0].text, "own A prompt");
    assert_eq!(own.said.as_deref(), Some("own A reply"));
    assert!(own.prompts.iter().all(|p| !p.text.contains("other B")));
    let switched = read(&explicit_a, Some(std::slice::from_ref(&lock_b))).unwrap();
    assert_eq!(switched.said.as_deref(), Some("other B reply"));
    assert!(
        resume(&explicit_a, Some(std::slice::from_ref(&lock_b)))
            .unwrap()
            .ends_with(&format!("agy --conversation={b}"))
    );
    assert!(resume(&explicit_a, Some(&[lock_a.clone(), lock_b.clone()])).is_none());
    assert!(resume(&argv, None).is_none());
    assert!(read(&explicit_a, Some(&[lock_a.clone(), lock_b])).is_none());
    assert_eq!(
        read(&argv, Some(&[lock_a.clone(), lock_a.clone()]))
            .unwrap()
            .said,
        own.said,
        "two descriptors for one lock are one identity"
    );
    assert!(
        read(&argv, Some(&[])).is_none(),
        "unheld files are no identity"
    );
    assert!(
        read(&argv, None).is_none(),
        "an unread table is no identity"
    );
    write_history("/other-workspace");
    assert!(read(&argv, Some(&[])).is_none());
    assert_eq!(
        read(&argv, Some(std::slice::from_ref(&lock_a)))
            .unwrap()
            .said,
        own.said
    );
    #[cfg(unix)]
    {
        let alias = dir.path().join("config-alias");
        std::os::unix::fs::symlink(&cfg, &alias).unwrap();
        let alias_env = vec![format!("AGY_CLI_CONFIG_DIR={}", alias.display())];
        assert_eq!(
            agy_excerpts_for(
                &argv,
                &alias_env,
                dir.path(),
                Some(std::slice::from_ref(&lock_a))
            )
            .unwrap()
            .0
            .said,
            own.said
        );
    }
    assert_eq!(read(&explicit_a, None).unwrap().said, own.said);
    let transcript_a = cfg
        .join("brain")
        .join(a)
        .join(".system_generated/logs/transcript.jsonl");
    std::fs::remove_file(&transcript_a).unwrap();
    assert!(read(&explicit_a, None).is_none());
    assert!(
        resume(&explicit_a, None)
            .unwrap()
            .ends_with(&format!("agy --conversation={a}"))
    );
    assert!(
        resume(&argv, Some(std::slice::from_ref(&lock_a)))
            .unwrap()
            .ends_with(&format!("agy --conversation={a}"))
    );
    assert!(
        read(
            &["agy".to_owned(), "--conversation=../foreign".to_owned()],
            None
        )
        .is_none()
    );
}

#[test]
fn agy_jsonl_tail_reads_only_regular_files_with_the_same_window() {
    let dir = tmp_dir("agy-excerpt-tail");
    let path = dir.path().join("history.jsonl");
    let tail = b"{\"display\":\"kept\"}\n";
    std::fs::write(&path, tail).unwrap();
    assert_eq!(
        read_jsonl_tail(&path, 65_536),
        Some((tail.to_vec(), tail.len() as u64))
    );
    let mut large = vec![b'x'; 70_000];
    large.push(b'\n');
    large.extend_from_slice(tail);
    std::fs::write(&path, &large).unwrap();
    assert_eq!(
        read_jsonl_tail(&path, 65_536),
        Some((tail.to_vec(), large.len() as u64))
    );
    let wide = dir.path().join("wide.jsonl");
    let row = format!("{{\"display\":\"{}\"}}\n", "x".repeat(70_000));
    std::fs::write(&wide, &row).unwrap();
    assert_eq!(
        read_jsonl_tail(&wide, 64),
        Some((row.as_bytes().to_vec(), row.len() as u64)),
        "a small initial window grows to a complete row"
    );
    let fragment = dir.path().join("fragment.jsonl");
    let mut partial = vec![b' '; 4 * 1024 * 1024 + 1];
    partial[0] = b'x';
    let record = b"{\"display\":\"unproved fragment\"}";
    let at = partial.len() - record.len();
    partial[at..].copy_from_slice(record);
    std::fs::write(&fragment, partial).unwrap();
    assert!(read_jsonl_tail(&fragment, 64).is_none());
    assert!(read_jsonl_tail(&fragment, u64::MAX).is_none());
    assert!(read_jsonl_tail(dir.path(), 65_536).is_none());
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let alias = dir.path().join("alias.jsonl");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert_eq!(
            read_jsonl_tail(&alias, 65_536),
            Some((tail.to_vec(), large.len() as u64))
        );
        std::fs::remove_file(&path).unwrap();
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: one NUL-terminated private path; mkfifo retains no pointer.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(read_jsonl_tail(&path, 65_536).is_none());
        assert!(read_jsonl_tail(&alias, 65_536).is_none());

        let cid = "11111111-1111-4111-8111-111111111111";
        let logs = dir
            .path()
            .join("brain")
            .join(cid)
            .join(".system_generated/logs");
        std::fs::create_dir_all(&logs).unwrap();
        let transcript = logs.join("transcript.jsonl");
        std::fs::write(
            &transcript,
            b"{\"type\":\"PLANNER_RESPONSE\",\"content\":\"regular transcript\"}\n",
        )
        .unwrap();
        let argv = vec!["agy".to_owned(), format!("--conversation={cid}")];
        let env = vec![format!("AGY_CLI_CONFIG_DIR={}", dir.path().display())];
        let read = || agy_excerpts_for(&argv, &env, dir.path(), None);
        assert_eq!(
            read().unwrap().0.said.as_deref(),
            Some("regular transcript")
        );
        std::fs::remove_file(&transcript).unwrap();
        let name = std::ffi::CString::new(transcript.as_os_str().as_bytes()).unwrap();
        // SAFETY: one NUL-terminated private path; mkfifo retains no pointer.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(read().is_none());
    }
    let data = dir.path().join("muse-data");
    let day = data.join("muse/sessions/2026/10/24");
    let sid_a = "01a16480-f908-7d10-a78e-5d95372dfb98";
    let sid_b = "01a16480-f908-7d10-a78e-5d95372dfb99";
    let a = day.join(sid_a);
    let b = day.join(sid_b);
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let lock = a.join(".session.lock");
    let other_lock = b.join(".session.lock");
    std::fs::write(&lock, b"pid=4242\nhost=private\n").unwrap();
    std::fs::write(&other_lock, b"pid=4242\n").unwrap();
    for (session, words) in [(&a, "own Muse reply"), (&b, "other Muse reply")] {
        std::fs::write(session.join("session.jsonl"), format!("{}\n", json!({"payload_type":"runtime.session", "payload":{"kind":"run", "event":{"kind":"assistant_message_committed", "text":words}}}))).unwrap();
    }
    let lock = std::fs::canonicalize(lock).unwrap();
    let other_lock = std::fs::canonicalize(other_lock).unwrap();
    let files = std::cell::RefCell::new(vec![lock.clone()]);
    let argv = vec!["muse".to_owned()];
    let explicit_a = vec!["muse".to_owned(), "resume".to_owned(), sid_a.to_owned()];
    let env = vec![format!("XDG_DATA_HOME={}", data.display())];
    let resume = || muse_resume_for(4242, &argv, &env, dir.path(), Some(&files.borrow()));
    let excerpts = || muse_excerpts_for(4242, &argv, &env, dir.path(), Some(&files.borrow()));
    assert!(resume().unwrap().ends_with(&format!("muse resume {sid_a}")));
    assert_eq!(
        excerpts().unwrap().0.said.as_deref(),
        Some("own Muse reply")
    );
    files.borrow_mut().clear();
    assert!(
        resume().is_none() && excerpts().is_none(),
        "saved PID records are not observed ownership"
    );
    assert!(
        muse_resume_for(
            4242,
            &explicit_a,
            &env,
            dir.path(),
            Some(std::slice::from_ref(&other_lock))
        )
        .unwrap()
        .ends_with(&format!("muse resume {sid_b}"))
    );
    assert_eq!(
        muse_excerpts_for(
            4242,
            &explicit_a,
            &env,
            dir.path(),
            Some(std::slice::from_ref(&other_lock))
        )
        .unwrap()
        .0
        .said
        .as_deref(),
        Some("other Muse reply")
    );
    *files.borrow_mut() = vec![lock.clone(), other_lock];
    assert!(
        resume().is_none() && excerpts().is_none(),
        "two held records are ambiguous"
    );
    *files.borrow_mut() = vec![lock.clone(), lock.clone()];
    assert!(resume().is_some(), "duplicate descriptors name one lock");
    *files.borrow_mut() = vec![lock.clone()];
    std::fs::write(&lock, b"pid=5252\n").unwrap();
    assert!(resume().is_none(), "the held record must name this process");
    let mut body = b"pid=4242\n".to_vec();
    body.resize(65_536, b' ');
    std::fs::write(&lock, &body).unwrap();
    assert!(resume().is_some(), "a complete record at the bound is read");
    body.push(b' ');
    std::fs::write(&lock, &body).unwrap();
    assert!(
        resume().is_none() && excerpts().is_none(),
        "a valid prefix is no complete PID proof"
    );
    std::fs::write(&lock, b"pid=4242\n\xff").unwrap();
    assert!(resume().is_none(), "invalid UTF-8 is not a complete record");
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        std::fs::remove_file(&lock).unwrap();
        let name = std::ffi::CString::new(lock.as_os_str().as_bytes()).unwrap();
        // SAFETY: one NUL-terminated private path; mkfifo retains no pointer.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(resume().is_none() && excerpts().is_none());
    }
    std::fs::remove_file(a.join("session.jsonl")).unwrap();
    assert!(
        muse_resume_for(0, &explicit_a, &env, dir.path(), None)
            .unwrap()
            .ends_with(&format!("muse resume {sid_a}"))
    );
    assert!(muse_excerpts_for(0, &explicit_a, &env, dir.path(), None).is_none());
}

/// THE FOOTER NEVER ASKS THE PERSON FOR THE WEEK (owner ruling R-12, 2026-10-24: "why does it
/// say \"?\" and \"/usage\" ??? THE HARNESS MUST DO THIS STOP ASKING THE USER"): no reason a
/// Claude Code footer gives for a missing weekly figure, in its words or its spoken form, names
/// `/usage` or a step for the person to take; each still says the figure is unknown.
#[test]
fn no_weekly_gap_tells_the_person_to_act() {
    use super::super::tap::Refusal;
    for gap in [
        WeeklyGap::TapOff,
        WeeklyGap::Unoffered(Refusal::Exe),
        WeeklyGap::Unoffered(Refusal::Home),
        WeeklyGap::Unoffered(Refusal::Login),
        WeeklyGap::Unoffered(Refusal::Line),
        WeeklyGap::Unoffered(Refusal::Argv),
        WeeklyGap::Unoffered(Refusal::Settings),
        WeeklyGap::Unoffered(Refusal::Room),
        WeeklyGap::Untapped,
        WeeklyGap::Unrestarted,
        WeeklyGap::NoReply,
        WeeklyGap::Stale,
    ] {
        for said in [gap.words(), gap.spoken()] {
            let lower = said.to_lowercase();
            for asks in [
                "/usage",
                "restart tab",
                "restart of the tab",
                "open ",
                "run ",
                "press ",
            ] {
                assert!(!lower.contains(asks), "{gap:?}: {said:?} asks {asks:?}");
            }
        }
        assert!(gap.words().starts_with("weekly ? "), "{gap:?}");
        assert!(gap.spoken().starts_with("weekly figure unknown"), "{gap:?}");
    }
}
