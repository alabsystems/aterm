// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The Link walker (the plan's §4.6.16, §8; design §13.1), over a scripted transport.

use std::sync::Arc;

use agh::error::IdentityRef;
use agh::page::{
    Consistency, End, LinkScope, ListOpts, NextPage, link_next, next_link, walk, walk_pages,
};
use agh::policy::{RouteRequest, RouteTarget, Router};
use agh::wire::Method;
use agh::wire::scripted::ScriptedTransport;
use agh::{Call, Github, Kind, Sub};
use aterm_json::Value;

use crate::common;

/// The scripted api origin's port.
const API: u16 = 47_301;

/// The repository the listings walk.
const REPO: &str = "fixture-dev/repo";

/// The first call of a release listing.
fn releases() -> Call {
    Call::get(
        "/repos/fixture-dev/repo/releases",
        RouteRequest::read(RouteTarget::Repo(common::slug(REPO))),
    )
}

/// A JSON array of `{"id": i}` for `ids`.
fn page(ids: std::ops::Range<u64>) -> Vec<u8> {
    let items: Vec<String> = ids
        .map(|i| format!(r#"{{"id":{i},"tag_name":"v{i}"}}"#))
        .collect();
    format!("[{}]", items.join(",")).into_bytes()
}

/// A `Link` header whose next is `/repos/fixture-dev/repo/releases?…&page={page}`.
fn next_to(page: u32) -> String {
    format!(
        r#"<{}/repos/fixture-dev/repo/releases?per_page=100&page={page}>; rel="next""#,
        common::base(API)
    )
}

/// Queue a page answer: its body, its ETag and, when `next` is given, its Link.
fn serve(transport: &ScriptedTransport, body: &[u8], etag: &str, next: Option<u32>) {
    let link = next.map(next_to);
    let mut headers = vec![("etag", etag)];
    if let Some(link) = link.as_deref() {
        headers.push(("link", link));
    }
    transport.answer(200, &headers, body);
}

fn ids(items: &[Value]) -> Vec<u64> {
    items
        .iter()
        .map(|item| item.get("id").and_then(Value::as_u64).expect("an id"))
        .collect()
}

#[test]
fn link_next_finds_the_next_relation() {
    let link = r#"<https://api.github.com/x?page=2>; rel="next", <https://api.github.com/x?page=9>; rel="last""#;
    assert_eq!(
        link_next(link).as_deref(),
        Some("https://api.github.com/x?page=2")
    );
    let reordered =
        r#"<https://a/x?page=1>; rel="prev", <https://a/x?path=a,b&page=3>; rel="next""#;
    assert_eq!(
        link_next(reordered).as_deref(),
        Some("https://a/x?path=a,b&page=3")
    );
    assert_eq!(link_next(r#"<https://a/x?page=9>; rel="last""#), None);
    assert_eq!(link_next(""), None);
    assert_eq!(
        link_next(r#"<https://a/x?page=2>; rel="prev next""#).as_deref(),
        Some("https://a/x?page=2")
    );
}

#[test]
fn next_link_accepts_the_listing_s_own_pages() {
    let e = common::scripted_endpoints(API);
    let first = "/repos/fixture-dev/repo/releases";
    let repo = LinkScope::Repo(common::slug(REPO));
    let url = format!(
        "{}/repos/Fixture-Dev/REPO/releases?per_page=100&page=2",
        common::base(API)
    );
    assert_eq!(
        next_link(&url, &e, &repo, None, first, 1).expect("accepted"),
        NextPage {
            path_and_query: "/repos/Fixture-Dev/REPO/releases?per_page=100&page=2".into(),
            page: 2,
        }
    );
    let numeric = format!(
        "{}/repositories/1000/releases?per_page=100&page=3",
        common::base(API)
    );
    assert_eq!(
        next_link(&numeric, &e, &repo, Some(1000), first, 2)
            .expect("accepted")
            .page,
        3
    );
    let owner = LinkScope::Owner { id: 100 };
    for path in [
        "/orgs/fixture-dev/repos?page=2",
        "/organizations/100/repos?page=2",
        "/user/100/repos?page=2",
    ] {
        let url = format!("{}{path}", common::base(API));
        assert!(
            next_link(&url, &e, &owner, None, "/orgs/fixture-dev/repos", 1).is_ok(),
            "{path}"
        );
    }
    let exact = format!("{}/user/repos?affiliation=owner&page=2", common::base(API));
    assert!(next_link(&exact, &e, &LinkScope::Exact, None, "/user/repos", 1).is_ok());
}

#[test]
fn next_link_refuses_everything_else() {
    let e = common::scripted_endpoints(API);
    let first = "/repos/fixture-dev/repo/releases";
    let repo = LinkScope::Repo(common::slug(REPO));
    let refused = |url: String, scope: &LinkScope, repo_id: Option<u64>, first: &str, last: u32| {
        let error = next_link(&url, &e, scope, repo_id, first, last).expect_err(&url);
        assert_eq!(
            (error.kind, error.sub.clone()),
            (Kind::CouldNotRun, Sub::Protocol),
            "{url}"
        );
        assert!(error.why.contains(&url), "{url}");
    };
    let b = common::base(API);
    refused(
        "http://127.0.0.1:47302/repos/fixture-dev/repo/releases?page=2".to_string(),
        &repo,
        None,
        first,
        1,
    );
    refused(
        "https://api.github.com/repos/fixture-dev/repo/releases?page=2".into(),
        &repo,
        None,
        first,
        1,
    );
    refused(
        format!("{b}/repos/fixture-dev/other/releases?page=2"),
        &repo,
        None,
        first,
        1,
    );
    refused(
        format!("{b}/repos/fixture-dev/repo/tags?page=2"),
        &repo,
        None,
        first,
        1,
    );
    refused(
        format!("{b}/repositories/1000/releases?page=2"),
        &repo,
        None,
        first,
        1,
    );
    refused(
        format!("{b}/repositories/1001/releases?page=2"),
        &repo,
        Some(1000),
        first,
        1,
    );
    refused(
        format!("{b}/repos/fixture-dev/repo/releases?page=2"),
        &repo,
        None,
        first,
        2,
    );
    refused(
        format!("{b}/repos/fixture-dev/repo/releases?page=1"),
        &repo,
        None,
        first,
        2,
    );
    refused(
        format!("{b}/repos/fixture-dev/repo/releases"),
        &repo,
        None,
        first,
        1,
    );
    refused(
        format!("{b}/repos/fixture-dev/repo/releases?per_page=101&page=2"),
        &repo,
        None,
        first,
        1,
    );
    refused(
        format!("ftp://127.0.0.1:{API}/x?page=2"),
        &repo,
        None,
        first,
        1,
    );
    let owner = LinkScope::Owner { id: 100 };
    refused(
        format!("{b}/organizations/101/repos?page=2"),
        &owner,
        None,
        "/orgs/fixture-dev/repos",
        1,
    );
    refused(
        format!("{b}/orgs/fixture-ext/repos?page=2"),
        &owner,
        None,
        "/orgs/fixture-dev/repos",
        1,
    );
    refused(
        format!("{b}/user/repos?page=2&x=1"),
        &LinkScope::Exact,
        None,
        "/users/fixture-ext/repos",
        1,
    );
}

#[test]
fn a_cursor_paged_next_link_is_refused_naming_its_cursor() {
    let e = common::scripted_endpoints(API);
    let b = common::base(API);
    for (path, cursor) in [
        ("/repositories?since=369", "since"),
        ("/repositories?cursor=v1_9", "cursor"),
        ("/repositories?per_page=100&after=Y3Vy", "after"),
        ("/repositories?before=Y3Vy", "before"),
    ] {
        let url = format!("{b}{path}");
        let error =
            next_link(&url, &e, &LinkScope::Exact, None, "/repositories", 1).expect_err(&url);
        assert_eq!(
            (error.kind, error.sub.clone(), error.exit()),
            (Kind::CouldNotRun, Sub::Protocol, 3),
            "{url}"
        );
        assert!(error.why.contains(&format!("{cursor}=")), "{}", error.why);
        assert!(error.next.contains("--all"), "{}", error.next);
    }
}

#[test]
fn a_next_link_without_a_page_or_a_cursor_keeps_the_kinds_next() {
    let e = common::scripted_endpoints(API);
    let b = common::base(API);
    let plain = agh::Error::could_not_run("x").next.clone();
    for path in [
        "/repositories?page=abc",
        "/repositories?per_page=100",
        "/repositories",
    ] {
        let url = format!("{b}{path}");
        let error =
            next_link(&url, &e, &LinkScope::Exact, None, "/repositories", 1).expect_err(&url);
        assert_eq!(
            (error.kind, error.sub.clone(), error.exit()),
            (Kind::CouldNotRun, Sub::Protocol, 3),
            "{url}"
        );
        assert_eq!(error.next, plain, "{url}");
    }
}

#[test]
fn a_snapshot_of_three_pages_is_revalidated_with_304s() {
    let transport = Arc::new(ScriptedTransport::new());
    serve(&transport, &page(1..101), "W/\"p1\"", Some(2));
    serve(&transport, &page(101..201), "W/\"p2\"", Some(3));
    serve(&transport, &page(201..251), "W/\"p3\"", None);
    for _ in 0..3 {
        transport.answer(304, &[], b"");
    }
    let gh = common::scripted(&transport, API);
    let listing = walk(
        &gh,
        releases(),
        &ListOpts::STANDARD,
        &LinkScope::Repo(common::slug(REPO)),
    )
    .expect("the listing");
    assert_eq!(listing.items.len(), 250);
    assert_eq!(ids(&listing.items), (1..251).collect::<Vec<u64>>());
    assert_eq!(listing.pages, 3);
    assert_eq!(listing.requests, 6);
    assert_eq!(listing.end, End::Complete);
    assert_eq!(listing.consistency, Consistency::Snapshot);
    assert_eq!(listing.as_, IdentityRef::Anonymous);
    let seen = transport.requests();
    assert_eq!(seen.len(), 6);
    assert_eq!(
        seen[0].url,
        format!(
            "{}/repos/fixture-dev/repo/releases?per_page=100",
            common::base(API)
        )
    );
    assert!(seen.iter().all(|r| r.method == Method::Get));
    let validators: Vec<Option<String>> = seen
        .iter()
        .map(|r| {
            r.headers
                .iter()
                .find(|(n, _)| n == "If-None-Match")
                .map(|(_, v)| v.clone())
        })
        .collect();
    assert_eq!(
        validators,
        vec![
            None,
            None,
            None,
            Some("W/\"p1\"".into()),
            Some("W/\"p2\"".into()),
            Some("W/\"p3\"".into()),
        ]
    );
    assert_eq!(seen[4].url, seen[1].url);
}

#[test]
fn a_single_page_needs_no_revalidation() {
    let transport = Arc::new(ScriptedTransport::new());
    serve(&transport, &page(1..4), "W/\"p1\"", None);
    let gh = common::scripted(&transport, API);
    let listing = walk(
        &gh,
        releases(),
        &ListOpts::STANDARD,
        &LinkScope::Repo(common::slug(REPO)),
    )
    .expect("the listing");
    assert_eq!((listing.pages, listing.requests), (1, 1));
}

#[test]
fn the_page_cap_is_incomplete_unless_partial() {
    let transport = Arc::new(ScriptedTransport::new());
    serve(&transport, &page(1..101), "W/\"p1\"", Some(2));
    serve(&transport, &page(101..201), "W/\"p2\"", Some(3));
    let gh = common::scripted(&transport, API);
    let opts = ListOpts::STANDARD.with_max_pages(2);
    let error =
        walk(&gh, releases(), &opts, &LinkScope::Repo(common::slug(REPO))).expect_err("capped");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (Kind::Incomplete, Sub::Capped { pages: 2 })
    );
    assert!(error.next.contains("--max-pages") && error.next.contains("--partial"));

    let transport = Arc::new(ScriptedTransport::new());
    serve(&transport, &page(1..101), "W/\"p1\"", Some(2));
    serve(&transport, &page(101..201), "W/\"p2\"", Some(3));
    transport.answer(304, &[], b"");
    transport.answer(304, &[], b"");
    let gh = common::scripted(&transport, API);
    let listing = walk(
        &gh,
        releases(),
        &opts.with_partial(true),
        &LinkScope::Repo(common::slug(REPO)),
    )
    .expect("partial");
    assert_eq!(listing.end, End::Capped { pages: 2 });
    assert_eq!(listing.items.len(), 200);
    assert!(!listing.is_complete());
    let error = listing.complete().expect_err("capped");
    assert_eq!(error.sub, Sub::Capped { pages: 2 });
}

#[test]
fn a_limit_stops_the_walk() {
    let transport = Arc::new(ScriptedTransport::new());
    serve(&transport, &page(1..101), "W/\"p1\"", Some(2));
    serve(&transport, &page(101..201), "W/\"p2\"", Some(3));
    transport.answer(304, &[], b"");
    transport.answer(304, &[], b"");
    let gh = common::scripted(&transport, API);
    let opts = ListOpts::STANDARD.with_limit(Some(150));
    let listing =
        walk(&gh, releases(), &opts, &LinkScope::Repo(common::slug(REPO))).expect("limited");
    assert_eq!(listing.end, End::Limited { at: 150 });
    assert_eq!(listing.items.len(), 150);
    assert_eq!(listing.pages, 2);
}

#[test]
fn a_limit_met_exactly_at_the_end_is_complete() {
    let transport = Arc::new(ScriptedTransport::new());
    serve(&transport, &page(1..4), "W/\"p1\"", None);
    let gh = common::scripted(&transport, API);
    let opts = ListOpts::STANDARD.with_limit(Some(3));
    let listing =
        walk(&gh, releases(), &opts, &LinkScope::Repo(common::slug(REPO))).expect("complete");
    assert_eq!(listing.end, End::Complete);
}

#[test]
fn one_tear_is_rescanned_and_answered() {
    let transport = Arc::new(ScriptedTransport::new());
    serve(&transport, &page(1..101), "W/\"p1\"", Some(2));
    serve(&transport, &page(101..151), "W/\"p2\"", None);
    transport.answer(304, &[], b"");
    serve(&transport, &page(101..150), "W/\"p2b\"", None);
    serve(&transport, &page(1..101), "W/\"p1\"", Some(2));
    serve(&transport, &page(101..150), "W/\"p2b\"", None);
    transport.answer(304, &[], b"");
    transport.answer(304, &[], b"");
    let gh = common::scripted(&transport, API);
    let listing = walk(
        &gh,
        releases(),
        &ListOpts::STANDARD,
        &LinkScope::Repo(common::slug(REPO)),
    )
    .expect("the rescan is whole");
    assert_eq!(listing.items.len(), 149);
    assert_eq!(listing.requests, 8);
    assert_eq!(transport.remaining(), 0);
}

#[test]
fn two_tears_are_torn() {
    let transport = Arc::new(ScriptedTransport::new());
    for pass in 0..2u64 {
        serve(&transport, &page(1..101), "W/\"p1\"", Some(2));
        serve(&transport, &page(101..151 - pass), "W/\"p2\"", None);
        transport.answer(304, &[], b"");
        serve(&transport, &page(101..140), "W/\"p2c\"", None);
    }
    let gh = common::scripted(&transport, API);
    let error = walk(
        &gh,
        releases(),
        &ListOpts::STANDARD,
        &LinkScope::Repo(common::slug(REPO)),
    )
    .expect_err("torn");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (Kind::Incomplete, Sub::Torn { page: 2 })
    );
    assert_eq!(error.repo.as_deref(), Some("fixture-dev/repo"));
    assert_eq!(transport.remaining(), 0);
}

#[test]
fn a_download_count_change_is_no_tear() {
    let body = |count: u64| {
        format!(
            r#"[{{"id":1,"download_count":{count},"reactions":{{"total_count":{count}}},"assets":[{{"id":9,"download_count":{count}}}]}}]"#
        )
        .into_bytes()
    };
    let transport = Arc::new(ScriptedTransport::new());
    serve(&transport, &page(10..11), "W/\"p1\"", Some(2));
    serve(&transport, &body(1), "W/\"p2\"", None);
    transport.answer(304, &[], b"");
    serve(&transport, &body(7), "W/\"p2x\"", None);
    let gh = common::scripted(&transport, API);
    let listing = walk(
        &gh,
        releases(),
        &ListOpts::STANDARD,
        &LinkScope::Repo(common::slug(REPO)),
    )
    .expect("unchanged on its stable form");
    assert_eq!(listing.requests, 4);
    assert_eq!(transport.remaining(), 0);
}

/// A repository object of an organization's listing, `stars` setting its counters and
/// timestamps.
fn repository(id: u64, name: &str, stars: u64) -> String {
    format!(
        r#"{{"id":{id},"full_name":"fixture-dev/{name}","private":false,"stargazers_count":{stars},"watchers_count":{stars},"watchers":{stars},"forks_count":1,"forks":1,"open_issues_count":2,"open_issues":2,"size":10,"pushed_at":"2026-10-19T00:00:0{stars}Z","updated_at":"2026-10-19T00:00:0{stars}Z"}}"#
    )
}

/// Serves one pass of a two-page organization repository listing: page 1 (next page 2), then
/// `second` as page 2.
fn serve_repositories(transport: &ScriptedTransport, second: &[String], etag: &str) {
    let next = format!(
        r#"<{}/organizations/100/repos?per_page=100&page=2>; rel="next""#,
        common::base(API)
    );
    let first = format!("[{}]", repository(1, "one", 1));
    transport.answer(
        200,
        &[("etag", "W/\"r1\""), ("link", next.as_str())],
        first.as_bytes(),
    );
    transport.answer(
        200,
        &[("etag", etag)],
        format!("[{}]", second.join(",")).as_bytes(),
    );
}

/// The walk of `/orgs/fixture-dev/repos` (organization id 100) under Snapshot.
fn walk_repositories(gh: &Github) -> agh::Result<agh::page::Listing<Value>> {
    let call = Call::get(
        "/orgs/fixture-dev/repos",
        RouteRequest::read(RouteTarget::Owner("fixture-dev".into())),
    );
    walk(gh, call, &ListOpts::STANDARD, &LinkScope::Owner { id: 100 })
}

#[test]
fn a_repository_counter_change_is_no_tear() {
    // Page 2's repository gains stars, a push and an update in both revalidations, which
    // tore the listing when only `download_count` was volatile.
    let transport = Arc::new(ScriptedTransport::new());
    serve_repositories(&transport, &[repository(2, "two", 1)], "W/\"r2\"");
    transport.answer(304, &[], b"");
    let changed = format!("[{}]", repository(2, "two", 7));
    transport.answer(200, &[("etag", "W/\"r2x\"")], changed.as_bytes());
    let gh = common::scripted(&transport, API);
    let listing = walk_repositories(&gh).expect("unchanged on its stable form");
    assert_eq!(listing.end, End::Complete);
    assert_eq!(ids(&listing.items), vec![1, 2]);
    assert_eq!(listing.requests, 4);
    assert_eq!(transport.remaining(), 0);
}

#[test]
fn a_repository_added_to_the_listing_is_still_a_change() {
    // The negative control: a third repository on page 2 is a change, so the walk rescans
    // and answers the rescan's listing.
    let transport = Arc::new(ScriptedTransport::new());
    serve_repositories(&transport, &[repository(2, "two", 1)], "W/\"r2\"");
    transport.answer(304, &[], b"");
    let grown = [repository(2, "two", 1), repository(3, "three", 1)];
    let body = format!("[{}]", grown.join(","));
    transport.answer(200, &[("etag", "W/\"r2x\"")], body.as_bytes());
    serve_repositories(&transport, &grown, "W/\"r2x\"");
    transport.answer(304, &[], b"");
    transport.answer(304, &[], b"");
    let gh = common::scripted(&transport, API);
    let listing = walk_repositories(&gh).expect("the rescan's listing");
    assert_eq!(listing.end, End::Complete);
    assert_eq!(ids(&listing.items), vec![1, 2, 3]);
    assert_eq!(listing.requests, 8);
    assert_eq!(transport.remaining(), 0);
}

#[test]
fn as_read_makes_no_revalidation_and_dedups() {
    let transport = Arc::new(ScriptedTransport::new());
    serve(&transport, &page(1..4), "W/\"p1\"", Some(2));
    serve(&transport, &page(3..6), "W/\"p2\"", None);
    let gh = common::scripted(&transport, API);
    let opts = ListOpts::STANDARD.with_consistency(Consistency::AsRead);
    let listing =
        walk(&gh, releases(), &opts, &LinkScope::Repo(common::slug(REPO))).expect("as read");
    assert_eq!(ids(&listing.items), vec![1, 2, 3, 4, 5]);
    assert_eq!(listing.requests, 2);
    assert_eq!(listing.consistency, Consistency::AsRead);
}

#[test]
fn per_page_over_100_is_usage() {
    let transport = Arc::new(ScriptedTransport::new());
    let gh = common::scripted(&transport, API);
    let opts = ListOpts::STANDARD.with_per_page(101);
    let error =
        walk(&gh, releases(), &opts, &LinkScope::Repo(common::slug(REPO))).expect_err("usage");
    assert_eq!(error.kind, Kind::Usage);
    assert!(error.why.contains("100"));
    assert!(transport.requests().is_empty());
}

#[test]
fn a_named_per_page_over_100_is_usage() {
    let path = "/repos/fixture-dev/repo/releases";
    let route = || RouteRequest::read(RouteTarget::Repo(common::slug(REPO)));
    for first in [
        releases().query("per_page", "500"),
        releases().query("per_page", "many"),
        Call::get(format!("{path}?per_page=500"), route()),
    ] {
        let transport = Arc::new(ScriptedTransport::new());
        let gh = common::scripted(&transport, API);
        let error = walk(
            &gh,
            first,
            &ListOpts::STANDARD,
            &LinkScope::Repo(common::slug(REPO)),
        )
        .expect_err("usage");
        assert_eq!(error.kind, Kind::Usage);
        assert!(error.why.contains("100"));
        assert!(transport.requests().is_empty());
    }
}

#[test]
fn a_path_s_own_query_is_joined_not_doubled() {
    let path = "/repos/fixture-dev/repo/releases";
    for (first, sent) in [
        (format!("{path}?per_page=5"), format!("{path}?per_page=5")),
        (
            format!("{path}?direction=asc"),
            format!("{path}?direction=asc&per_page=100"),
        ),
    ] {
        let transport = Arc::new(ScriptedTransport::new());
        serve(&transport, &page(1..3), "W/\"p1\"", None);
        let gh = common::scripted(&transport, API);
        let call = Call::get(
            first,
            RouteRequest::read(RouteTarget::Repo(common::slug(REPO))),
        );
        let listing = walk(
            &gh,
            call,
            &ListOpts::STANDARD,
            &LinkScope::Repo(common::slug(REPO)),
        )
        .expect("a listing");
        assert_eq!(ids(&listing.items), [1, 2]);
        let urls: Vec<String> = transport.requests().into_iter().map(|r| r.url).collect();
        assert_eq!(urls, [format!("{}{sent}", common::base(API))]);
    }
}

#[test]
fn a_page_that_is_not_an_array_is_protocol() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(200, &[], br#"{"id":1}"#);
    let gh = common::scripted(&transport, API);
    let error = walk(
        &gh,
        releases(),
        &ListOpts::STANDARD,
        &LinkScope::Repo(common::slug(REPO)),
    )
    .expect_err("object");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (Kind::CouldNotRun, Sub::Protocol)
    );
    assert!(error.why.contains("page 1"));
    assert_eq!(error.repo.as_deref(), Some("fixture-dev/repo"));
}

#[test]
fn a_page_that_is_not_json_names_the_repository() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(200, &[], b"not json");
    let gh = common::scripted(&transport, API);
    let error = walk(
        &gh,
        releases(),
        &ListOpts::STANDARD,
        &LinkScope::Repo(common::slug(REPO)),
    )
    .expect_err("not JSON");
    assert_eq!(error.kind, Kind::CouldNotRun);
    assert_eq!(error.repo.as_deref(), Some("fixture-dev/repo"));
}

#[test]
fn a_walk_that_starts_at_page_3_refuses_a_next_link_back_to_page_2() {
    let transport = Arc::new(ScriptedTransport::new());
    serve(&transport, &page(201..250), "\"p3\"", Some(2));
    let gh = common::scripted(&transport, API);
    let error = walk(
        &gh,
        releases().query("page", "3"),
        &ListOpts::STANDARD,
        &LinkScope::Repo(common::slug(REPO)),
    )
    .expect_err("a link backwards");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (Kind::CouldNotRun, Sub::Protocol)
    );
    assert_eq!(error.repo.as_deref(), Some("fixture-dev/repo"));
    assert_eq!(transport.requests().len(), 1);
}

