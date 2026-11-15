// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Tests for rate snapshots and the advisory rate book.

use agh::error::IdentityRef;
use agh::rate::{RateBook, RateSnapshot, book_key, resource_for};

use crate::common;

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
    let mut blank_resource = all.to_vec();
    blank_resource[3] = ("X-RateLimit-Resource", "   ");
    assert_eq!(
        RateSnapshot::from_headers(&common::headers(&blank_resource))
            .expect("blank resource defaults to core")
            .resource,
        "core"
    );
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

    fn authenticated(
        transport: &Arc<ScriptedTransport>,
        book: RateBook,
        root: &std::path::Path,
        reserve: u64,
    ) -> Github {
        GithubBuilder::new(common::router(&common::canonical_config(root), root))
            .transport(transport.clone())
            .endpoints(Endpoints::loopback(47_201, 47_202, 47_203, 47_204))
            .env(Arc::new(common::env()))
            .fx(Arc::new(Fx::simulated(NOW)))
            .caller("quota-test")
            .rate_book(book)
            .core_reserve(reserve)
            .retry(RetryPolicy::NONE)
            .build()
    }

    #[test]
    fn separate_clients_revalidate_one_cached_representation_and_never_hide_a_change() {
        let root = common::scratch("rate-conditional");
        let fake = agh_fake::FakeGithub::start();
        common::canonical_tokens(root.path(), &fake, true, false);
        let book = RateBook::at(Dirs::under(root.path()).rate());
        let transport = Arc::new(ScriptedTransport::new());
        let call = repo_get();
        transport.answer(
            200,
            &[("etag", "\"one\""), ("content-type", "application/json")],
            b"{\"id\":1}",
        );
        let first = authenticated(&transport, book.clone(), root.path(), 1000)
            .call(&call)
            .unwrap();
        for _ in 0..9 {
            transport.answer(304, &[], b"");
            let answer = authenticated(&transport, book.clone(), root.path(), 1000)
                .call(&call)
                .unwrap();
            assert_eq!(answer.body, first.body);
        }
        for request in transport.requests().iter().skip(1) {
            assert_eq!(
                request
                    .headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("if-none-match"))
                    .map(|(_, v)| v.as_str()),
                Some("\"one\"")
            );
        }
        let rows = book.calls_in_hour(NOW / 3600).unwrap();
        assert_eq!(rows.len(), 10);
        assert_eq!(
            rows.iter()
                .filter(|row| row["status"].as_u64() == Some(200))
                .count(),
            1,
            "ten unchanged reads cost one counted request instead of ten"
        );
        transport.answer(200, &[("etag", "\"two\"")], b"{\"id\":2}");
        let changed = authenticated(&transport, book.clone(), root.path(), 1000)
            .call(&call)
            .unwrap();
        assert_ne!(changed.body, first.body);
        transport.answer(403, &[], b"{}");
        assert!(
            authenticated(&transport, book.clone(), root.path(), 1000)
                .call(&call)
                .is_err(),
            "an error never serves the old body"
        );
        assert!(
            book.calls_in_hour(NOW / 3600)
                .unwrap()
                .iter()
                .all(|row| row.get("body").is_none())
        );
    }

    #[test]
    fn background_stops_at_the_reserve_and_cutter_reads_still_send() {
        let root = common::scratch("rate-reserve");
        let fake = agh_fake::FakeGithub::start();
        common::canonical_tokens(root.path(), &fake, true, false);
        let book = RateBook::at(Dirs::under(root.path()).rate());
        let transport = Arc::new(ScriptedTransport::new());
        transport.answer(
            200,
            &[
                ("x-ratelimit-limit", "5000"),
                ("x-ratelimit-remaining", "1000"),
                ("x-ratelimit-reset", "1760003600"),
            ],
            b"{}",
        );
        let background = authenticated(&transport, book.clone(), root.path(), 1000);
        background.call(&repo_get()).unwrap();
        assert_eq!(
            background.call(&repo_get()).unwrap_err().sub,
            Sub::PreRefused
        );
        assert_eq!(
            transport.requests().len(),
            1,
            "background sends no request in the reserve"
        );
        transport.answer(200, &[], b"{}");
        authenticated(&transport, book, root.path(), 0)
            .call(&repo_get())
            .unwrap();
        assert_eq!(transport.requests().len(), 2, "the cutter can still read");
    }

    /// A registry whose one record names `pid`, started at `start` in boot `boot`, as a
    /// release line's holder: what a cutter writes when it takes its fleet line.
    fn line_record(registry: &std::path::Path, pid: u32, boot: &str, start: u64) {
        std::fs::create_dir_all(registry).unwrap();
        let identity = aterm_ctl::release_holder::Identity {
            pid,
            boot: boot.to_owned(),
            start,
        };
        let record = aterm_json::json!({"holder": {"pid": pid, "boot": boot, "start": start}});
        std::fs::write(
            registry.join(identity.record_name("0.200.0")),
            record.to_string(),
        )
        .unwrap();
    }

    /// This test process, as the kernel names it: its pid, boot and start.
    fn this_process() -> (u32, String, u64) {
        let pid = std::process::id();
        let start = aterm_uds::process::read_process_start(i32::try_from(pid).unwrap())
            .expect("this process's start reads")
            .boot
            .0;
        let boot = aterm_uds::process::boot_id().expect("this host names its boot");
        (pid, boot, start)
    }

    const AT_THE_RESERVE: [(&str, &str); 3] = [
        ("x-ratelimit-limit", "5000"),
        ("x-ratelimit-remaining", "995"),
        ("x-ratelimit-reset", "1760003600"),
    ];

    /// R32: a cut's own process tree is the cutter for a client that never said so. The
    /// client is the default one (reserve 1000), as the `agh` command builds it.
    /// NEGATIVE CONTROLS: with no record, with a record of this pid under another start (a pid
    /// that changed hands), and with one of another boot, the same read is refused at the
    /// reserve and nothing is sent.
    #[test]
    fn a_read_in_a_cuts_own_tree_spends_the_reserve_and_a_stale_or_forged_record_does_not() {
        let (pid, boot, start) = this_process();
        let other_boot = if boot.starts_with('0') { "1" } else { "0" };
        let read = |record: Option<(&str, u64)>| {
            let root = common::scratch("rate-cut-tree");
            let fake = agh_fake::FakeGithub::start();
            common::canonical_tokens(root.path(), &fake, true, false);
            let registry = root.path().join("release-lines");
            if let Some((boot, start)) = record {
                line_record(&registry, pid, boot, start);
            }
            let transport = Arc::new(ScriptedTransport::new());
            transport.answer(200, &AT_THE_RESERVE, b"{}");
            transport.answer(200, &AT_THE_RESERVE, b"{}");
            let client = GithubBuilder::new(common::router(
                &common::canonical_config(root.path()),
                root.path(),
            ))
            .transport(transport.clone())
            .endpoints(Endpoints::loopback(47_201, 47_202, 47_203, 47_204))
            .env(Arc::new(common::env()))
            .fx(Arc::new(Fx::simulated(NOW)))
            .caller("quota-test")
            .rate_book(RateBook::at(Dirs::under(root.path()).rate()))
            .cut_registry(Some(registry))
            .retry(RetryPolicy::NONE)
            .build();
            client.call(&repo_get()).unwrap();
            let second = client
                .call(&repo_get())
                .map(|_| ())
                .map_err(|e| e.sub.clone());
            (second, transport.requests().len())
        };
        assert_eq!(
            read(Some((&boot, start))),
            (Ok(()), 2),
            "the cut's own tree"
        );
        assert_eq!(read(None), (Err(Sub::PreRefused), 1), "no record");
        assert_eq!(
            read(Some((&boot, start.wrapping_add(1)))),
            (Err(Sub::PreRefused), 1),
            "another holder of this pid"
        );
        assert_eq!(
            read(Some((other_boot, start))),
            (Err(Sub::PreRefused), 1),
            "another boot's record"
        );
    }

    /// What the reserve never governed is unchanged by a cut's record: a spent allowance
    /// refuses the cut's tree as it refuses everyone (the wait for the reset is the release's),
    /// and an anonymous read at the reserve is sent with the record and without it.
    #[test]
    fn a_cuts_record_changes_neither_a_spent_allowance_nor_an_anonymous_read() {
        let (pid, boot, start) = this_process();
        for recorded in [true, false] {
            let root = common::scratch("rate-cut-unchanged");
            let fake = agh_fake::FakeGithub::start();
            common::canonical_tokens(root.path(), &fake, true, false);
            let registry = root.path().join("release-lines");
            if recorded {
                line_record(&registry, pid, &boot, start);
            }
            let book = RateBook::at(Dirs::under(root.path()).rate());
            let transport = Arc::new(ScriptedTransport::new());
            let spent = [
                ("x-ratelimit-limit", "5000"),
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1760003600"),
            ];
            transport.answer(200, &spent, b"{}");
            let own = GithubBuilder::new(common::router(
                &common::canonical_config(root.path()),
                root.path(),
            ))
            .transport(transport.clone())
            .endpoints(Endpoints::loopback(47_201, 47_202, 47_203, 47_204))
            .env(Arc::new(common::env()))
            .fx(Arc::new(Fx::simulated(NOW)))
            .caller("quota-test")
            .rate_book(book.clone())
            .cut_registry(Some(registry.clone()))
            .retry(RetryPolicy::NONE)
            .build();
            own.call(&repo_get()).unwrap();
            let refused = own.call(&repo_get()).unwrap_err();
            assert_eq!(
                (refused.kind, refused.sub.clone(), refused.reset),
                (Kind::RateLimited, Sub::PreRefused, Some(1_760_003_600)),
                "recorded={recorded}: a spent allowance refuses the cut too"
            );
            assert_eq!(transport.requests().len(), 1);

            let anonymous = Arc::new(ScriptedTransport::new());
            anonymous.answer(200, &AT_THE_RESERVE, b"{}");
            anonymous.answer(200, &AT_THE_RESERVE, b"{}");
            let nobody = GithubBuilder::new(Router::empty())
                .transport(anonymous.clone())
                .endpoints(Endpoints::loopback(47_201, 47_202, 47_203, 47_204))
                .env(Arc::new(common::env()))
                .fx(Arc::new(Fx::simulated(NOW)))
                .caller("test")
                .retry(RetryPolicy::NONE)
                .rate_book(book)
                .cut_registry(Some(registry))
                .build();
            nobody.call(&repo_get()).unwrap();
            nobody.call(&repo_get()).unwrap();
            assert_eq!(
                anonymous.requests().len(),
                2,
                "recorded={recorded}: no reserve is kept from an anonymous read"
            );
        }
    }

    /// R32 at the command line, which is how a cut's publication engine reads: `agh api` at the
    /// reserve exits 11 `rate_limited`, and exits 0 once a release-line record names this
    /// process as its holder. NEGATIVE CONTROL: the same command with a record of another start.
    #[test]
    fn the_agh_command_in_a_cuts_tree_reads_past_the_reserve() {
        let (pid, boot, start) = this_process();
        let run = |start: Option<u64>| {
            let root = common::scratch("rate-cut-cli");
            let fake = agh_fake::FakeGithub::start();
            common::canonical_tokens(root.path(), &fake, true, false);
            let dirs = Dirs::under(root.path());
            common::write_config(&dirs, &common::canonical_config(root.path()));
            if let Some(start) = start {
                line_record(
                    &root
                        .path()
                        .join("state/harness/claude-harness/release-lines"),
                    pid,
                    &boot,
                    start,
                );
            }
            let transport = Arc::new(ScriptedTransport::new());
            transport.answer(200, &AT_THE_RESERVE, b"{}");
            transport.answer(200, &AT_THE_RESERVE, b"{}");
            let codes: Vec<u8> = (0..2)
                .map(|_| {
                    let (mut ctx, _captured) = agh::cli::Ctx::for_tests(
                        root.path(),
                        transport.clone(),
                        Endpoints::loopback(47_201, 47_202, 47_203, 47_204),
                        common::env(),
                    );
                    common::run_cli(&mut ctx, &["api", "/repos/fixture-dev/repo"])
                })
                .collect();
            (codes, transport.requests().len())
        };
        assert_eq!(run(Some(start)), (vec![0, 0], 2), "in the cut's tree");
        assert_eq!(run(None), (vec![0, 11], 1), "no record: the reserve holds");
        assert_eq!(
            run(Some(start.wrapping_add(1))),
            (vec![0, 11], 1),
            "a record of another start"
        );
    }

    #[test]
    fn a_concurrent_background_burst_leaves_the_reserve_instead_of_spending_old_snapshots() {
        for (reserve, expected) in [(1000, 1), (0, 32)] {
            let root = common::scratch("rate-burst");
            let fake = agh_fake::FakeGithub::start();
            common::canonical_tokens(root.path(), &fake, true, false);
            let book = RateBook::at(Dirs::under(root.path()).rate());
            let transport = Arc::new(ScriptedTransport::new());
            for _ in 0..32 {
                transport.answer(
                    200,
                    &[
                        ("x-ratelimit-limit", "5000"),
                        ("x-ratelimit-remaining", "1000"),
                        ("x-ratelimit-reset", "1760003600"),
                    ],
                    b"{}",
                );
            }
            let gh = authenticated(&transport, book, root.path(), reserve);
            let barrier = std::sync::Barrier::new(32);
            std::thread::scope(|scope| {
                for _ in 0..32 {
                    let (gh, barrier) = (&gh, &barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        if let Err(error) = gh.call(&repo_get()) {
                            assert_eq!(error.sub, Sub::PreRefused);
                        }
                    });
                }
            });
            assert_eq!(
                transport.requests().len(),
                expected,
                "reserve={reserve}: one background send observes the boundary; foreground control sends every call"
            );
        }
    }

    #[test]
    fn attribution_keeps_each_retry_and_omits_untrusted_failure_details() {
        use agh::wire::scripted::Scripted;
        use agh::wire::{Phase, WireError, WireKind};
        use aterm_json::Value;

        let root = common::scratch("rate-attempts");
        let book = RateBook::at(Dirs::under(root.path()).rate());
        let transport = Arc::new(ScriptedTransport::new());
        transport.push(Scripted::Fail(WireError {
            phase: Phase::Sent,
            kind: WireKind::Reset,
            detail: "untrusted-failure-canary".into(),
        }));
        transport.answer(200, &[], b"{}");
        let gh = client(&transport, book.clone());
        gh.call(&repo_get().retry(Some(RetryPolicy::STANDARD)))
            .unwrap();
        let rows = book.calls_in_hour(NOW / 3600).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["wire_error"], Value::from("reset"));
        assert_eq!(rows[0]["wire_phase"], Value::from("sent"));
        assert!(rows[0]["status"].is_null());
        assert_eq!(rows[1]["status"], Value::from(200));
        assert!(rows[1]["rate"].is_null());
        assert!(
            !Value::Array(rows)
                .to_string()
                .contains("untrusted-failure-canary")
        );
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
        assert!(
            book.refuse_before_send("anonymous", "core", NOW).is_some(),
            "a reordered response cannot restore spent quota"
        );
        book.record("anonymous", &snapshot("core", 1, NOW + 120));
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

    /// NEGATIVE CONTROL: Mixed-case `X-RateLimit-Resource` headers normalize to lowercase so
    /// `refuse_before_send(..., "core", ...)` still matches, invalid resource characters/lengths
    /// are rejected rather than corrupting `line()` or `RateBook`, and recording >64 distinct
    /// unspent resources stays bounded well under `BOOK_MAX` without evicting a spent resource.
    #[test]
    fn rate_book_normalizes_resources_rejects_malformed_names_and_bounds_entries() {
        let mixed = [
            ("X-RateLimit-Limit", "60"),
            ("X-RateLimit-Remaining", "0"),
            ("X-RateLimit-Reset", "1760003600"),
            ("X-RateLimit-Resource", "Core"),
        ];
        let parsed = RateSnapshot::from_headers(&common::headers(&mixed)).expect("mixed-case Core");
        assert_eq!(parsed.resource, "core");

        for bad in [
            "core:0/0",
            "core\nfake:0/60",
            "core space",
            "../escape",
            &"r".repeat(65),
        ] {
            let headers = [
                ("X-RateLimit-Limit", "60"),
                ("X-RateLimit-Remaining", "0"),
                ("X-RateLimit-Reset", "1760003600"),
                ("X-RateLimit-Resource", bad),
            ];
            assert!(
                RateSnapshot::from_headers(&common::headers(&headers)).is_none(),
                "must reject {bad:?}"
            );
        }

        let root = common::scratch("rate-bounded");
        let rate_dir = Dirs::under(root.path()).rate();
        let book = RateBook::at(rate_dir.clone());
        book.record("anonymous", &snapshot("Core", 0, NOW + 60));
        book.record("anonymous", &snapshot("core\ninjected", 0, NOW + 60));
        for i in 0..200_u64 {
            book.record(
                "anonymous",
                &snapshot(&format!("custom_res_{i}"), 50, NOW + 10_000 + i),
            );
        }

        let held = book.read("anonymous");
        assert_eq!(held.len(), 64, "book must cap stored resources at 64");
        assert!(
            book.refuse_before_send("anonymous", "core", NOW).is_some(),
            "spent core entry must survive unspent resource churn and BOOK_MAX cap"
        );
        let size = std::fs::metadata(rate_dir.join("anonymous.json"))
            .expect("book metadata")
            .len();
        assert!(
            size < 16 * 1024,
            "book size {size} must stay far below 64 KiB"
        );
    }
}
