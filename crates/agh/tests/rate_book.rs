// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Rate snapshots and the advisory rate book (the plan's §4.6.14).

use agh::error::IdentityRef;
use agh::rate::{RateBook, RateSnapshot, book_key, resource_for};

use crate::common;

/// The simulated clock's start.
const NOW: u64 = 1_760_000_000;

fn snapshot(resource: &str, remaining: u64, reset: u64) -> RateSnapshot {
    RateSnapshot {
        resource: resource.into(),
        limit: 60,
        remaining,
        reset,
        used: 60 - remaining,
    }
}

#[test]
fn from_headers_needs_limit_remaining_and_reset() {
    let all = [
        ("X-RateLimit-Limit", "5000"),
        ("X-RateLimit-Remaining", "4987"),
        ("X-RateLimit-Reset", "1760003600"),
        ("X-RateLimit-Resource", "search"),
        // Not `limit - remaining` (13), so a read value and the default differ.
        ("X-RateLimit-Used", "20"),
    ];
    let full = RateSnapshot {
        resource: "search".into(),
        limit: 5000,
        remaining: 4987,
        reset: 1_760_003_600,
        used: 20,
    };
    assert_eq!(
        RateSnapshot::from_headers(&common::headers(&all)),
        Some(full.clone())
    );
    for missing in 0..all.len() {
        let some: Vec<(&str, &str)> = all
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != missing)
            .map(|(_, p)| *p)
            .collect();
        let expected = match missing {
            0..=2 => None,
            3 => Some(RateSnapshot {
                resource: "core".into(),
                ..full.clone()
            }),
            _ => Some(RateSnapshot {
                used: 13,
                ..full.clone()
            }),
        };
        assert_eq!(
            RateSnapshot::from_headers(&common::headers(&some)),
            expected,
            "{missing}"
        );
    }
    let defaults = RateSnapshot::from_headers(&common::headers(&all[..3])).expect("a snapshot");
    assert_eq!(defaults.resource, "core");
    assert_eq!(defaults.used, 13);
}

#[test]
fn snapshots_round_trip_through_json() {
    let s = snapshot("core", 42, NOW + 60);
    let json = s.to_json();
    let keys: Vec<&str> = json
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["limit", "remaining", "reset", "resource", "used"]);
    assert_eq!(RateSnapshot::from_json(&json), Some(s.clone()));
    assert_eq!(s.line(), "core:42/60");
}

#[test]
fn resources_and_keys() {
    assert_eq!(resource_for("/search/issues"), "search");
    assert_eq!(resource_for("/search/code"), "code_search");
    assert_eq!(resource_for("/search/code?q=x"), "code_search");
    assert_eq!(resource_for("/search/commits"), "search");
    assert_eq!(resource_for("/graphql"), "graphql");
    assert_eq!(resource_for("/repos/fixture-dev/repo"), "core");
    assert_eq!(book_key(&IdentityRef::Anonymous), "anonymous");
    let profile = IdentityRef::Profile {
        name: "dev".into(),
        fingerprint: "0123456789ab".into(),
    };
    assert_eq!(book_key(&profile), "0123456789ab");
}

#[test]
fn no_book_is_inert() {
    let book = RateBook::none();
    book.record("anonymous", &snapshot("core", 0, NOW + 60));
    assert!(book.read("anonymous").is_empty());
    assert!(book.refuse_before_send("anonymous", "core", NOW).is_none());
}

#[cfg(unix)]
mod files {
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::Arc;

    use super::*;
    use agh::config::Dirs;
    use agh::policy::{RouteRequest, RouteTarget, Router};
    use agh::retry::RetryPolicy;
    use agh::wire::Endpoints;
    use agh::wire::scripted::ScriptedTransport;
    use agh::{Call, Fx, Github, GithubBuilder, Kind, Sub};

    use crate::common;

    /// A client with `book`, anonymous, over `transport`.
    fn client(transport: &Arc<ScriptedTransport>, book: RateBook) -> Github {
        GithubBuilder::new(Router::empty())
            .transport(transport.clone())
            .endpoints(Endpoints::loopback(47_201, 47_202, 47_203, 47_204))
            .env(Arc::new(common::env()))
            .fx(Arc::new(Fx::simulated(NOW)))
            .caller("test")
            .retry(RetryPolicy::NONE)
            .rate_book(book)
            .build()
    }

    fn repo_get() -> Call {
        let slug = common::slug("fixture-dev/repo");
        Call::get(
            "/repos/fixture-dev/repo",
            RouteRequest::read(RouteTarget::Repo(slug)),
        )
    }

