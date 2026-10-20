// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stub's claude.ai account routes: the bodies a subscriber's Claude Code
//! reads its plan and its limits from, in the shapes the managed 2.1.301
//! bundle parses them, each answered from the `--account-file` (`account TAG
//! …` writes it; re-read at every request).
//!
//! * `GET /api/oauth/usage` — the usage body, whatever its query: Claude Code
//!   names three reads of it (`/api/oauth/usage` when `/usage` opens,
//!   `?at_wall=1&skip_spend=1` at a wall, `?cedar_ember=1&skip_spend=1` for a
//!   reset used away from one), and the log names which. The windows
//!   `five_hour`, `seven_day`, `seven_day_oauth_apps`, `seven_day_opus`,
//!   `seven_day_sonnet` and `cinder_cove` are each `{utilization, resets_at}`
//!   (a percent and an ISO instant), both keys present and either null: the
//!   bundle's schema `TS`, which a cached copy (`cachedUsageUtilization`) is
//!   read back through, refuses a window that lacks one, and with it the
//!   whole copy (the live read is not checked against it). `limits[]`
//!   (`{kind, group, percent, resets_at, severity, is_active, scope}`, `scope`
//!   `{model, surface}` each `{display_name}`; a `weekly_scoped` row whose
//!   `scope.model.display_name` is on the server's allowlist is the panel's
//!   `Current week (<name>)`), `extra_usage` (`{is_enabled, monthly_limit,
//!   used_credits, utilization, currency, disabled_reason}`, cents), the
//!   credit blocks `cinder_cove` (a bare window, drawn `Claude Code and Cowork
//!   credit` with its `resets_at` as the subtext) and `wattle_ember` (that
//!   window plus `label`, `limit_dollars`, `used_dollars`,
//!   `remaining_dollars` and `periods[]` of `{starts_at, ends_at}`, drawn
//!   under its `label`, else `Credit`), and the two limit-reset programs a
//!   wall reads: `juniper_tide` (the session reset: `eligible`, `arm`,
//!   `available`, `next_available_at`, `weekly_resets_at`, `resets_per_week`)
//!   and `cedar_ember` (the grants: `eligible`, `at_limit`, `exhausted[]`,
//!   `grants[]` of `{id, label, resets_total, resets_left, starts_at,
//!   ends_at, clears[], paused, usable_now, use_requires_limit,
//!   percent_used, blocking[]}`, `next_grant_id`, `weekly_resets_at`,
//!   `cooldown_until`).
//! * `GET /api/oauth/profile` (an OAuth login) and `GET
//!   /api/claude_cli_profile` (an API key) — the profile that fills
//!   `.claude.json`'s `oauthAccount`: `account{uuid, email, display_name,
//!   full_name, created_at}`, `organization{uuid, organization_type,
//!   rate_limit_tier, seat_tier, has_extra_usage_enabled, billing_type,
//!   subscription_created_at, plan_display_name, cc_onboarding_flags, …}`.
//! * `GET /api/claude_cli/bootstrap` — its `oauth_account` block carries the
//!   tiers `oauthAccount` keeps (`organization_type`,
//!   `organization_rate_limit_tier`, `user_rate_limit_tier`, `seat_tier`).
//!
//! The file is `{"usage": {…}, "profile": {…}, "bootstrap": {…}}`; a route
//! whose key is missing or null is a 404 in Anthropic's error shape. A string
//! `"@ISO+N@"` anywhere in a body is the UTC instant N seconds after the answer
//! (ISO 8601 with milliseconds, as the usage body spells `resets_at`), and
//! `"@NOW+N@"` that unix second as a number. [`composed`] is the lab's own
//! body set: every value composed, every name an example.com one.
//!
//! What Claude Code 2.1.301 does with these routes behind the lab's base URL
//! is measured in [`crate::stats`] ("Account stats facts"): it sends none of
//! them to the stub, because it builds each from its fixed claude.ai API base,
//! never from `ANTHROPIC_BASE_URL`.

use crate::pyjson::J;

/// An account route of the stub.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// `GET /api/oauth/usage`, any query.
    Usage,
    /// `GET /api/oauth/profile`.
    Profile,
    /// `GET /api/claude_cli_profile`: the profile an API key reads.
    CliProfile,
    /// `GET /api/claude_cli/bootstrap`.
    Bootstrap,
}

