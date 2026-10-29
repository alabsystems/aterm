// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! User identity and rate-limit tests.
#![cfg(unix)]

use agh::Call;
use agh::config::Dirs;
use agh::error::{IdentityRef, Kind};
use agh::identity::{self, Profile, Source};
use agh::policy::{RouteRequest, RouteTarget, Tier};
use agh::rate::{self, RateBook, RateSnapshot};
use agh_fake::{FakeGithub, RepoSpec};
use aterm_json::Value;

use crate::common;

/// A fake holding `fixture-dev/app` with canonical tokens registered.
fn fake_with_dev(root: &std::path::Path) -> FakeGithub {
    let fake = FakeGithub::start();
    fake.add_repo("fixture-dev/app", RepoSpec::public());
    common::canonical_tokens(root, &fake, true, false);
    fake
}

#[test]
fn me_answers_the_profiles_login_and_identity_needs_as() {
    let root = common::scratch("users-me");
    let fake = fake_with_dev(root.path());
    let gh = common::github(
        common::router(&common::canonical_config(root.path()), root.path()),
        &fake,
    );
    let me = gh.users().me("dev").expect("GET /user as dev");
    assert_eq!(me.login, "fixture-dev-bot");
    let last = fake.seen().pop().expect("a request");
    assert_eq!(last.path_and_query, "/user");
    assert_eq!(last.as_login.as_deref(), Some("fixture-dev-bot"));

    let error = gh
        .call(&Call::get(
            "/user",
            RouteRequest::read(RouteTarget::Identity),
        ))
        .expect_err("an identity path with no --as");
    assert_eq!(error.kind, Kind::Usage);
    assert_eq!(
        error.sub.slug().as_deref(),
        Some("policy.identity_needs_as")
    );

    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    let dirs = ctx.dirs.clone().expect("dirs");
    common::write_config(&dirs, &common::canonical_config(root.path()));
    assert_eq!(common::run_cli(&mut ctx, &["api", "/user"]), 2);
    let error = common::error_json(&captured.stderr_text());
    assert_eq!(
        error["sub"],
        Value::from("policy.identity_needs_as"),
        "{error}"
    );
}

#[test]
fn get_reads_an_owner() {
    let root = common::scratch("users-get");
    let fake = fake_with_dev(root.path());
    let gh = common::github(
        common::router(&common::canonical_config(root.path()), root.path()),
        &fake,
    );
    let user = gh
        .users()
        .get("fixture-dev")
        .expect("GET /users/fixture-dev");
    assert_eq!(user.login, "fixture-dev");
    assert_eq!(user.kind, "Organization");
    let last = fake.seen().pop().expect("a request");
    assert_eq!(last.path_and_query, "/users/fixture-dev");
    assert!(matches!(
        gh.meter().as_,
        Some(IdentityRef::Profile { ref name, .. }) if name == "dev"
    ));
}

#[test]
fn get_refuses_an_empty_or_dot_segment_login_before_any_request() {
    let root = common::scratch("users-get-dots");
    let fake = fake_with_dev(root.path());
    let gh = common::github(
        common::router(&common::canonical_config(root.path()), root.path()),
        &fake,
    );
    for login in ["", ".", ".."] {
        let error = gh.users().get(login).expect_err(login);
        assert_eq!(error.kind, Kind::Usage, "{login:?}");
    }
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
}

#[test]
fn rate_is_sorted_by_resource_profiled_and_anonymous() {
    let root = common::scratch("users-rate");
    let fake = fake_with_dev(root.path());
    let gh = common::github(
        common::router(&common::canonical_config(root.path()), root.path()),
        &fake,
    );
    for who in [Some("dev"), None] {
        let snapshots = gh.users().rate(who).expect("GET /rate_limit");
        let resources: Vec<&str> = snapshots.iter().map(|s| s.resource.as_str()).collect();
        assert_eq!(resources, ["core", "graphql", "search"], "{who:?}");
        let core = &snapshots[0];
        let limit = if who.is_some() { 5000 } else { 60 };
        assert_eq!(core.limit, limit, "{who:?}");
        assert_eq!(core.remaining, limit, "{who:?}");
        assert_eq!(core.used + core.remaining, core.limit, "{who:?}");
    }
    let seen = fake.seen();
    let logins: Vec<Option<&str>> = seen.iter().map(|s| s.as_login.as_deref()).collect();
    assert_eq!(logins, [Some("fixture-dev-bot"), None]);
    assert!(!seen[1].had_authorization);
}