#[test]
fn the_repository_id_is_fetched_once_for_numeric_links() {
    let numeric = |page: u32| {
        format!(
            r#"<{}/repositories/1000/releases?per_page=100&page={page}>; rel="next""#,
            common::base(API)
        )
    };
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(200, &[("link", &numeric(2))], &page(1..3));
    transport.answer(200, &[], br#"{"id":1000,"full_name":"fixture-dev/repo"}"#);
    transport.answer(200, &[("link", &numeric(3))], &page(3..5));
    transport.answer(200, &[], &page(5..7));
    let gh = common::scripted(&transport, API);
    let opts = ListOpts::STANDARD.with_consistency(Consistency::AsRead);
    let listing =
        walk(&gh, releases(), &opts, &LinkScope::Repo(common::slug(REPO))).expect("numeric links");
    assert_eq!(ids(&listing.items), vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(listing.requests, 4);
    let urls: Vec<String> = transport.requests().into_iter().map(|r| r.url).collect();
    assert_eq!(
        urls,
        vec![
            format!(
                "{}/repos/fixture-dev/repo/releases?per_page=100",
                common::base(API)
            ),
            format!("{}/repos/fixture-dev/repo", common::base(API)),
            format!(
                "{}/repositories/1000/releases?per_page=100&page=2",
                common::base(API)
            ),
            format!(
                "{}/repositories/1000/releases?per_page=100&page=3",
                common::base(API)
            ),
        ]
    );
    assert_eq!(
        gh.repo_id(&common::slug(REPO), &releases().route)
            .expect("memo"),
        1000
    );
    assert_eq!(transport.requests().len(), 4);
}

#[test]
fn walk_pages_returns_each_body_whole() {
    let transport = Arc::new(ScriptedTransport::new());
    let next = format!(
        r#"<{}/user/repos?per_page=100&page=2>; rel="next""#,
        common::base(API)
    );
    transport.answer(200, &[("link", &next)], br#"{"total":2,"items":[1]}"#);
    transport.answer(200, &[("etag", "W/\"x\"")], br#"{"total":2,"items":[2]}"#);
    let gh = common::scripted(&transport, API);
    let first = Call::get("/user/repos", RouteRequest::read(RouteTarget::Unscoped));
    let listing =
        walk_pages(&gh, first, &ListOpts::STANDARD, &LinkScope::Exact).expect("two pages");
    assert_eq!(listing.items.len(), 2);
    assert_eq!(
        listing.items[1]
            .get("items")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(listing.consistency, Consistency::AsRead);
    assert_eq!((listing.pages, listing.requests), (2, 2));
}

/// The same walks over `HttpTransport::direct` on agh-fake, whose release listing pages by
/// the numeric `/repositories/{id}` link.
mod loopback {
    use agh_fake::{FakeGithub, Fault, Match, ReleaseSpec, RepoSpec};

    use super::*;

    /// A fake holding the public `fixture-dev/repo` with `count` published releases.
    fn fake_with_releases(count: u64) -> FakeGithub {
        let fake = FakeGithub::start();
        fake.add_repo("fixture-dev/repo", RepoSpec::public());
        for i in 0..count {
            let created = format!(
                "2026-01-01T{:02}:{:02}:{:02}Z",
                i / 3600,
                i / 60 % 60,
                i % 60
            );
            fake.add_release(
                "fixture-dev/repo",
                ReleaseSpec::published(&format!("v{i}"), &created),
            );
        }
        fake
    }

    fn walk_releases(fake: &FakeGithub) -> agh::Result<agh::page::Listing<Value>> {
        let gh = common::github(Router::empty(), fake);
        walk(
            &gh,
            releases(),
            &ListOpts::STANDARD,
            &LinkScope::Repo(common::slug(REPO)),
        )
    }

    #[test]
    fn two_hundred_fifty_releases_are_three_pages_revalidated() {
        let fake = fake_with_releases(250);
        let listing = walk_releases(&fake).expect("the listing");
        assert_eq!(listing.items.len(), 250);
        assert_eq!(listing.pages, 3);
        assert_eq!(listing.end, End::Complete);
        let seen = fake.seen();
        // Three pages, the repository id once for the numeric links, and three 304s.
        assert_eq!(listing.requests, 7);
        assert_eq!(seen.len(), 7);
        let revalidated: Vec<bool> = seen
            .iter()
            .map(|s| {
                s.headers
                    .iter()
                    .any(|(n, _)| n.eq_ignore_ascii_case("if-none-match"))
            })
            .collect();
        assert_eq!(revalidated.iter().filter(|r| **r).count(), 3);
        assert_eq!(fake.violations(), Vec::<String>::new());
    }

    #[test]
    fn one_tear_is_rescanned_and_answered() {
        let fake = fake_with_releases(150);
        fake.fault(Match::api("/repos/fixture-dev/repo/releases"), Fault::Torn);
        let listing = walk_releases(&fake).expect("the rescan is whole");
        assert_eq!(listing.items.len(), 149);
        assert_eq!(listing.end, End::Complete);
        assert_eq!(fake.violations(), Vec::<String>::new());
    }

    #[test]
    fn a_listing_torn_every_time_is_torn() {
        let fake = fake_with_releases(150);
        fake.fault_always(Match::api("/repos/fixture-dev/repo/releases"), Fault::Torn);
        let error = walk_releases(&fake).expect_err("torn");
        assert_eq!(error.kind, Kind::Incomplete);
        assert!(matches!(error.sub, Sub::Torn { .. }), "{:?}", error.sub);
        assert_eq!(fake.violations(), Vec::<String>::new());
    }

    /// A renamed repository's tag listing, which agh-fake pages by `/repos/{o}/{r}/tags`
    /// links of the new name: the walk follows the 301 on page 1 and reads the rest as the
    /// new name's listing, the revalidation included.
    #[test]
    fn a_renamed_repository_s_listing_is_walked_under_its_new_name() {
        let fake = FakeGithub::start();
        fake.add_repo("fixture-dev/repo", RepoSpec::public());
        for i in 0..150 {
            fake.set_tag("fixture-dev/repo", &format!("v{i}"), &format!("{i:040x}"));
        }
        fake.rename_repo("fixture-dev/repo", "fixture-dev/renamed");
        let gh = common::github(Router::empty(), &fake);
        let first = Call::get(
            "/repos/fixture-dev/repo/tags",
            RouteRequest::read(RouteTarget::Repo(common::slug(REPO))),
        );
        let listing = walk(
            &gh,
            first,
            &ListOpts::STANDARD,
            &LinkScope::Repo(common::slug(REPO)),
        )
        .expect("the moved listing");
        assert_eq!(listing.items.len(), 150);
        assert_eq!(listing.pages, 2);
        assert_eq!(listing.end, End::Complete);
        let paths: Vec<String> = fake.seen().into_iter().map(|s| s.path_and_query).collect();
        // Page 1's 301 and the id lookup, then every request under the new name: page 1
        // again, page 2, and the two revalidations.
        assert_eq!(paths.len(), 6, "{paths:?}");
        assert_eq!(listing.requests, 6);
        assert!(
            paths[0].starts_with("/repos/fixture-dev/repo/tags"),
            "{paths:?}"
        );
        assert!(paths[1].starts_with("/repositories/"), "{paths:?}");
        assert!(
            paths[2..]
                .iter()
                .all(|p| p.starts_with("/repos/fixture-dev/renamed/tags")),
            "{paths:?}"
        );
        assert_eq!(fake.violations(), Vec::<String>::new());
    }
}

/// A walk whose first page follows a transfer out of a release-tier owner: the pages after
/// the move are routed for the new owner, so the release token never reads a repository that
/// left the release tier (design §6.5, §7.2). File credentials are unix-only (plan §1 rule 12).
#[cfg(unix)]
#[test]
fn pages_after_a_transfer_are_routed_for_the_new_owner() {
    let root = common::scratch("pages-transfer");
    common::write_token(
        &root.path().join("tokens"),
        "dev.token",
        &agh_fake::token("dev"),
    );
    common::write_token(
        &root.path().join("secrets"),
        "release.token",
        &agh_fake::token("release"),
    );
    let text = common::canonical_config(root.path()).replace(
        "read = [\"dev\", \"anonymous\"]\nwrite = \"release\"",
        "read = [\"release\"]\nwrite = \"release\"",
    );
    assert!(text.contains("read = [\"release\"]"), "{text}");
    let router = common::router(&text, root.path());
    let old = common::slug("alabsystems/foo");
    let new = common::slug("fixture-ext/foo");
    let transport = Arc::new(ScriptedTransport::new());
    let location = format!("{}/repositories/77/releases", common::base(API));
    transport.answer(301, &[("location", &location)], b"{}");
    transport.answer(200, &[], br#"{"id":77,"full_name":"fixture-ext/foo"}"#);
    let numeric = format!(
        r#"<{}/repositories/77/releases?per_page=100&page=2>; rel="next""#,
        common::base(API)
    );
    transport.answer(200, &[("link", &numeric)], &page(1..3));
    transport.answer(200, &[], &page(3..5));
    let gh = common::github_over(router, transport.clone(), common::scripted_endpoints(API));
    let first = Call::get(
        "/repos/alabsystems/foo/releases",
        RouteRequest::read(RouteTarget::Repo(old.clone())),
    );
    let opts = ListOpts::STANDARD.with_consistency(Consistency::AsRead);
    let listing = walk(&gh, first, &opts, &LinkScope::Repo(old)).expect("the moved listing");
    assert_eq!(ids(&listing.items), vec![1, 2, 3, 4]);
    let seen = transport.requests();
    let sent: Vec<(String, Option<String>)> = seen
        .iter()
        .map(|r| (r.url.clone(), r.auth_profile.clone()))
        .collect();
    assert_eq!(
        sent,
        vec![
            (
                format!(
                    "{}/repos/alabsystems/foo/releases?per_page=100",
                    common::base(API)
                ),
                Some("release".to_string())
            ),
            (format!("{}/repositories/77", common::base(API)), None),
            (
                format!(
                    "{}/repos/fixture-ext/foo/releases?per_page=100",
                    common::base(API)
                ),
                Some("dev".to_string())
            ),
            (
                format!(
                    "{}/repositories/77/releases?per_page=100&page=2",
                    common::base(API)
                ),
                Some("dev".to_string())
            ),
        ]
    );
    // The move's lookup memoized the new name's id: the numeric link cost no request.
    assert_eq!(listing.requests, 4);
    assert!(
        matches!(&listing.as_, IdentityRef::Profile { name, .. } if name == "dev"),
        "{:?}",
        listing.as_
    );
    assert_eq!(
        gh.repo_id(&new, &RouteRequest::read(RouteTarget::Unscoped))
            .ok(),
        Some(77)
    );
    assert_eq!(transport.requests().len(), 4);
}