impl Route {
    /// The route of a request, or `None` for every other one.
    #[must_use]
    pub fn of(method: &str, path: &str) -> Option<Route> {
        if method != "GET" {
            return None;
        }
        match path {
            "/api/oauth/usage" => Some(Route::Usage),
            "/api/oauth/profile" => Some(Route::Profile),
            "/api/claude_cli_profile" => Some(Route::CliProfile),
            "/api/claude_cli/bootstrap" => Some(Route::Bootstrap),
            _ => None,
        }
    }

    /// The account file's key that answers it.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Route::Usage => "usage",
            Route::Profile | Route::CliProfile => "profile",
            Route::Bootstrap => "bootstrap",
        }
    }

    /// The log's word for it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Route::Usage => "usage",
            Route::Profile => "profile",
            Route::CliProfile => "cli-profile",
            Route::Bootstrap => "bootstrap",
        }
    }
}

/// Which of Claude Code's usage reads a query is, as its bundle names them:
/// `at_wall` (`at_wall=1`), `cedar_ember` (`cedar_ember=1`), else `plain`.
#[must_use]
pub fn usage_read(query: &[(String, String)]) -> &'static str {
    let on = |k: &str| query.iter().any(|(n, v)| n == k && v == "1");
    if on("at_wall") {
        "at_wall"
    } else if on("cedar_ember") {
        "cedar_ember"
    } else {
        "plain"
    }
}