/// `agh rate`'s rows, from stdout.
fn rows_of(stdout: &str) -> Vec<Value> {
    let value: Value = aterm_json::from_str(stdout.trim_end()).expect("JSON rows");
    value.as_array().expect("an array").clone()
}

#[test]
fn agh_rate_shows_each_identity_from_the_api_and_the_book() {
    let root = common::scratch("users-cli");
    let fake = fake_with_dev(root.path());
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    let dirs: Dirs = ctx.dirs.clone().expect("dirs");
    common::write_config(&dirs, &common::canonical_config(root.path()));
    let dev = Profile::new(
        "dev",
        Source::File(root.path().join("tokens").join("dev.token")),
        Tier::Dev,
    );
    let fingerprint = identity::load_for_tests(&dev, &common::env())
        .expect("the dev token")
        .fingerprint()
        .to_string();
    let book = RateBook::at(dirs.rate());
    let booked = |resource: &str, remaining: u64| RateSnapshot {
        resource: resource.to_string(),
        limit: 100,
        remaining,
        reset: 1_760_000_100,
        used: 100 - remaining,
    };
    let dev_ref = IdentityRef::Profile {
        name: "dev".into(),
        fingerprint,
    };
    book.record(&rate::book_key(&dev_ref), &booked("graphql", 7));
    book.record(
        &rate::book_key(&IdentityRef::Anonymous),
        &booked("search", 3),
    );

    assert_eq!(
        common::run_cli(&mut ctx, &["rate"]),
        0,
        "{}",
        captured.stderr_text()
    );
    let rows = rows_of(&captured.stdout_text());
    let key = |row: &Value| {
        (
            row["as"].as_str().unwrap_or_default().to_string(),
            row["source"].as_str().unwrap_or_default().to_string(),
            row["resource"].as_str().unwrap_or_default().to_string(),
        )
    };
    let keys: Vec<(String, String, String)> = rows.iter().map(key).collect();
    for (who, source, resource) in [
        ("dev", "api", "core"),
        ("dev", "api", "graphql"),
        ("dev", "api", "search"),
        ("dev", "book", "graphql"),
        ("anonymous", "api", "core"),
        ("anonymous", "book", "search"),
    ] {
        let wanted = (who.to_string(), source.to_string(), resource.to_string());
        assert!(keys.contains(&wanted), "{wanted:?} in {keys:?}");
    }
    let first_anonymous = keys.iter().position(|k| k.0 == "anonymous");
    let last_dev = keys.iter().rposition(|k| k.0 == "dev");
    assert!(last_dev < first_anonymous, "profiles first: {keys:?}");
    let dev_book = rows
        .iter()
        .find(|row| key(row) == ("dev".into(), "book".into(), "graphql".into()))
        .expect("the dev book row");
    assert_eq!(dev_book["remaining"], Value::from(7u64));
    assert_eq!(dev_book["limit"], Value::from(100u64));
    for row in &rows {
        let mut fields: Vec<&str> = row
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        fields.sort_unstable();
        assert_eq!(
            fields,
            [
                "as",
                "limit",
                "remaining",
                "reset",
                "resource",
                "source",
                "used"
            ]
        );
    }

    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(common::run_cli(&mut ctx, &["rate", "--anonymous"]), 0);
    let anonymous = rows_of(&captured.stdout_text());
    assert!(!anonymous.is_empty());
    assert!(
        anonymous
            .iter()
            .all(|row| row["as"] == Value::from("anonymous"))
    );

    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(common::run_cli(&mut ctx, &["rate", "--as", "dev"]), 0);
    let dev = rows_of(&captured.stdout_text());
    assert!(dev.iter().all(|row| row["as"] == Value::from("dev")));
    fake.assert_clean();
}

#[test]
fn agh_rate_as_an_unknown_profile_is_usage() {
    let root = common::scratch("users-unknown");
    let fake = fake_with_dev(root.path());
    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    let dirs = ctx.dirs.clone().expect("dirs");
    common::write_config(&dirs, &common::canonical_config(root.path()));
    assert_eq!(common::run_cli(&mut ctx, &["rate", "--as", "nobody"]), 2);
    let error = common::error_json(&captured.stderr_text());
    assert_eq!(error["kind"], Value::from("usage"), "{error}");
}