    fn mode(path: &std::path::Path) -> u32 {
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn record_writes_a_private_file_that_read_reads_back() {
        let root = common::scratch("rate-record");
        let dirs = Dirs::under(root.path());
        let book = RateBook::at(dirs.rate());
        book.record("anonymous", &snapshot("search", 9, NOW + 60));
        book.record("anonymous", &snapshot("core", 41, NOW + 60));
        book.record("anonymous", &snapshot("core", 40, NOW + 60));
        assert_eq!(mode(&dirs.rate()), 0o700);
        assert_eq!(mode(&dirs.rate().join("anonymous.json")), 0o600);
        assert_eq!(
            book.read("anonymous"),
            vec![
                snapshot("core", 40, NOW + 60),
                snapshot("search", 9, NOW + 60)
            ]
        );
        assert!(book.read("0123456789ab").is_empty());
    }

    #[test]
    fn a_spent_resource_is_refused_until_its_reset() {
        let root = common::scratch("rate-refuse");
        let book = RateBook::at(Dirs::under(root.path()).rate());
        book.record("anonymous", &snapshot("core", 0, NOW + 60));
        let refused = book
            .refuse_before_send("anonymous", "core", NOW)
            .expect("spent");
        assert_eq!(
            (refused.kind, refused.sub.clone()),
            (Kind::RateLimited, Sub::PreRefused)
        );
        assert_eq!(refused.reset, Some(NOW + 60));
        assert!(refused.why.contains("core"));
        assert!(
            book.refuse_before_send("anonymous", "core", NOW + 60)
                .is_none()
        );
        assert!(
            book.refuse_before_send("anonymous", "search", NOW)
                .is_none()
        );
        book.record("anonymous", &snapshot("core", 1, NOW + 60));
        assert!(book.refuse_before_send("anonymous", "core", NOW).is_none());
    }

    #[test]
    fn a_loose_rate_directory_disables_the_book_and_stays_loose() {
        let root = common::scratch("rate-loose");
        let dirs = Dirs::under(root.path());
        let rate = dirs.rate();
        std::fs::create_dir_all(&rate).expect("the rate directory");
        std::fs::set_permissions(&dirs.state, std::fs::Permissions::from_mode(0o700))
            .expect("chmod state");
        std::fs::set_permissions(&rate, std::fs::Permissions::from_mode(0o755))
            .expect("chmod rate");
        let book = RateBook::at(rate.clone());
        book.record("anonymous", &snapshot("core", 0, NOW + 60));
        assert!(!rate.join("anonymous.json").exists());
        assert_eq!(mode(&rate), 0o755);
        assert!(book.read("anonymous").is_empty());
        assert!(book.refuse_before_send("anonymous", "core", NOW).is_none());
    }

    #[test]
    fn an_answer_is_recorded_and_the_next_call_is_pre_refused() {
        let root = common::scratch("rate-pipeline");
        let book = RateBook::at(Dirs::under(root.path()).rate());
        let transport = Arc::new(ScriptedTransport::new());
        transport.answer(
            403,
            &[
                ("x-ratelimit-limit", "60"),
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1760003600"),
                ("x-ratelimit-resource", "core"),
            ],
            br#"{"message":"API rate limit exceeded"}"#,
        );
        let gh = client(&transport, book.clone());
        let error = gh.call(&repo_get()).expect_err("primary");
        assert_eq!(
            (error.kind, error.sub.clone()),
            (Kind::RateLimited, Sub::Primary)
        );
        assert_eq!(error.reset, Some(1_760_003_600));
        assert_eq!(book.read("anonymous").len(), 1);
        let again = gh.call(&repo_get()).expect_err("pre-refused");
        assert_eq!(again.sub, Sub::PreRefused);
        assert_eq!(again.reset, Some(1_760_003_600));
        assert_eq!(transport.requests().len(), 1);
    }

    #[test]
    fn a_pre_refused_call_sends_nothing() {
        let root = common::scratch("rate-pre");
        let book = RateBook::at(Dirs::under(root.path()).rate());
        book.record("anonymous", &snapshot("core", 0, NOW + 3600));
        let transport = Arc::new(ScriptedTransport::new());
        transport.answer(200, &[], b"{}");
        let gh = client(&transport, book);
        let error = gh.call(&repo_get()).expect_err("pre-refused");
        assert_eq!(error.exit(), Kind::RateLimited.exit());
        assert!(transport.requests().is_empty());
        assert_eq!(gh.meter().requests, 0);
    }

    #[test]
    fn the_free_rate_limit_endpoint_is_never_pre_refused() {
        let root = common::scratch("rate-free");
        let book = RateBook::at(Dirs::under(root.path()).rate());
        book.record("anonymous", &snapshot("core", 0, NOW + 60));
        let transport = Arc::new(ScriptedTransport::new());
        transport.answer(200, &[], br#"{"resources":{}}"#);
        let gh = client(&transport, book);
        let call = Call::get(
            "/rate_limit",
            RouteRequest::read(RouteTarget::Unscoped).with_anonymous(true),
        );
        let answered = gh.call(&call).expect("answered though core is spent");
        assert_eq!(answered.status, 200);
        assert_eq!(transport.requests().len(), 1);
        let again = gh.call(&repo_get()).expect_err("core is still spent");
        assert_eq!(again.sub, Sub::PreRefused);
        assert_eq!(transport.requests().len(), 1);
    }
}