/// `spec` with every string `"@ISO+N@"` the UTC instant `now + N` (ISO 8601,
/// milliseconds) and every string `"@NOW+N@"` the unix second `now + N`.
#[must_use]
pub fn expand(spec: &J, now: i64) -> J {
    let offset = |s: &str, head: &str| -> Option<i64> {
        let n = s.strip_prefix(head)?.strip_suffix('@')?;
        (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            .then(|| n.parse::<i64>().ok())
            .flatten()
    };
    match spec {
        J::Str(s) => {
            if let Some(n) = offset(s, "@ISO+") {
                J::s(crate::util::utc_iso(now + n))
            } else if let Some(n) = offset(s, "@NOW+") {
                J::Int(now + n)
            } else {
                spec.clone()
            }
        }
        J::Arr(a) => J::Arr(a.iter().map(|v| expand(v, now)).collect()),
        J::Obj(m) => J::Obj(m.iter().map(|(k, v)| (k.clone(), expand(v, now))).collect()),
        _ => spec.clone(),
    }
}

/// An instant in the composed bodies: `secs` after the answer.
fn at(secs: i64) -> J {
    J::s(format!("@ISO+{secs}@"))
}

const ACCOUNT_UUID: &str = "00000000-0000-4000-8000-0000000000a1";
const ORG_UUID: &str = "00000000-0000-4000-8000-0000000000b1";
const EMAIL: &str = "ana@example.com";

/// The lab's composed account: a Max 5x subscriber `ana@example.com` with
/// usage credits on. Its five-hour window at 62.5 % resets in 1 h 25 m, the
/// week at 71 % in 3 d 4 h; the Fable week (a `weekly_scoped` row) at 40 %;
/// $41 of $200 of credits used this month; a one-time credit of $50 with
/// $12.50 used; the session reset used (next available in 4 d); one grant of
/// two left, usable now, until 5 d from now. Nothing in it is anyone's.
#[must_use]
pub fn composed() -> J {
    const H5: i64 = 5_100;
    const WEEK: i64 = 274_800;
    let window =
        |pct: f64, reset: i64| J::obj([("utilization", J::Float(pct)), ("resets_at", at(reset))]);
    let limit = |kind: &str, group: &str, pct: i64, reset: i64, active: bool, scope: J| {
        let mut row = J::obj([
            ("kind", J::s(kind)),
            ("group", J::s(group)),
            ("percent", J::Int(pct)),
            ("resets_at", at(reset)),
            ("severity", J::s("normal")),
            ("is_active", J::Bool(active)),
        ]);
        if !matches!(scope, J::Null) {
            row.set("scope", scope);
        }
        row
    };
    let fable = J::obj([("model", J::obj([("display_name", J::s("Fable"))]))]);
    let usage = J::obj([
        ("five_hour", window(62.5, H5)),
        ("seven_day", window(71.0, WEEK)),
        ("seven_day_oauth_apps", J::Null),
        ("seven_day_opus", J::Null),
        ("seven_day_sonnet", J::Null),
        ("cinder_cove", J::Null),
        (
            "wattle_ember",
            J::obj([
                ("utilization", J::Float(25.0)),
                ("resets_at", at(1_209_600)),
                ("label", J::s("One-time credit")),
                ("limit_dollars", J::Float(50.0)),
                ("used_dollars", J::Float(12.5)),
                ("remaining_dollars", J::Float(37.5)),
                (
                    "periods",
                    J::Arr(vec![J::obj([
                        ("starts_at", at(0)),
                        ("ends_at", at(1_209_600)),
                    ])]),
                ),
            ]),
        ),
        (
            "extra_usage",
            J::obj([
                ("is_enabled", J::Bool(true)),
                ("monthly_limit", J::Int(20_000)),
                ("used_credits", J::Int(4_100)),
                ("utilization", J::Float(20.5)),
                ("currency", J::s("USD")),
                ("disabled_reason", J::Null),
            ]),
        ),
        (
            "limits",
            J::Arr(vec![
                limit("session", "session", 62, H5, true, J::Null),
                limit("weekly_all", "weekly", 71, WEEK, false, J::Null),
                limit("weekly_scoped", "weekly", 40, WEEK, false, fable),
            ]),
        ),
        (
            "juniper_tide",
            J::obj([
                ("eligible", J::Bool(true)),
                ("ineligible_reason", J::Null),
                ("in_experiment", J::Bool(true)),
                ("arm", J::s("reset")),
                ("available", J::Bool(false)),
                ("next_available_at", at(345_600)),
                ("weekly_resets_at", at(WEEK)),
                ("resets_per_week", J::Int(1)),
            ]),
        ),
        (
            "cedar_ember",
            J::obj([
                ("eligible", J::Bool(true)),
                ("ineligible_reason", J::Null),
                ("at_limit", J::Bool(true)),
                ("exhausted", J::Arr(vec![J::s("five_hour")])),
                (
                    "grants",
                    J::Arr(vec![J::obj([
                        ("id", J::s("lab-grant-1")),
                        ("label", J::s("Limit reset")),
                        ("resets_total", J::Int(2)),
                        ("resets_left", J::Int(1)),
                        ("starts_at", at(0)),
                        ("ends_at", at(432_000)),
                        ("clears", J::Arr(vec![J::s("five_hour"), J::s("seven_day")])),
                        ("paused", J::Bool(false)),
                        ("usable_now", J::Bool(true)),
                        ("use_requires_limit", J::Bool(true)),
                        ("percent_used", J::obj([])),
                        ("blocking", J::Arr(Vec::new())),
                    ])]),
                ),
                ("next_grant_id", J::s("lab-grant-1")),
                ("weekly_resets_at", at(WEEK)),
                ("cooldown_until", J::Null),
            ]),
        ),
    ]);
    let profile = J::obj([
        (
            "account",
            J::obj([
                ("uuid", J::s(ACCOUNT_UUID)),
                ("email", J::s(EMAIL)),
                ("display_name", J::s("Ana")),
                ("full_name", J::s("Ana Example")),
                ("created_at", J::s("2025-01-02T03:04:05.000Z")),
            ]),
        ),
        (
            "organization",
            J::obj([
                ("uuid", J::s(ORG_UUID)),
                ("name", J::s("Example Org")),
                ("organization_type", J::s("claude_max")),
                ("rate_limit_tier", J::s("default_claude_max_5x")),
                ("seat_tier", J::Null),
                ("has_extra_usage_enabled", J::Bool(true)),
                ("billing_type", J::s("stripe_subscription")),
                ("subscription_created_at", J::s("2025-02-03T04:05:06.000Z")),
                ("plan_display_name", J::s("Max 5x")),
                ("cc_onboarding_flags", J::obj([])),
                ("claude_code_trial_ends_at", J::Null),
                ("claude_code_trial_duration_days", J::Null),
            ]),
        ),
    ]);
    let bootstrap = J::obj([(
        "oauth_account",
        J::obj([
            ("account_uuid", J::s(ACCOUNT_UUID)),
            ("account_email", J::s(EMAIL)),
            ("organization_uuid", J::s(ORG_UUID)),
            ("organization_name", J::s("Example Org")),
            ("organization_type", J::s("claude_max")),
            (
                "organization_rate_limit_tier",
                J::s("default_claude_max_5x"),
            ),
            ("user_rate_limit_tier", J::s("default_claude_max_5x")),
            ("seat_tier", J::Null),
        ]),
    )]);
    J::obj([
        ("usage", usage),
        ("profile", profile),
        ("bootstrap", bootstrap),
    ])
}

/// The `oauthAccount` Claude Code 2.1.301 keeps in `.claude.json` once it
/// has read [`composed`]'s profile and bootstrap (the bundle's profile
/// merge, then the bootstrap's tiers over it): what `claude-json TAG --set
/// oauthAccount composed` seeds a scratch home with, since behind the lab's
/// base URL it never reads them itself. `profileFetchedAt` is `now_ms`.
#[must_use]
pub fn composed_oauth_account(now_ms: i64) -> J {
    J::obj([
        ("accountUuid", J::s(ACCOUNT_UUID)),
        ("emailAddress", J::s(EMAIL)),
        ("organizationUuid", J::s(ORG_UUID)),
        ("hasExtraUsageEnabled", J::Bool(true)),
        ("billingType", J::s("stripe_subscription")),
        ("accountCreatedAt", J::s("2025-01-02T03:04:05.000Z")),
        ("subscriptionCreatedAt", J::s("2025-02-03T04:05:06.000Z")),
        ("ccOnboardingFlags", J::obj([])),
        ("claudeCodeTrialEndsAt", J::Null),
        ("claudeCodeTrialDurationDays", J::Null),
        ("seatTier", J::Null),
        ("planDisplayName", J::s("Max 5x")),
        ("displayName", J::s("Ana")),
        ("fullName", J::s("Ana Example")),
        ("profileFetchedAt", J::Int(now_ms)),
        ("organizationType", J::s("claude_max")),
        ("organizationRateLimitTier", J::s("default_claude_max_5x")),
        ("userRateLimitTier", J::s("default_claude_max_5x")),
        ("organizationName", J::s("Example Org")),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_routes_are_the_paths_claude_code_names_and_get_only() {
        assert_eq!(Route::of("GET", "/api/oauth/usage"), Some(Route::Usage));
        assert_eq!(Route::of("POST", "/api/oauth/usage"), None);
        assert_eq!(
            Route::of("GET", "/api/claude_cli_profile").map(Route::key),
            Some("profile")
        );
        assert_eq!(Route::of("GET", "/api/oauth/usage/x"), None);
        let q = |s: &str| crate::stub::parse_query(s);
        assert_eq!(usage_read(&q("at_wall=1&skip_spend=1")), "at_wall");
        assert_eq!(usage_read(&q("cedar_ember=1&skip_spend=1")), "cedar_ember");
        assert_eq!(usage_read(&q("")), "plain");
        assert_eq!(usage_read(&q("at_wall=0")), "plain");
    }

    #[test]
    fn instants_expand_everywhere_and_nothing_else_moves() {
        let spec =
            J::parse(r#"{"a":"@ISO+60@","b":["@NOW+5@",{"c":"@ISO+x@"}],"d":"@ISO+60","e":7}"#)
                .unwrap();
        let out = expand(&spec, 1_792_000_000);
        assert_eq!(
            out.py(),
            r#"{"a": "2026-10-14T17:47:40.000Z", "b": [1792000005, {"c": "@ISO+x@"}], "d": "@ISO+60", "e": 7}"#
        );
    }

    #[test]
    fn the_composed_account_names_no_one_and_carries_every_block() {
        let c = composed();
        let text = c.py();
        for block in [
            "five_hour",
            "seven_day",
            "limits",
            "extra_usage",
            "wattle_ember",
            "cinder_cove",
            "juniper_tide",
            "cedar_ember",
            "oauth_account",
        ] {
            assert!(text.contains(&format!("\"{block}\"")), "{block}");
        }
        let emails: Vec<&str> = text
            .split('"')
            .filter(|w| w.contains('@') && !w.starts_with("@ISO+"))
            .collect();
        assert!(
            emails.iter().all(|e| e.ends_with("@example.com")),
            "{emails:?}"
        );
        let fable = c
            .get("usage")
            .and_then(|u| u.get("limits"))
            .and_then(J::as_arr)
            .and_then(|l| {
                l.iter()
                    .find(|r| r.get("kind").and_then(J::as_str) == Some("weekly_scoped"))
            })
            .and_then(|r| r.get("scope"))
            .and_then(|s| s.get("model"))
            .and_then(|m| m.get("display_name"))
            .and_then(J::as_str);
        assert_eq!(fable, Some("Fable"));
    }
}
